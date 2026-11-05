use tokio::sync::Notify;

/// Wait until `is_paused` returns false, woken by `notify.notify_waiters()`.
///
/// The waiter registers with `notify` *before* checking the flag. `notify_waiters`
/// only wakes waiters that are already registered, so checking first would lose a
/// resume that lands between the check and the await, leaving the worker asleep.
pub async fn wait_while_paused(notify: &Notify, is_paused: impl Fn() -> bool) {
    loop {
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !is_paused() {
            return;
        }
        notified.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration;

    #[tokio::test]
    async fn returns_immediately_when_not_paused() {
        let notify = Notify::new();
        tokio::time::timeout(Duration::from_secs(1), wait_while_paused(&notify, || false))
            .await
            .expect("should not wait");
    }

    #[tokio::test]
    async fn resume_between_flag_check_and_await_is_not_lost() {
        // The first flag read sees "paused", and the resume (flag cleared + notify)
        // happens straight after it, before the waiter reaches its await.
        let notify = Notify::new();
        let checks = Cell::new(0);
        let is_paused = || {
            checks.set(checks.get() + 1);
            if checks.get() == 1 {
                notify.notify_waiters();
                return true;
            }
            false
        };

        tokio::time::timeout(
            Duration::from_secs(1),
            wait_while_paused(&notify, is_paused),
        )
        .await
        .expect("resume notification was lost");
        assert_eq!(checks.get(), 2);
    }
}
