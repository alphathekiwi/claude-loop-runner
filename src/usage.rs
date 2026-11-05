use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

const USAGE_API_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// 30 minutes before reset, wake up and burn remaining tokens.
const EARLY_WAKE_SECS: i64 = 30 * 60;

#[derive(Debug, Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
}

#[derive(Debug, Deserialize)]
struct UsageWindow {
    utilization: f64,
    resets_at: Option<String>,
}

/// Fetches the OAuth access token from the macOS Keychain.
fn get_oauth_token() -> anyhow::Result<String> {
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to read Claude Code credentials from Keychain: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let raw = String::from_utf8(output.stdout)?.trim().to_string();
    let parsed: serde_json::Value = serde_json::from_str(&raw)?;
    let token = parsed
        .get("claudeAiOauth")
        .and_then(|v| v.get("accessToken"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Could not find accessToken in credentials"))?
        .to_string();

    Ok(token)
}

/// Query the Anthropic usage API and return (five_hour_utilization, seven_day_utilization, soonest_reset).
async fn fetch_usage(limit_percent: f64) -> anyhow::Result<(f64, f64, Option<DateTime<Utc>>)> {
    let token = get_oauth_token()?;

    let client = reqwest::Client::new();
    let resp = client
        .get(USAGE_API_URL)
        .header("Authorization", format!("Bearer {}", token))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("Content-Type", "application/json")
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Usage API returned {}: {}", status, body);
    }

    let usage: UsageResponse = resp.json().await?;

    Ok(summarize_usage(&usage, limit_percent))
}

/// Reduce a usage response to (five_hour_utilization, seven_day_utilization, soonest_reset).
/// Only windows at or over `limit_percent` contribute a reset time, since waking at the
/// reset of a window that isn't limiting us changes nothing.
/// Missing windows count as 0% utilization; unparseable reset times are ignored.
fn summarize_usage(usage: &UsageResponse, limit_percent: f64) -> (f64, f64, Option<DateTime<Utc>>) {
    let five_hr = usage
        .five_hour
        .as_ref()
        .map(|w| w.utilization)
        .unwrap_or(0.0);
    let seven_day = usage
        .seven_day
        .as_ref()
        .map(|w| w.utilization)
        .unwrap_or(0.0);

    // Find the soonest reset time from whichever window is over limit
    let mut soonest_reset: Option<DateTime<Utc>> = None;
    for window in [&usage.five_hour, &usage.seven_day]
        .into_iter()
        .flatten()
        .filter(|w| w.utilization >= limit_percent)
    {
        if let Some(ref reset_str) = window.resets_at
            && let Ok(dt) = DateTime::parse_from_rfc3339(reset_str)
        {
            let dt_utc = dt.with_timezone(&Utc);
            match soonest_reset {
                None => soonest_reset = Some(dt_utc),
                Some(existing) if dt_utc < existing => soonest_reset = Some(dt_utc),
                _ => {}
            }
        }
    }

    (five_hr, seven_day, soonest_reset)
}

/// What the monitor loop should do after one usage reading.
#[derive(Debug, PartialEq)]
enum UsageAction {
    /// Quota fully expended: pause, clear burn mode, then sleep `Some(secs)` if the
    /// reset is in the future, else fall through to the normal check interval.
    SleepUntilReset(Option<u64>),
    /// Over the soft limit with a reset more than `EARLY_WAKE_SECS` away: pause, sleep
    /// this many seconds, then resume in burn-remaining mode.
    PauseUntilEarlyWake(u64),
    /// Over the soft limit and already inside the early-wake window: resume in burn mode now.
    BurnNow,
    /// Over the soft limit with no known reset time: pause and keep checking.
    Pause,
    /// Back under the limit while paused or burning (the window reset): clear burn mode and resume.
    Resume,
    /// Nothing to change.
    NoChange,
}

/// Pure decision logic for one iteration of the usage monitor loop.
fn decide_usage_action(
    five_hr: f64,
    seven_day: f64,
    limit_percent: f64,
    soonest_reset: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    currently_paused: bool,
    burning_remaining: bool,
) -> UsageAction {
    let over_limit = five_hr >= limit_percent || seven_day >= limit_percent;
    let fully_expended = five_hr >= 100.0 || seven_day >= 100.0;

    if fully_expended {
        let sleep_secs = soonest_reset
            .filter(|reset_at| *reset_at > now)
            .map(|reset_at| (reset_at - now).num_seconds().max(0) as u64 + 60); // 60s buffer past reset
        UsageAction::SleepUntilReset(sleep_secs)
    } else if over_limit && !burning_remaining {
        match soonest_reset {
            Some(reset_at) => {
                let wake_at = reset_at - chrono::TimeDelta::seconds(EARLY_WAKE_SECS);
                if wake_at > now {
                    UsageAction::PauseUntilEarlyWake((wake_at - now).num_seconds().max(0) as u64)
                } else {
                    UsageAction::BurnNow
                }
            }
            None => UsageAction::Pause,
        }
    } else if !over_limit && (currently_paused || burning_remaining) {
        UsageAction::Resume
    } else {
        UsageAction::NoChange
    }
}

/// Monitor that checks Claude API usage and pauses workers when utilization exceeds the limit.
pub struct UsageMonitor {
    paused: Arc<AtomicBool>,
    resume_notify: Arc<Notify>,
}

impl UsageMonitor {
    pub fn new() -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
            resume_notify: Arc::new(Notify::new()),
        }
    }

    pub fn handle(&self) -> UsageHandle {
        UsageHandle {
            paused: Arc::clone(&self.paused),
            resume_notify: Arc::clone(&self.resume_notify),
        }
    }

    /// Start the background usage monitoring task.
    ///
    /// When utilization exceeds `limit_percent`:
    /// 1. Pause workers and sleep until 30 minutes before quota reset
    /// 2. Resume workers to use remaining tokens in the final 30-minute window
    /// 3. If usage hits 100% (fully expended), sleep until actual reset + 60s buffer
    /// 4. After reset, resume normally
    pub fn spawn_monitor(
        &self,
        limit_percent: f64,
        check_interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        self.spawn_monitor_with(limit_percent, check_interval, move || {
            fetch_usage(limit_percent)
        })
    }

    /// [`spawn_monitor`](Self::spawn_monitor) with the usage source injected, so the
    /// loop can be driven by scripted readings in tests.
    fn spawn_monitor_with<F, Fut>(
        &self,
        limit_percent: f64,
        check_interval: Duration,
        fetch: F,
    ) -> tokio::task::JoinHandle<()>
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<(f64, f64, Option<DateTime<Utc>>)>> + Send,
    {
        let paused = Arc::clone(&self.paused);
        let resume_notify = Arc::clone(&self.resume_notify);

        tokio::spawn(async move {
            // Track whether we're in the "burning remaining tokens" phase
            let mut burning_remaining = false;

            loop {
                match fetch().await {
                    Ok((five_hr, seven_day, soonest_reset)) => {
                        let currently_paused = paused.load(Ordering::SeqCst);
                        let action = decide_usage_action(
                            five_hr,
                            seven_day,
                            limit_percent,
                            soonest_reset,
                            Utc::now(),
                            currently_paused,
                            burning_remaining,
                        );

                        match action {
                            UsageAction::SleepUntilReset(sleep_secs) => {
                                // Tokens fully used up — pause and sleep until actual reset
                                if !currently_paused {
                                    paused.store(true, Ordering::SeqCst);
                                }
                                burning_remaining = false;

                                warn!(
                                    five_hour = format!("{:.1}%", five_hr),
                                    seven_day = format!("{:.1}%", seven_day),
                                    "Quota fully expended, sleeping until reset"
                                );

                                if let (Some(sleep_secs), Some(reset_at)) =
                                    (sleep_secs, soonest_reset)
                                {
                                    info!(
                                        reset_at = %reset_at.format("%Y-%m-%d %H:%M:%S UTC"),
                                        sleep_secs = sleep_secs,
                                        "Sleeping until quota resets"
                                    );
                                    tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
                                    continue;
                                }
                            }
                            UsageAction::PauseUntilEarlyWake(_)
                            | UsageAction::BurnNow
                            | UsageAction::Pause => {
                                // Over soft limit — pause and sleep until 30min before reset
                                if !currently_paused {
                                    paused.store(true, Ordering::SeqCst);
                                }

                                warn!(
                                    five_hour = format!("{:.1}%", five_hr),
                                    seven_day = format!("{:.1}%", seven_day),
                                    limit = format!("{:.1}%", limit_percent),
                                    "Usage limit exceeded, pausing workers"
                                );

                                if let UsageAction::PauseUntilEarlyWake(sleep_secs) = action {
                                    if let Some(reset_at) = soonest_reset {
                                        let wake_at =
                                            reset_at - chrono::TimeDelta::seconds(EARLY_WAKE_SECS);
                                        info!(
                                            reset_at = %reset_at.format("%H:%M:%S UTC"),
                                            wake_at = %(wake_at).format("%H:%M:%S UTC"),
                                            sleep_secs = sleep_secs,
                                            "Sleeping until 30min before reset to burn remaining tokens"
                                        );
                                    }
                                    tokio::time::sleep(Duration::from_secs(sleep_secs)).await;

                                    // Wake up in burn-remaining mode
                                    burning_remaining = true;
                                    paused.store(false, Ordering::SeqCst);
                                    resume_notify.notify_waiters();
                                    info!("Resuming workers to use remaining tokens before reset");
                                    continue;
                                } else if action == UsageAction::BurnNow {
                                    // Already within the 30-min window — just burn tokens
                                    burning_remaining = true;
                                    paused.store(false, Ordering::SeqCst);
                                    resume_notify.notify_waiters();
                                    info!(
                                        "Within 30min of reset, resuming to burn remaining tokens"
                                    );
                                }
                            }
                            UsageAction::Resume => {
                                // Usage dropped below limit (e.g. after reset)
                                burning_remaining = false;
                                paused.store(false, Ordering::SeqCst);
                                resume_notify.notify_waiters();
                                info!(
                                    five_hour = format!("{:.1}%", five_hr),
                                    seven_day = format!("{:.1}%", seven_day),
                                    "Usage back under limit, resuming workers"
                                );
                            }
                            UsageAction::NoChange => {
                                debug!(
                                    five_hour = format!("{:.1}%", five_hr),
                                    seven_day = format!("{:.1}%", seven_day),
                                    paused = currently_paused,
                                    burning_remaining = burning_remaining,
                                    "Usage check"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to fetch usage (will retry)");
                    }
                }

                tokio::time::sleep(check_interval).await;
            }
        })
    }
}

/// Cheaply cloneable handle for workers to check usage pause state.
#[derive(Clone)]
pub struct UsageHandle {
    paused: Arc<AtomicBool>,
    resume_notify: Arc<Notify>,
}

impl UsageHandle {
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub async fn wait_if_paused(&self) {
        crate::pause::wait_while_paused(&self.resume_notify, || self.is_paused()).await;
    }
}

/// A no-op handle for when --limit is not set.
pub fn noop_handle() -> UsageHandle {
    UsageHandle {
        paused: Arc::new(AtomicBool::new(false)),
        resume_notify: Arc::new(Notify::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const EARLY: i64 = EARLY_WAKE_SECS;

    fn parse(json: &str) -> UsageResponse {
        serde_json::from_str(json).expect("valid usage json")
    }

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn decide(
        five: f64,
        seven: f64,
        reset: Option<DateTime<Utc>>,
        paused: bool,
        burning: bool,
    ) -> UsageAction {
        decide_usage_action(five, seven, 95.0, reset, at(0), paused, burning)
    }

    #[test]
    fn summarize_picks_utilizations_and_soonest_reset() {
        let usage = parse(
            r#"{
                "five_hour": {"utilization": 42.5, "resets_at": "2025-01-01T05:00:00Z"},
                "seven_day": {"utilization": 88.0, "resets_at": "2025-01-01T03:00:00+00:00"}
            }"#,
        );
        let (five, seven, reset) = summarize_usage(&usage, 0.0);
        assert_eq!(five, 42.5);
        assert_eq!(seven, 88.0);
        assert_eq!(
            reset,
            Some(Utc.with_ymd_and_hms(2025, 1, 1, 3, 0, 0).unwrap())
        );
    }

    #[test]
    fn summarize_keeps_earlier_reset_when_later_one_follows() {
        let usage = parse(
            r#"{
                "five_hour": {"utilization": 1.0, "resets_at": "2025-01-01T01:00:00Z"},
                "seven_day": {"utilization": 2.0, "resets_at": "2025-01-07T00:00:00Z"}
            }"#,
        );
        assert_eq!(
            summarize_usage(&usage, 0.0).2,
            Some(Utc.with_ymd_and_hms(2025, 1, 1, 1, 0, 0).unwrap())
        );
    }

    #[test]
    fn summarize_normalises_offsets_to_utc() {
        let usage =
            parse(r#"{"five_hour": {"utilization": 0, "resets_at": "2025-01-01T12:00:00+10:00"}}"#);
        assert_eq!(
            summarize_usage(&usage, 0.0).2,
            Some(Utc.with_ymd_and_hms(2025, 1, 1, 2, 0, 0).unwrap())
        );
    }

    #[test]
    fn summarize_treats_missing_windows_as_zero_and_no_reset() {
        let (five, seven, reset) = summarize_usage(&parse("{}"), 0.0);
        assert_eq!((five, seven, reset), (0.0, 0.0, None));

        let (five, seven, reset) = summarize_usage(
            &parse(r#"{"five_hour": null, "seven_day": {"utilization": 7}}"#),
            0.0,
        );
        assert_eq!((five, seven, reset), (0.0, 7.0, None));
    }

    #[test]
    fn summarize_ignores_unparseable_reset_times() {
        let usage = parse(
            r#"{
                "five_hour": {"utilization": 10, "resets_at": "tomorrow-ish"},
                "seven_day": {"utilization": 20, "resets_at": "2025-01-07T00:00:00Z"}
            }"#,
        );
        assert_eq!(
            summarize_usage(&usage, 0.0).2,
            Some(Utc.with_ymd_and_hms(2025, 1, 7, 0, 0, 0).unwrap())
        );
    }

    #[test]
    fn summarize_only_uses_resets_of_windows_over_the_limit() {
        // Over the limit on the 7-day window only: the 5-hour reset must not wake workers.
        let usage = parse(
            r#"{
                "five_hour": {"utilization": 20, "resets_at": "2025-01-01T01:00:00Z"},
                "seven_day": {"utilization": 97, "resets_at": "2025-01-07T00:00:00Z"}
            }"#,
        );
        assert_eq!(
            summarize_usage(&usage, 95.0).2,
            Some(Utc.with_ymd_and_hms(2025, 1, 7, 0, 0, 0).unwrap())
        );
        // Under the limit everywhere: no reset to wait for.
        let usage =
            parse(r#"{"five_hour": {"utilization": 20, "resets_at": "2025-01-01T01:00:00Z"}}"#);
        assert_eq!(summarize_usage(&usage, 95.0).2, None);
    }

    #[test]
    fn usage_window_requires_utilization() {
        assert!(serde_json::from_str::<UsageResponse>(r#"{"five_hour": {}}"#).is_err());
    }

    #[test]
    fn fully_expended_sleeps_until_reset_plus_buffer() {
        assert_eq!(
            decide(100.0, 0.0, Some(at(3600)), false, false),
            UsageAction::SleepUntilReset(Some(3660))
        );
        // Either window at 100% counts, and it wins over burn mode.
        assert_eq!(
            decide(10.0, 100.0, Some(at(10)), true, true),
            UsageAction::SleepUntilReset(Some(70))
        );
    }

    #[test]
    fn fully_expended_without_future_reset_does_not_sleep() {
        assert_eq!(
            decide(100.0, 0.0, None, false, false),
            UsageAction::SleepUntilReset(None)
        );
        assert_eq!(
            decide(100.0, 0.0, Some(at(0)), false, false),
            UsageAction::SleepUntilReset(None)
        );
        assert_eq!(
            decide(100.0, 0.0, Some(at(-5)), false, false),
            UsageAction::SleepUntilReset(None)
        );
    }

    #[test]
    fn over_soft_limit_sleeps_until_thirty_minutes_before_reset() {
        assert_eq!(
            decide(95.0, 0.0, Some(at(2 * 3600)), false, false),
            UsageAction::PauseUntilEarlyWake(2 * 3600 - EARLY as u64)
        );
        assert_eq!(
            decide(0.0, 99.9, Some(at(EARLY + 1)), false, false),
            UsageAction::PauseUntilEarlyWake(1)
        );
    }

    #[test]
    fn over_soft_limit_inside_early_window_burns_immediately() {
        assert_eq!(
            decide(96.0, 0.0, Some(at(EARLY)), false, false),
            UsageAction::BurnNow
        );
        assert_eq!(
            decide(96.0, 0.0, Some(at(60)), true, false),
            UsageAction::BurnNow
        );
    }

    #[test]
    fn over_soft_limit_without_reset_just_pauses() {
        assert_eq!(decide(96.0, 0.0, None, false, false), UsageAction::Pause);
    }

    #[test]
    fn over_soft_limit_while_burning_changes_nothing() {
        assert_eq!(
            decide(99.0, 0.0, Some(at(60)), false, true),
            UsageAction::NoChange
        );
    }

    #[test]
    fn under_limit_resumes_only_when_paused_or_burning() {
        assert_eq!(decide(94.9, 94.9, None, true, false), UsageAction::Resume);
        assert_eq!(decide(94.9, 94.9, None, true, true), UsageAction::Resume);
        assert_eq!(decide(94.9, 94.9, None, false, true), UsageAction::Resume);
        assert_eq!(
            decide(10.0, 10.0, None, false, false),
            UsageAction::NoChange
        );
    }

    #[test]
    fn zero_limit_treats_everything_as_over_limit() {
        // main.rs only spawns the monitor when --limit > 0, because 0 here would pause forever.
        assert_eq!(
            decide_usage_action(0.0, 0.0, 0.0, None, at(0), false, false),
            UsageAction::Pause
        );
    }

    #[test]
    fn burn_mode_is_cleared_after_reset() {
        // Mirrors the spawn_monitor loop's state updates.
        let mut paused = false;
        let mut burning = false;
        let reset = Some(at(3600));
        for (five, expect_pause) in [(96.0, true), (10.0, false), (96.0, true)] {
            match decide_usage_action(five, 0.0, 95.0, reset, at(0), paused, burning) {
                UsageAction::PauseUntilEarlyWake(_) | UsageAction::BurnNow => {
                    burning = true;
                    paused = false;
                    assert!(expect_pause, "unexpected pause at {five}%");
                }
                UsageAction::Pause => paused = true,
                UsageAction::Resume => {
                    burning = false;
                    paused = false;
                }
                UsageAction::SleepUntilReset(_) => {
                    burning = false;
                    paused = true;
                }
                UsageAction::NoChange => {
                    assert!(!expect_pause, "soft limit breach at {five}% ignored");
                }
            }
        }
    }

    #[tokio::test]
    async fn handles_share_pause_state_with_monitor() {
        let monitor = UsageMonitor::new();
        let handle = monitor.handle();
        let clone = handle.clone();
        assert!(!handle.is_paused());

        monitor.paused.store(true, Ordering::SeqCst);
        assert!(handle.is_paused());
        assert!(clone.is_paused());
    }

    #[tokio::test]
    async fn wait_if_paused_blocks_until_resumed() {
        let monitor = UsageMonitor::new();
        let handle = monitor.handle();
        monitor.paused.store(true, Ordering::SeqCst);

        let waiter = tokio::spawn({
            let handle = handle.clone();
            async move { handle.wait_if_paused().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());

        monitor.paused.store(false, Ordering::SeqCst);
        monitor.resume_notify.notify_waiters();
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("waiter should wake after resume")
            .unwrap();
    }

    #[tokio::test]
    async fn noop_handle_never_pauses() {
        let handle = noop_handle();
        assert!(!handle.is_paused());
        handle.wait_if_paused().await;
    }

    /// One scripted usage reading: utilizations and the reset time, in seconds from
    /// "now" (wall clock at fetch time). `None` makes the fetch fail.
    type Reading = Option<(f64, f64, Option<i64>)>;

    /// Spawn the real monitor loop over scripted readings; once they run out every
    /// fetch fails, which the loop treats as "no change".
    fn monitor_with(readings: Vec<Reading>) -> (UsageMonitor, UsageHandle) {
        let monitor = UsageMonitor::new();
        let handle = monitor.handle();
        let queue = Arc::new(std::sync::Mutex::new(
            readings
                .into_iter()
                .collect::<std::collections::VecDeque<_>>(),
        ));
        monitor.spawn_monitor_with(95.0, Duration::from_secs(60), move || {
            let next = queue.lock().unwrap().pop_front().flatten();
            async move {
                let (five, seven, reset) =
                    next.ok_or_else(|| anyhow::anyhow!("scripted fetch failure"))?;
                let reset = reset.map(|secs| Utc::now() + chrono::TimeDelta::seconds(secs));
                Ok((five, seven, reset))
            }
        });
        (monitor, handle)
    }

    async fn after(secs: u64) {
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn monitor_pauses_wakes_early_to_burn_then_pauses_again_after_reset() {
        let (_monitor, handle) = monitor_with(vec![
            Some((96.0, 0.0, Some(2 * 3600))), // over soft limit, reset in 2h
            Some((10.0, 0.0, None)),           // window has reset
            Some((96.0, 0.0, Some(2 * 3600))), // over again in the new window
        ]);

        after(1).await;
        assert!(handle.is_paused(), "should pause over the soft limit");

        // Sleeps until 30 minutes before the reset, then resumes to burn what's left.
        let waiter = tokio::spawn({
            let handle = handle.clone();
            async move { handle.wait_if_paused().await }
        });
        after(2 * 3600 - 30 * 60).await;
        assert!(
            waiter.is_finished(),
            "workers should wake for the burn window"
        );
        assert!(!handle.is_paused());

        // The next reading is under the limit; the one after breaches again and must pause.
        after(60 + 1).await;
        assert!(handle.is_paused(), "breach after a reset was ignored");
    }

    #[tokio::test(start_paused = true)]
    async fn monitor_burns_immediately_inside_the_early_wake_window() {
        let (_monitor, handle) = monitor_with(vec![
            Some((96.0, 0.0, Some(10 * 60))), // reset in 10 minutes
            Some((99.0, 0.0, Some(9 * 60))),  // still over while burning: no change
        ]);
        after(1).await;
        assert!(!handle.is_paused());
        after(60).await;
        assert!(!handle.is_paused());
    }

    #[tokio::test(start_paused = true)]
    async fn monitor_sleeps_through_a_fully_expended_window() {
        let (_monitor, handle) = monitor_with(vec![
            Some((100.0, 50.0, Some(3600))), // used up, resets in 1h
            Some((0.0, 50.0, None)),         // after the reset
        ]);
        after(1).await;
        assert!(handle.is_paused());
        // Still asleep just before reset + 60s buffer...
        after(3600 + 60 - 2).await;
        assert!(handle.is_paused());
        // ...and resumed once the post-reset reading comes in.
        after(2).await;
        assert!(!handle.is_paused());
    }

    #[tokio::test(start_paused = true)]
    async fn monitor_rides_out_fetch_errors_and_pauses_without_a_reset_time() {
        let (_monitor, handle) = monitor_with(vec![
            None,                    // fetch error: nothing changes
            Some((97.0, 0.0, None)), // over with no reset time: plain pause
            Some((97.0, 0.0, None)), // still over: stays paused
            Some((50.0, 0.0, None)), // back under: resume
        ]);
        after(1).await;
        assert!(!handle.is_paused());
        after(60).await;
        assert!(handle.is_paused());
        after(60).await;
        assert!(handle.is_paused());
        after(60).await;
        assert!(!handle.is_paused());
    }
}
