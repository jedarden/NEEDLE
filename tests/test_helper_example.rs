//! Example integration test demonstrating telemetry test helper usage.
//!
//! This test shows how to use the `TestHelper` infrastructure to capture
//! and assert on telemetry events during integration testing.

#![cfg(feature = "integration")]

use needle::telemetry::test_utils::TestHelper;
use needle::telemetry::EventKind;
use needle::types::BeadId;

#[tokio::test]
async fn test_helper_captures_telemetry_events() {
    // Create a test helper with a memory-backed telemetry emitter
    let helper = TestHelper::new("integration-test-worker");

    // Emit events through the helper's telemetry emitter
    helper
        .telemetry()
        .emit(EventKind::WorkerBooting {
            worker_name: "test-worker".to_string(),
            version: "0.1.0".to_string(),
        })
        .unwrap();

    helper.telemetry().emit(EventKind::QueueEmpty).unwrap();

    helper
        .telemetry()
        .emit(EventKind::ClaimAttempt {
            bead_id: BeadId::from("needle-test-123"),
            attempt: 1,
        })
        .unwrap();

    // Wait for async event delivery
    helper.sync().await;

    // Query captured events
    assert_eq!(helper.event_count(), 3);

    // Filter by event type
    let booting_events = helper.events_by_type("worker.booting");
    assert_eq!(booting_events.len(), 1);

    let queue_events = helper.events_by_type("worker.queue_empty");
    assert_eq!(queue_events.len(), 1);

    // Filter by bead ID
    let bead_events = helper.events_by_bead_id("needle-test-123");
    assert_eq!(bead_events.len(), 1);

    // Use helper assertion methods
    helper.assert_event_emitted("worker.booting");
    helper.assert_event_emitted("worker.queue_empty");
    helper.assert_event_emitted("bead.claim.attempted");
    helper.assert_event_count("worker.booting", 1);
    helper.assert_event_not_emitted("worker.errored");

    // Find specific events
    if let Some(event) = helper.find_event("worker.booting") {
        assert_eq!(event.worker_id, "integration-test-worker");
        assert_eq!(event.data["worker_name"], "test-worker");
        assert_eq!(event.data["version"], "0.1.0");
    }

    // Clear events for next test phase
    helper.clear();
    assert_eq!(helper.event_count(), 0);
}

#[tokio::test]
async fn test_helper_supports_event_filtering() {
    let helper = TestHelper::new("filter-test-worker");

    // Emit multiple events of the same type
    for i in 0..5 {
        helper
            .telemetry()
            .emit(EventKind::ClaimAttempt {
                bead_id: BeadId::from(format!("needle-{}", i)),
                attempt: i + 1,
            })
            .unwrap();
    }

    helper.sync().await;

    // Verify count
    helper.assert_event_count("bead.claim.attempted", 5);

    // Get first and last events
    let first = helper.find_event("bead.claim.attempted").unwrap();
    assert_eq!(first.data["attempt"], 1);

    let last = helper.last_event("bead.claim.attempted").unwrap();
    assert_eq!(last.data["attempt"], 5);
}

#[tokio::test]
async fn test_helper_clears_events_properly() {
    let helper = TestHelper::new("clear-test-worker");

    // Emit some events
    helper.telemetry().emit(EventKind::QueueEmpty).unwrap();
    helper.sync().await;

    assert_eq!(helper.event_count(), 1);

    // Clear and verify empty
    helper.clear();
    assert_eq!(helper.event_count(), 0);

    // Emit more events after clear
    helper
        .telemetry()
        .emit(EventKind::WorkerStarted {
            worker_name: "test".to_string(),
            version: "0.1.0".to_string(),
        })
        .unwrap();
    helper.sync().await;

    assert_eq!(helper.event_count(), 1);
    helper.assert_event_emitted("worker.started");
    helper.assert_event_not_emitted("worker.queue_empty");
}
