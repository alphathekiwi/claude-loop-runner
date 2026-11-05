use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use sysinfo::System;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Memory monitor that tracks system memory usage and signals workers to pause
/// when memory pressure is high.
pub struct MemoryMonitor {
    /// Signal that workers should pause
    paused: Arc<AtomicBool>,
    /// Notify waiters when unpaused
    resume_notify: Arc<Notify>,
}

impl MemoryMonitor {
    pub fn new() -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
            resume_notify: Arc::new(Notify::new()),
        }
    }

    /// Get a handle that workers can use to check pause state
    pub fn handle(&self) -> MemoryHandle {
        MemoryHandle {
            paused: Arc::clone(&self.paused),
            resume_notify: Arc::clone(&self.resume_notify),
        }
    }

    /// Start the background monitoring task
    ///
    /// # Arguments
    /// * `high_threshold` - Memory usage percentage to trigger pause (e.g., 85.0)
    /// * `low_threshold` - Memory usage percentage to resume workers (e.g., 70.0)
    /// * `check_interval` - How often to check memory usage
    ///
    /// Returns a JoinHandle for the monitoring task
    pub fn spawn_monitor(
        &self,
        high_threshold: f64,
        low_threshold: f64,
        check_interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let paused = Arc::clone(&self.paused);
        let resume_notify = Arc::clone(&self.resume_notify);

        tokio::spawn(async move {
            let mut sys = match System::new() {
                Ok(sys) => sys,
                Err(e) => {
                    warn!(error = %e, "Memory monitoring unavailable; workers won't pause on memory pressure");
                    return;
                }
            };

            loop {
                sys.refresh_memory();

                let total = sys.total_memory() as f64;
                let used = sys.used_memory() as f64;
                let percent = (used / total) * 100.0;

                let currently_paused = paused.load(Ordering::SeqCst);

                if !currently_paused && percent > high_threshold {
                    // Memory pressure - pause workers
                    paused.store(true, Ordering::SeqCst);
                    warn!(
                        memory_percent = format!("{:.1}", percent),
                        threshold = high_threshold,
                        "Memory pressure detected, pausing workers"
                    );
                } else if currently_paused && percent < low_threshold {
                    // Memory recovered - resume workers
                    paused.store(false, Ordering::SeqCst);
                    resume_notify.notify_waiters();
                    info!(
                        memory_percent = format!("{:.1}", percent),
                        threshold = low_threshold,
                        "Memory recovered, resuming workers"
                    );
                } else {
                    debug!(
                        memory_percent = format!("{:.1}", percent),
                        paused = currently_paused,
                        "Memory check"
                    );
                }

                tokio::time::sleep(check_interval).await;
            }
        })
    }
}

impl Default for MemoryMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Cheaply cloneable handle for workers to check memory pressure state
#[derive(Clone)]
pub struct MemoryHandle {
    paused: Arc<AtomicBool>,
    resume_notify: Arc<Notify>,
}

impl MemoryHandle {
    /// Check if workers should pause due to memory pressure
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Wait until not paused (returns immediately if not paused)
    pub async fn wait_if_paused(&self) {
        crate::pause::wait_while_paused(&self.resume_notify, || self.is_paused()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_memory_handle_clone() {
        let monitor = MemoryMonitor::new();
        let handle1 = monitor.handle();
        let handle2 = handle1.clone();

        // Both handles should see the same state
        assert!(!handle1.is_paused());
        assert!(!handle2.is_paused());
    }

    #[tokio::test]
    async fn test_wait_if_paused_returns_immediately_when_not_paused() {
        let monitor = MemoryMonitor::new();
        let handle = monitor.handle();

        // Should return immediately since not paused
        handle.wait_if_paused().await;
    }

    // The monitor reads real system memory, which is always strictly between 0% and 100%,
    // so thresholds outside that range force each branch deterministically.

    async fn wait_for(handle: &MemoryHandle, paused: bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while handle.is_paused() != paused {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("handle never reached paused={paused}"));
    }

    #[tokio::test]
    async fn monitor_pauses_when_usage_exceeds_high_threshold() {
        let monitor = MemoryMonitor::default();
        let handle = monitor.handle();
        // low < 0 means it can never recover once paused.
        let task = monitor.spawn_monitor(-1.0, -1.0, Duration::from_millis(5));
        wait_for(&handle, true).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(handle.is_paused(), "should stay paused above low threshold");
        task.abort();
    }

    #[tokio::test]
    async fn monitor_does_not_pause_below_high_threshold() {
        let monitor = MemoryMonitor::new();
        let handle = monitor.handle();
        let task = monitor.spawn_monitor(101.0, 50.0, Duration::from_millis(5));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!handle.is_paused());
        task.abort();
    }

    #[tokio::test]
    async fn monitor_resumes_and_wakes_waiters_below_low_threshold() {
        let monitor = MemoryMonitor::new();
        let handle = monitor.handle();
        monitor.paused.store(true, Ordering::SeqCst);

        let waiter = tokio::spawn({
            let handle = handle.clone();
            async move { handle.wait_if_paused().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished(), "waiter should block while paused");

        // high > 100 so it never re-pauses; low > 100 so it resumes on the first check.
        let task = monitor.spawn_monitor(101.0, 101.0, Duration::from_millis(5));
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("waiter should be woken on resume")
            .unwrap();
        assert!(!handle.is_paused());
        task.abort();
    }

    #[tokio::test]
    async fn monitor_holds_pause_between_thresholds() {
        let monitor = MemoryMonitor::new();
        let handle = monitor.handle();
        monitor.paused.store(true, Ordering::SeqCst);
        // Hysteresis: already paused, usage is above low (-1) so no resume, and the
        // high threshold (101) is irrelevant while paused.
        let task = monitor.spawn_monitor(101.0, -1.0, Duration::from_millis(5));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(handle.is_paused());
        task.abort();
    }
}
