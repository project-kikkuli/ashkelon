use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Counts response-forwarding tasks that `pipeline::handle` spawns off the request future
/// (streaming bodies are relayed to the client and logged in a detached `tokio::spawn`, so the
/// client can finish reading — and a short-lived harness can exit — before that task has
/// finished writing the call's telemetry). `ashkelon run` exits the whole process the instant
/// the harness process exits; without draining tracked tasks first, the last call of a run can
/// have its response delivered correctly to the harness while its `CallRecord` never gets
/// written, because `std::process::exit` tears down every task unconditionally.
#[derive(Default)]
pub struct Tracker {
    active: AtomicUsize,
}

/// Held by a spawned response-forwarding task for its lifetime; decrements the tracker's count
/// on drop, including on an early return or panic inside the task.
pub struct Guard(Arc<Tracker>);

impl Tracker {
    pub fn track(self: &Arc<Self>) -> Guard {
        self.active.fetch_add(1, Ordering::SeqCst);
        Guard(self.clone())
    }

    /// Polls until every tracked task has finished, or `timeout` elapses (whichever first).
    /// Never errors: a caller that hits the timeout proceeds exactly as it would have before
    /// this existed, just with a bounded chance to avoid it.
    pub async fn wait_idle(&self, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.active.load(Ordering::SeqCst) > 0 {
            if tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_idle_returns_immediately_when_nothing_is_tracked() {
        let tracker = Tracker::default();
        tracker.wait_idle(Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn wait_idle_returns_once_the_guard_drops() {
        let tracker = Arc::new(Tracker::default());
        let guard = tracker.track();
        let tracker_bg = tracker.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            drop(guard);
        });
        let started = tokio::time::Instant::now();
        tracker_bg.wait_idle(Duration::from_secs(5)).await;
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn wait_idle_gives_up_at_the_timeout_when_never_released() {
        let tracker = Arc::new(Tracker::default());
        let _guard = tracker.track();
        let started = tokio::time::Instant::now();
        tracker.wait_idle(Duration::from_millis(50)).await;
        assert!(started.elapsed() >= Duration::from_millis(50));
    }
}
