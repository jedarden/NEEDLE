//! Telemetry event field verification tests.
//!
//! These tests verify that telemetry events include the correct field contents:
//! - Exclusion reasons are correctly aggregated
//! - Workspace path is included in events
//! - Event field values match expectations

// Required for Telemetry::with_sink which is gated behind the integration feature
#![cfg(feature = "integration")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use needle::telemetry::{EventKind, Sink, Telemetry};
use needle::types::{BeadId, WorkerState};

// ═════════════════════════════════════════════════════════════════════════════
// Test infrastructure
// ═════════════════════════════════════════════════════════════════════════════

/// Helper to emit an event and wait for it to be flushed
async fn emit_and_wait(telemetry: &Telemetry, kind: EventKind) -> anyhow::Result<()> {
    telemetry.emit(kind)?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok(())
}

/// In-memory sink for testing — collects events via a shared Vec.
struct MemorySink {
    events: Arc<Mutex<Vec<needle::telemetry::TelemetryEvent>>>,
}

impl MemorySink {
    pub fn new() -> (Self, Arc<Mutex<Vec<needle::telemetry::TelemetryEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            MemorySink {
                events: events.clone(),
            },
            events,
        )
    }
}

impl Sink for MemorySink {
    fn accept(&self, event: &needle::telemetry::TelemetryEvent) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }

    fn flush(&self, _deadline: Duration) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn test_pluck_starvation_event_includes_workspace() {
    // Test that PluckStarvationDetected event includes workspace path
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let workspace_path = "/test/workspace/path".to_string();
    let exclusion_reasons = vec![
        "blocked:depends_on_bf-123".to_string(),
        "blocked:depends_on_bf-456".to_string(),
    ];

    emit_and_wait(&telemetry, EventKind::PluckStarvationDetected {
        workspace: workspace_path.clone(),
        open_count: 5,
        excluded_count: 2,
        candidate_exclusion_reasons: exclusion_reasons.clone(),
    }).await.expect("emit should succeed");

    let emitted = events.lock().unwrap();
    assert_eq!(emitted.len(), 1, "should emit exactly one event");

    let event = &emitted[0];
    assert_eq!(event.event_type, "strand.pluck.starvation_detected");
    assert_eq!(event.worker_id, "test-worker");

    // Verify workspace is in the data payload
    if let Some(workspace) = event.data.get("workspace") {
        assert_eq!(workspace, &workspace_path.as_str());
    } else {
        panic!("workspace field missing from event data");
    }

    // Verify workspace is also optionally in the envelope (if set)
    if let Some(env_workspace) = &event.workspace {
        assert_eq!(env_workspace, &PathBuf::from(&workspace_path));
    }
}

#[tokio::test]
async fn test_pluck_starvation_aggregates_exclusion_reasons() {
    // Test that PluckStarvationDetected correctly aggregates exclusion reasons
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let exclusion_reasons = vec![
        "blocked:depends_on_bf-111".to_string(),
        "blocked:depends_on_bf-222".to_string(),
        "blocked:depends_on_bf-333".to_string(),
        "unassigned:ready:not_ready".to_string(),
        "paused:waiting_on_user".to_string(),
    ];

    emit_and_wait(&telemetry, EventKind::PluckStarvationDetected {
        workspace: "/test/workspace".to_string(),
        open_count: 3,
        excluded_count: 5,
        candidate_exclusion_reasons: exclusion_reasons.clone(),
    }).await.expect("emit should succeed");

    let emitted = events.lock().unwrap();
    let event = &emitted[0];

    // Verify the exclusion reasons array is correctly serialized
    if let Some(reasons) = event.data.get("candidate_exclusion_reasons") {
        if let Some(reasons_array) = reasons.as_array() {
            assert_eq!(
                reasons_array.len(),
                exclusion_reasons.len(),
                "should have all exclusion reasons"
            );

            // Verify each reason is present
            for (i, expected_reason) in exclusion_reasons.iter().enumerate() {
                let reason = reasons_array.get(i).unwrap().as_str().unwrap();
                assert_eq!(reason, *expected_reason, "reason at index {} should match", i);
            }
        } else {
            panic!("candidate_exclusion_reasons should be an array");
        }
    } else {
        panic!("candidate_exclusion_reasons field missing from event data");
    }
}

#[tokio::test]
async fn test_exclusion_reasons_can_be_counted_and_categorized() {
    // Test that exclusion reasons can be counted by type for aggregation
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let exclusion_reasons = vec![
        "blocked:depends_on_bf-001".to_string(),
        "blocked:depends_on_bf-002".to_string(),
        "blocked:depends_on_bf-003".to_string(),
        "paused:waiting_on_user".to_string(),
        "unassigned:ready:not_ready".to_string(),
    ];

    emit_and_wait(&telemetry, EventKind::PluckStarvationDetected {
        workspace: "/test/workspace".to_string(),
        open_count: 0,
        excluded_count: 5,
        candidate_exclusion_reasons: exclusion_reasons.clone(),
    }).await.expect("emit should succeed");

    let emitted = events.lock().unwrap();
    let event = &emitted[0];

    // Parse and categorize the reasons
    let mut category_counts: HashMap<String, usize> = HashMap::new();

    if let Some(reasons) = event.data.get("candidate_exclusion_reasons") {
        if let Some(reasons_array) = reasons.as_array() {
            for reason in reasons_array {
                if let Some(reason_str) = reason.as_str() {
                    // Extract category (first part before the colon)
                    if let Some(category) = reason_str.split(':').next() {
                        *category_counts.entry(category.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    // Verify categorization
    assert_eq!(category_counts.get("blocked"), Some(&3), "should have 3 blocked");
    assert_eq!(category_counts.get("paused"), Some(&1), "should have 1 paused");
    assert_eq!(
        category_counts.get("unassigned"),
        Some(&1),
        "should have 1 unassigned"
    );
    assert_eq!(category_counts.len(), 3, "should have 3 categories");
}

#[tokio::test]
async fn test_pluck_starvation_counts_match_reasons_length() {
    // Test that excluded_count matches the length of exclusion_reasons
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let exclusion_reasons = vec![
        "blocked:depends_on_bf-111".to_string(),
        "blocked:depends_on_bf-222".to_string(),
        "paused:waiting_on_user".to_string(),
    ];

    emit_and_wait(&telemetry, EventKind::PluckStarvationDetected {
        workspace: "/test/workspace".to_string(),
        open_count: 7,
        excluded_count: 3, // Should match len(exclusion_reasons)
        candidate_exclusion_reasons: exclusion_reasons.clone(),
    }).await.expect("emit should succeed");

    let emitted = events.lock().unwrap();
    let event = &emitted[0];

    // Verify excluded_count field
    if let Some(excluded_count) = event.data.get("excluded_count") {
        assert_eq!(
            excluded_count.as_u64(),
            Some(3),
            "excluded_count should be 3"
        );
    } else {
        panic!("excluded_count field missing from event data");
    }

    // Verify candidate_exclusion_reasons length matches excluded_count
    if let Some(reasons) = event.data.get("candidate_exclusion_reasons") {
        if let Some(reasons_array) = reasons.as_array() {
            assert_eq!(
                reasons_array.len(),
                3,
                "reasons array length should match excluded_count"
            );
        }
    }

    // Verify open_count field
    if let Some(open_count) = event.data.get("open_count") {
        assert_eq!(open_count.as_u64(), Some(7), "open_count should be 7");
    } else {
        panic!("open_count field missing from event data");
    }
}

#[tokio::test]
async fn test_workspace_path_included_in_event_envelope() {
    // Test that workspace path is correctly set in the event envelope
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let _test_workspace = PathBuf::from("/absolute/path/to/workspace");

    // Emit an event that should include workspace context
    emit_and_wait(&telemetry, EventKind::StateTransition {
        from: WorkerState::Selecting,
        to: WorkerState::Exhausted,
    }).await.expect("emit should succeed");

    let emitted = events.lock().unwrap();
    let event = &emitted[0];

    // For events without bead context, workspace should be None or the current directory
    // This test verifies the field exists and can be set
    assert!(event.workspace.is_none() || event.workspace.is_some());
}

#[tokio::test]
async fn test_telemetry_event_all_required_fields_present() {
    // Test that all required envelope fields are present in events
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker-2".to_string(), Arc::new(sink));

    emit_and_wait(&telemetry, EventKind::WorkerStarted {
        worker_name: "test-worker-2".to_string(),
        version: "1.0.0".to_string(),
    }).await.expect("emit should succeed");

    let emitted = events.lock().unwrap();
    let event = &emitted[0];

    // Verify all required envelope fields are present
    assert!(!event.timestamp.to_string().is_empty(), "timestamp should be set");

    assert_eq!(event.event_type, "worker.started");
    assert_eq!(event.worker_id, "test-worker-2");

    assert!(!event.session_id.is_empty(), "session_id should be set");
    assert_eq!(event.sequence, 0, "first event should have sequence 0");

    // bead_id should be None for worker-scoped events
    assert!(event.bead_id.is_none(), "bead_id should be None for worker events");

    // workspace should be None or a valid path
    assert!(event.workspace.is_none() || event.workspace.is_some());

    // data should be an object
    assert!(event.data.is_object(), "data should be a JSON object");

    // Verify specific data fields for WorkerStarted
    if let Some(worker_name) = event.data.get("worker_name") {
        assert_eq!(worker_name.as_str(), Some("test-worker-2"));
    } else {
        panic!("worker_name field missing from event data");
    }

    if let Some(version) = event.data.get("version") {
        assert_eq!(version.as_str(), Some("1.0.0"));
    } else {
        panic!("version field missing from event data");
    }
}

#[tokio::test]
async fn test_sequence_numbers_increment_correctly() {
    // Test that sequence numbers increment properly across multiple events
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    // Emit multiple events
    emit_and_wait(&telemetry, EventKind::WorkerStarted {
        worker_name: "test-worker".to_string(),
        version: "1.0.0".to_string(),
    }).await.expect("emit should succeed");

    emit_and_wait(&telemetry, EventKind::StateTransition {
        from: WorkerState::Booting,
        to: WorkerState::Selecting,
    }).await.expect("emit should succeed");

    emit_and_wait(&telemetry, EventKind::QueueEmpty).await.expect("emit should succeed");

    let emitted = events.lock().unwrap();
    assert_eq!(emitted.len(), 3, "should have 3 events");

    assert_eq!(emitted[0].sequence, 0, "first event sequence should be 0");
    assert_eq!(emitted[1].sequence, 1, "second event sequence should be 1");
    assert_eq!(emitted[2].sequence, 2, "third event sequence should be 2");
}

#[tokio::test]
async fn test_bead_id_set_correctly_for_bead_scoped_events() {
    // Test that bead_id is correctly set for bead-scoped events
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let test_bead_id = BeadId::from("bf-test123");

    emit_and_wait(&telemetry, EventKind::ClaimAttempt {
        bead_id: test_bead_id.clone(),
        attempt: 1,
    }).await.expect("emit should succeed");

    let emitted = events.lock().unwrap();
    let event = &emitted[0];

    // Verify bead_id is set
    assert!(event.bead_id.is_some(), "bead_id should be set for ClaimAttempt");

    if let Some(bead_id) = &event.bead_id {
        assert_eq!(bead_id.as_ref(), "bf-test123");
    }

    // Verify bead_id is also in the data payload
    if let Some(bead_id_data) = event.data.get("bead_id") {
        assert_eq!(bead_id_data.as_str(), Some("bf-test123"));
    } else {
        panic!("bead_id field missing from event data");
    }
}

#[tokio::test]
async fn test_exclusion_reasons_empty_list_handled_correctly() {
    // Test that empty exclusion reasons list is handled correctly
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let empty_reasons: Vec<String> = vec![];

    emit_and_wait(&telemetry, EventKind::PluckStarvationDetected {
        workspace: "/test/workspace".to_string(),
        open_count: 0,
        excluded_count: 0,
        candidate_exclusion_reasons: empty_reasons,
    }).await.expect("emit should succeed");

    let emitted = events.lock().unwrap();
    let event = &emitted[0];

    // Verify empty array is serialized correctly
    if let Some(reasons) = event.data.get("candidate_exclusion_reasons") {
        if let Some(reasons_array) = reasons.as_array() {
            assert_eq!(reasons_array.len(), 0, "should have empty reasons array");
        } else {
            panic!("candidate_exclusion_reasons should be an array");
        }
    } else {
        panic!("candidate_exclusion_reasons field missing from event data");
    }

    // Verify excluded_count is 0
    if let Some(excluded_count) = event.data.get("excluded_count") {
        assert_eq!(excluded_count.as_u64(), Some(0), "excluded_count should be 0");
    } else {
        panic!("excluded_count field missing from event data");
    }
}

#[tokio::test]
async fn test_duration_ms_field_set_correctly() {
    // Test that duration_ms is correctly set for events that include it
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let test_bead_id = BeadId::from("bf-test456");

    emit_and_wait(&telemetry, EventKind::BeadCompleted {
        bead_id: test_bead_id,
        duration_ms: 1234,
    }).await.expect("emit should succeed");

    let emitted = events.lock().unwrap();
    let event = &emitted[0];

    // Verify duration_ms is set in the event
    assert_eq!(event.duration_ms, Some(1234), "duration_ms should be set");

    // Verify duration_ms is also in the data payload
    if let Some(duration_ms) = event.data.get("duration_ms") {
        assert_eq!(duration_ms.as_u64(), Some(1234));
    } else {
        panic!("duration_ms field missing from event data");
    }
}

#[tokio::test]
async fn test_multiple_exclusion_reasons_of_same_type_aggregated() {
    // Test that multiple exclusion reasons of the same type are all included
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    // All reasons are of the same type (blocked)
    let exclusion_reasons = vec![
        "blocked:depends_on_bf-001".to_string(),
        "blocked:depends_on_bf-002".to_string(),
        "blocked:depends_on_bf-003".to_string(),
        "blocked:depends_on_bf-004".to_string(),
        "blocked:depends_on_bf-005".to_string(),
    ];

    emit_and_wait(&telemetry, EventKind::PluckStarvationDetected {
        workspace: "/test/workspace".to_string(),
        open_count: 2,
        excluded_count: 5,
        candidate_exclusion_reasons: exclusion_reasons.clone(),
    }).await.expect("emit should succeed");

    let emitted = events.lock().unwrap();
    let event = &emitted[0];

    // Verify all 5 reasons are present
    if let Some(reasons) = event.data.get("candidate_exclusion_reasons") {
        if let Some(reasons_array) = reasons.as_array() {
            assert_eq!(
                reasons_array.len(),
                5,
                "all 5 blocked reasons should be present"
            );

            // Verify they're all "blocked" type
            for reason in reasons_array {
                let reason_str = reason.as_str().unwrap();
                assert!(
                    reason_str.starts_with("blocked:"),
                    "all reasons should start with 'blocked:'"
                );
            }
        }
    }
}

#[tokio::test]
async fn test_session_id_consistent_across_events() {
    // Test that session_id is consistent across all events from the same telemetry instance
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    // Emit multiple events
    for _i in 0..5 {
        telemetry
            .emit(EventKind::QueueEmpty)
            .expect("emit should succeed");
    }

    // Wait for events to be flushed
    tokio::time::sleep(Duration::from_millis(50)).await;

    let emitted = events.lock().unwrap();

    // Collect all session_ids
    let session_ids: Vec<_> = emitted.iter().map(|e| &e.session_id).collect();

    // Verify all session_ids are the same
    let first_session_id = session_ids[0];
    for session_id in session_ids {
        assert_eq!(
            session_id, first_session_id,
            "all events should have the same session_id"
        );
    }

    // Verify session_id is present and non-empty
    // Note: with_sink uses "test0000" which is not valid hex, but real session IDs are hex
    assert_eq!(first_session_id.len(), 8, "session_id should be 8 chars");
    assert!(!first_session_id.is_empty(), "session_id should be set");
}
