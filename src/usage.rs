use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

const USAGE_API_URL: &str = "https://api.anthropic.com/api/oauth/usage";

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
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
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
async fn fetch_usage() -> anyhow::Result<(f64, f64, Option<DateTime<Utc>>)> {
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

    let five_hr = usage.five_hour.as_ref().map(|w| w.utilization).unwrap_or(0.0);
    let seven_day = usage.seven_day.as_ref().map(|w| w.utilization).unwrap_or(0.0);

    // Find the soonest reset time from whichever window is over limit
    let mut soonest_reset: Option<DateTime<Utc>> = None;
    for window in [&usage.five_hour, &usage.seven_day].into_iter().flatten() {
        if let Some(ref reset_str) = window.resets_at {
            if let Ok(dt) = DateTime::parse_from_rfc3339(reset_str) {
                let dt_utc = dt.with_timezone(&Utc);
                match soonest_reset {
                    None => soonest_reset = Some(dt_utc),
                    Some(existing) if dt_utc < existing => soonest_reset = Some(dt_utc),
                    _ => {}
                }
            }
        }
    }

    Ok((five_hr, seven_day, soonest_reset))
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
        let paused = Arc::clone(&self.paused);
        let resume_notify = Arc::clone(&self.resume_notify);

        // 30 minutes before reset, wake up and burn remaining tokens
        let early_wake_secs: i64 = 30 * 60;

        tokio::spawn(async move {
            // Track whether we're in the "burning remaining tokens" phase
            let mut burning_remaining = false;

            loop {
                match fetch_usage().await {
                    Ok((five_hr, seven_day, soonest_reset)) => {
                        let currently_paused = paused.load(Ordering::SeqCst);
                        let over_limit = five_hr >= limit_percent || seven_day >= limit_percent;
                        let fully_expended = five_hr >= 100.0 || seven_day >= 100.0;

                        if fully_expended {
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

                            if let Some(reset_at) = soonest_reset {
                                let now = Utc::now();
                                if reset_at > now {
                                    let sleep_secs = (reset_at - now)
                                        .num_seconds()
                                        .max(0) as u64
                                        + 60; // 60s buffer past reset
                                    info!(
                                        reset_at = %reset_at.format("%Y-%m-%d %H:%M:%S UTC"),
                                        sleep_secs = sleep_secs,
                                        "Sleeping until quota resets"
                                    );
                                    tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
                                    continue;
                                }
                            }
                        } else if over_limit && !burning_remaining {
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

                            if let Some(reset_at) = soonest_reset {
                                let now = Utc::now();
                                let wake_at = reset_at
                                    - chrono::TimeDelta::seconds(early_wake_secs);

                                if wake_at > now {
                                    let sleep_secs =
                                        (wake_at - now).num_seconds().max(0) as u64;
                                    info!(
                                        reset_at = %reset_at.format("%H:%M:%S UTC"),
                                        wake_at = %(wake_at).format("%H:%M:%S UTC"),
                                        sleep_secs = sleep_secs,
                                        "Sleeping until 30min before reset to burn remaining tokens"
                                    );
                                    tokio::time::sleep(Duration::from_secs(sleep_secs)).await;

                                    // Wake up in burn-remaining mode
                                    burning_remaining = true;
                                    paused.store(false, Ordering::SeqCst);
                                    resume_notify.notify_waiters();
                                    info!("Resuming workers to use remaining tokens before reset");
                                    continue;
                                } else {
                                    // Already within the 30-min window — just burn tokens
                                    burning_remaining = true;
                                    paused.store(false, Ordering::SeqCst);
                                    resume_notify.notify_waiters();
                                    info!("Within 30min of reset, resuming to burn remaining tokens");
                                }
                            }
                        } else if !over_limit && currently_paused {
                            // Usage dropped below limit (e.g. after reset)
                            burning_remaining = false;
                            paused.store(false, Ordering::SeqCst);
                            resume_notify.notify_waiters();
                            info!(
                                five_hour = format!("{:.1}%", five_hr),
                                seven_day = format!("{:.1}%", seven_day),
                                "Usage back under limit, resuming workers"
                            );
                        } else {
                            debug!(
                                five_hour = format!("{:.1}%", five_hr),
                                seven_day = format!("{:.1}%", seven_day),
                                paused = currently_paused,
                                burning_remaining = burning_remaining,
                                "Usage check"
                            );
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
        while self.is_paused() {
            self.resume_notify.notified().await;
        }
    }
}

/// A no-op handle for when --limit is not set.
pub fn noop_handle() -> UsageHandle {
    UsageHandle {
        paused: Arc::new(AtomicBool::new(false)),
        resume_notify: Arc::new(Notify::new()),
    }
}
