//! Test utilities for the telemetry module.

#[cfg(any(test, feature = "integration"))]
use crate::telemetry::{Result, Sink, Telemetry, TelemetryEvent};
#[cfg(any(test, feature = "integration"))]
use std::sync::{Arc, Mutex};

/// In-memory sink for testing — collects events via a shared Vec.
#[cfg(any(test, feature = "integration"))]
pub struct MemorySink {
    events: Arc<Mutex<Vec<TelemetryEvent>>>,
}

#[cfg(any(test, feature = "integration"))]
impl MemorySink {
    pub fn new() -> (Self, Arc<Mutex<Vec<TelemetryEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            MemorySink {
                events: events.clone(),
            },
            events,
        )
    }
}

#[cfg(any(test, feature = "integration"))]
impl Sink for MemorySink {
    fn accept(&self, event: &TelemetryEvent) -> Result<()> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }

    fn flush(&self, _deadline: std::time::Duration) -> Result<()> {
        Ok(())
    }
}

/// Test helper for capturing and inspecting telemetry events.
///
/// Wraps a `Telemetry` emitter with a `MemorySink` to collect events
/// during tests. Provides convenience methods for querying and asserting
/// on captured events.
///
/// # Example
///
/// ```no_run
/// use needle::telemetry::test_utils::TestHelper;
///
/// let helper = TestHelper::new("test-worker");
///
/// // Emit events through the helper's telemetry emitter
/// helper.telemetry().emit(EventKind::WorkerStarted { ... }).unwrap();
///
/// // Query captured events
/// let started_events = helper.events_by_type("worker.started");
/// assert_eq!(started_events.len(), 1);
///
/// // Or use helper methods for assertions
/// helper.assert_event_emitted("worker.started");
/// helper.assert_event_count("worker.started", 1);
/// ```
#[cfg(any(test, feature = "integration"))]
pub struct TestHelper {
    /// The telemetry emitter configured with a memory sink.
    telemetry: Telemetry,
    /// Shared reference to the captured events.
    events: Arc<Mutex<Vec<TelemetryEvent>>>,
}

#[cfg(any(test, feature = "integration"))]
impl TestHelper {
    /// Create a new test helper with a memory-backed telemetry emitter.
    ///
    /// Returns a helper that collects all emitted telemetry events in memory,
    /// allowing tests to query and assert on them.
    ///
    /// # Arguments
    ///
    /// * `worker_id` - The worker ID to use for the telemetry emitter
    ///
    /// # Example
    ///
    /// ```no_run
    /// let helper = TestHelper::new("test-worker");
    /// ```
    pub fn new(worker_id: impl Into<String>) -> Self {
        let worker_id = worker_id.into();
        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink(worker_id, sink);
        Self { telemetry, events }
    }

    /// Get a reference to the telemetry emitter for emitting events.
    ///
    /// # Example
    ///
    /// ```no_run
    /// helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
    /// ```
    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    /// Get all captured events as a vector.
    ///
    /// Returns a copy of all events collected so far. This acquires a lock
    /// on the internal events vector, so it will block if another thread
    /// is currently adding events.
    pub fn all_events(&self) -> Vec<TelemetryEvent> {
        self.events.lock().unwrap().clone()
    }

    /// Get events filtered by event type.
    ///
    /// Returns all events whose `event_type` matches the given pattern.
    /// The pattern is an exact string match (not a glob pattern).
    ///
    /// # Arguments
    ///
    /// * `event_type` - The exact event type to filter by (e.g., "worker.started")
    ///
    /// # Example
    ///
    /// ```no_run
    /// let started = helper.events_by_type("worker.started");
    /// assert_eq!(started.len(), 1);
    /// ```
    pub fn events_by_type(&self, event_type: &str) -> Vec<TelemetryEvent> {
        self.all_events()
            .into_iter()
            .filter(|e| e.event_type == event_type)
            .collect()
    }

    /// Get events filtered by bead ID.
    ///
    /// Returns all events that have a `bead_id` field matching the given ID.
    ///
    /// # Arguments
    ///
    /// * `bead_id` - The bead ID to filter by
    ///
    /// # Example
    ///
    /// ```no_run
    /// let bead_events = helper.events_by_bead_id("needle-abc123");
    /// ```
    pub fn events_by_bead_id(&self, bead_id: &str) -> Vec<TelemetryEvent> {
        self.all_events()
            .into_iter()
            .filter(|e| {
                e.bead_id
                    .as_ref()
                    .map(|b| b.as_ref() == bead_id)
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Assert that at least one event of the given type was emitted.
    ///
    /// # Arguments
    ///
    /// * `event_type` - The event type to check for
    ///
    /// # Panics
    ///
    /// Panics if no events of the given type were captured.
    ///
    /// # Example
    ///
    /// ```no_run
    /// helper.assert_event_emitted("worker.started");
    /// ```
    pub fn assert_event_emitted(&self, event_type: &str) {
        let events = self.events_by_type(event_type);
        if events.is_empty() {
            panic!(
                "Expected event '{}' to be emitted, but no such events were captured",
                event_type
            );
        }
    }

    /// Assert that NO events of the given type were emitted.
    ///
    /// # Arguments
    ///
    /// * `event_type` - The event type to check for absence
    ///
    /// # Panics
    ///
    /// Panics if any events of the given type were captured.
    ///
    /// # Example
    ///
    /// ```no_run
    /// helper.assert_event_not_emitted("worker.errored");
    /// ```
    pub fn assert_event_not_emitted(&self, event_type: &str) {
        let events = self.events_by_type(event_type);
        if !events.is_empty() {
            panic!(
                "Expected event '{}' NOT to be emitted, but {} events were captured",
                event_type,
                events.len()
            );
        }
    }

    /// Assert that exactly N events of the given type were emitted.
    ///
    /// # Arguments
    ///
    /// * `event_type` - The event type to check
    /// * `count` - The expected number of events
    ///
    /// # Panics
    ///
    /// Panics if the count doesn't match.
    ///
    /// # Example
    ///
    /// ```no_run
    /// helper.assert_event_count("worker.started", 1);
    /// helper.assert_event_count("bead.claim.attempted", 5);
    /// ```
    pub fn assert_event_count(&self, event_type: &str, count: usize) {
        let actual = self.events_by_type(event_type).len();
        if actual != count {
            panic!(
                "Expected {} events of type '{}', but found {}",
                count, event_type, actual
            );
        }
    }

    /// Find the first event of the given type and return a reference.
    ///
    /// Returns `None` if no events of the type exist.
    ///
    /// # Arguments
    ///
    /// * `event_type` - The event type to find
    ///
    /// # Example
    ///
    /// ```no_run
    /// if let Some(event) = helper.find_event("worker.started") {
    ///     assert_eq!(event.worker_id, "test-worker");
    /// }
    /// ```
    pub fn find_event(&self, event_type: &str) -> Option<TelemetryEvent> {
        self.events_by_type(event_type).into_iter().next()
    }

    /// Get the last event of the given type, if any.
    ///
    /// Returns `None` if no events of the type exist.
    ///
    /// # Arguments
    ///
    /// * `event_type` - The event type to find
    ///
    /// # Example
    ///
    /// ```no_run
    /// if let Some(event) = helper.last_event("worker.started") {
    ///     // Inspect the most recent worker.started event
    /// }
    /// ```
    pub fn last_event(&self, event_type: &str) -> Option<TelemetryEvent> {
        self.events_by_type(event_type).into_iter().last()
    }

    /// Clear all captured events.
    ///
    /// Useful for testing specific sections of code where you only want
    /// to capture events emitted after a certain point.
    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }

    /// Get the total count of captured events.
    pub fn event_count(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    /// Allow time for events to be delivered to the sink.
    ///
    /// The telemetry system uses an async channel, so events may not be
    /// immediately available after calling `emit()`. This method sleeps
    /// briefly to allow the background task to process pending events.
    ///
    /// # Example
    ///
    /// ```no_run
    /// helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
    /// helper.sync().await;
    /// assert_eq!(helper.event_count(), 1);
    /// ```
    pub async fn sync(&self) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::EventKind;
    use crate::types::BeadId;

    #[tokio::test]
    async fn test_helper_captures_events() {
        let helper = TestHelper::new("test-worker");

        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
        helper.sync().await;

        assert_eq!(helper.event_count(), 1);
        let events = helper.events_by_type("worker.queue_empty");
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn test_helper_filters_by_bead_id() {
        let helper = TestHelper::new("test-worker");

        helper
            .telemetry()
            .emit(EventKind::ClaimAttempt {
                bead_id: BeadId::from("needle-abc"),
                attempt: 1,
            })
            .unwrap();

        helper
            .telemetry()
            .emit(EventKind::ClaimAttempt {
                bead_id: BeadId::from("needle-def"),
                attempt: 1,
            })
            .unwrap();
        helper.sync().await;

        let abc_events = helper.events_by_bead_id("needle-abc");
        assert_eq!(abc_events.len(), 1);
        assert_eq!(
            abc_events[0].bead_id.as_ref().unwrap().as_ref(),
            "needle-abc"
        );
    }

    #[tokio::test]
    async fn assert_event_emitted_works() {
        let helper = TestHelper::new("test-worker");

        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
        helper.sync().await;

        helper.assert_event_emitted("worker.queue_empty");
    }

    #[tokio::test]
    #[should_panic(expected = "Expected event 'worker.started' to be emitted")]
    async fn assert_event_emitted_panics_when_missing() {
        let helper = TestHelper::new("test-worker");
        helper.assert_event_emitted("worker.started");
    }

    #[tokio::test]
    async fn assert_event_not_emitted_works() {
        let helper = TestHelper::new("test-worker");

        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();

        helper.assert_event_not_emitted("worker.started");
    }

    #[tokio::test]
    #[should_panic(expected = "Expected event 'worker.queue_empty' NOT to be emitted")]
    async fn assert_event_not_emitted_panics_when_present() {
        let helper = TestHelper::new("test-worker");

        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
        helper.sync().await;

        helper.assert_event_not_emitted("worker.queue_empty");
    }

    #[tokio::test]
    async fn assert_event_count_works() {
        let helper = TestHelper::new("test-worker");

        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
        helper.sync().await;

        helper.assert_event_count("worker.queue_empty", 2);
    }

    #[tokio::test]
    #[should_panic(expected = "Expected 5 events of type")]
    async fn assert_event_count_panics_on_mismatch() {
        let helper = TestHelper::new("test-worker");

        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();

        helper.assert_event_count("worker.queue_empty", 5);
    }

    #[tokio::test]
    async fn find_event_works() {
        let helper = TestHelper::new("test-worker");

        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
        helper.sync().await;

        let found = helper.find_event("worker.queue_empty");
        assert!(found.is_some());
        assert_eq!(found.unwrap().event_type, "worker.queue_empty");
    }

    #[tokio::test]
    async fn find_event_returns_none_when_missing() {
        let helper = TestHelper::new("test-worker");
        let found = helper.find_event("worker.started");
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn last_event_works() {
        let helper = TestHelper::new("test-worker");

        helper
            .telemetry()
            .emit(EventKind::ClaimAttempt {
                bead_id: BeadId::from("needle-abc"),
                attempt: 1,
            })
            .unwrap();

        helper
            .telemetry()
            .emit(EventKind::ClaimAttempt {
                bead_id: BeadId::from("needle-def"),
                attempt: 2,
            })
            .unwrap();
        helper.sync().await;

        let last = helper.last_event("bead.claim.attempted");
        assert!(last.is_some());
        // Last event should be the second ClaimAttempt (attempt 2)
        assert_eq!(last.unwrap().data["attempt"], 2);
    }

    #[tokio::test]
    async fn clear_works() {
        let helper = TestHelper::new("test-worker");

        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
        helper.sync().await;

        assert_eq!(helper.event_count(), 2);
        helper.clear();
        assert_eq!(helper.event_count(), 0);
    }

    #[tokio::test]
    async fn event_count_works() {
        let helper = TestHelper::new("test-worker");

        assert_eq!(helper.event_count(), 0);

        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
        helper.sync().await;

        assert_eq!(helper.event_count(), 1);

        helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
        helper.sync().await;

        assert_eq!(helper.event_count(), 2);
    }
}
