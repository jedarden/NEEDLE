//! Time boundary for worker policy.
//!
//! Production code uses Tokio's clock. Tests can inject [`ManualClock`] and
//! move policy time forward explicitly, so deadline and backoff behaviour does
//! not consume wall-clock time.

use async_trait::async_trait;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::Notify;

/// Monotonic and sleeping operations needed by worker policy.
#[async_trait]
pub trait Clock: Send + Sync + fmt::Debug {
    /// Return the current monotonic instant.
    fn now(&self) -> Instant;

    /// Return wall-clock time for policies that need a persisted timestamp.
    fn system_time(&self) -> SystemTime;

    /// Wait until `duration` has elapsed on this clock.
    async fn sleep(&self, duration: Duration);

    /// Measure elapsed time without consulting the process-global clock.
    fn elapsed_since(&self, start: Instant) -> Duration {
        self.now().saturating_duration_since(start)
    }
}

/// The production clock, backed by Tokio's monotonic timer.
#[derive(Debug, Default)]
pub struct TokioClock;

#[async_trait]
impl Clock for TokioClock {
    fn now(&self) -> Instant {
        tokio::time::Instant::now().into_std()
    }

    fn system_time(&self) -> SystemTime {
        SystemTime::now()
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// A manually advanced clock for deterministic policy tests.
///
/// Sleepers remain pending until [`advance`](Self::advance) reaches their
/// deadline. Advancing wakes every sleeper; each re-checks its own deadline.
#[derive(Debug, Clone)]
pub struct ManualClock {
    inner: Arc<ManualClockInner>,
}

#[derive(Debug)]
struct ManualClockInner {
    monotonic_origin: Instant,
    system_origin: SystemTime,
    elapsed: Mutex<Duration>,
    changed: Notify,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ManualClockInner {
                monotonic_origin: Instant::now(),
                system_origin: SystemTime::now(),
                elapsed: Mutex::new(Duration::ZERO),
                changed: Notify::new(),
            }),
        }
    }

    /// Move time forward and wake tasks waiting on this clock.
    pub fn advance(&self, duration: Duration) {
        let mut elapsed = self
            .inner
            .elapsed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *elapsed = elapsed.saturating_add(duration);
        drop(elapsed);
        self.inner.changed.notify_waiters();
    }

    fn elapsed(&self) -> Duration {
        *self
            .inner
            .elapsed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.inner.monotonic_origin + self.elapsed()
    }

    fn system_time(&self) -> SystemTime {
        self.inner.system_origin + self.elapsed()
    }

    async fn sleep(&self, duration: Duration) {
        let deadline = self.now() + duration;
        loop {
            // Register before checking so an advance cannot be lost between
            // the deadline check and awaiting the notification.
            let changed = self.inner.changed.notified();
            if self.now() >= deadline {
                return;
            }
            changed.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_advances_monotonic_and_system_time_together() {
        let clock = ManualClock::new();
        let monotonic_start = clock.now();
        let system_start = clock.system_time();

        clock.advance(Duration::from_secs(90));

        assert_eq!(
            clock.elapsed_since(monotonic_start),
            Duration::from_secs(90)
        );
        assert_eq!(
            clock.system_time().duration_since(system_start).unwrap(),
            Duration::from_secs(90)
        );
    }

    #[tokio::test]
    async fn manual_sleep_completes_only_after_deadline_is_advanced() {
        let clock = ManualClock::new();
        let sleeping_clock = clock.clone();
        let sleeper = tokio::spawn(async move {
            sleeping_clock.sleep(Duration::from_secs(30)).await;
        });
        tokio::task::yield_now().await;

        clock.advance(Duration::from_secs(29));
        tokio::task::yield_now().await;
        assert!(!sleeper.is_finished());

        clock.advance(Duration::from_secs(1));
        sleeper.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn production_clock_obeys_tokio_virtual_time() {
        let clock = TokioClock;
        let start = clock.now();

        clock.sleep(Duration::from_secs(60)).await;

        assert_eq!(clock.elapsed_since(start), Duration::from_secs(60));
    }
}
