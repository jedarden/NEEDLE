//! End-to-end telemetry test with claim operation.
//!
//! This test performs a complete claim operation and verifies comprehensive telemetry output
//! against baseline expectations, ensuring:
//! - All expected events are captured
//! - Events flow to configured sinks (file + memory)
//! - No events are missing or altered
//! - Event data matches expected structure

#![cfg(feature = "integration")]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::BufRead;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use needle::config::{FileSinkConfig, StdoutSinkConfig, TelemetryConfig};
use needle::telemetry::{EventKind, Sink, Telemetry, TelemetryEvent};
use needle::types::BeadId;
use tempfile::TempDir;

// ═════════════════════════════════════════════════════════════════════════════
// Test Infrastructure
// ═════════════════════════════════════════════════════════════════════════════

/// In-memory sink for collecting events during testing
struct MemorySink {
    events: Arc<Mutex<Vec<TelemetryEvent>>>,
}

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

impl Sink for MemorySink {
    fn accept(&self, event: &TelemetryEvent) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }

    fn flush(&self, _deadline: Duration) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Helper to emit an event and wait for it to be flushed
async fn emit_and_wait(telemetry: &Telemetry, kind: EventKind) -> anyhow::Result<()> {
    telemetry.emit(kind)?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok(())
}

/// Expected telemetry events for a complete claim operation
struct ExpectedBaseline {
    event_types: Vec<&'static str>,
    event_counts: HashMap<&'static str, usize>,
}

impl ExpectedBaseline {
    fn new() -> Self {
        let event_types = vec![
            "worker.booting",
            "worker.started",
            "bead.claim.attempted",
            "bead.claim.succeeded",
            "worker.queue_empty",
            "worker.idle",
        ];

        let mut event_counts = HashMap::new();
        for event_type in &event_types {
            event_counts.insert(*event_type, 1);
        }

        Self {
            event_types,
            event_counts,
        }
    }

    fn verify(&self, captured_events: &[TelemetryEvent]) -> anyhow::Result<()> {
        let captured_types: HashSet<String> = captured_events
            .iter()
            .map(|e| e.event_type.clone())
            .collect();

        // Check all expected events are present
        for expected_type in &self.event_types {
            if !captured_types.contains(*expected_type) {
                anyhow::bail!(
                    "Missing expected event type: {} (captured: {:?})",
                    expected_type,
                    captured_types
                );
            }
        }

        // Check event counts
        let mut captured_counts: HashMap<String, usize> = HashMap::new();
        for event in captured_events {
            *captured_counts.entry(event.event_type.clone()).or_insert(0) += 1;
        }

        for (expected_type, expected_count) in &self.event_counts {
            let actual_count = captured_counts.get(*expected_type).unwrap_or(&0);
            if actual_count != expected_count {
                anyhow::bail!(
                    "Event count mismatch for {}: expected {}, got {}",
                    expected_type,
                    expected_count,
                    actual_count
                );
            }
        }

        Ok(())
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// End-to-End Tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn end_to_end_claim_operation_telemetry() {
    // Test: Complete claim operation with memory sink capture
    let baseline = ExpectedBaseline::new();
    let (sink, events) = MemorySink::new();
    let worker_id = "e2e-test-worker";

    let telemetry = Telemetry::with_sink(worker_id.to_string(), Arc::new(sink));

    // Simulate complete claim operation lifecycle
    let test_bead_id = BeadId::from("bf-e2e-test-123");

    // Worker booting
    emit_and_wait(
        &telemetry,
        EventKind::WorkerBooting {
            worker_name: worker_id.to_string(),
            version: "test-1.0.0".to_string(),
        },
    )
    .await
    .expect("Booting event should succeed");

    // Worker started
    emit_and_wait(
        &telemetry,
        EventKind::WorkerStarted {
            worker_name: worker_id.to_string(),
            version: "test-1.0.0".to_string(),
        },
    )
    .await
    .expect("WorkerStarted event should succeed");

    // Claim attempt
    emit_and_wait(
        &telemetry,
        EventKind::ClaimAttempt {
            bead_id: test_bead_id.clone(),
            attempt: 1,
        },
    )
    .await
    .expect("ClaimAttempt event should succeed");

    // Claim succeeded
    emit_and_wait(
        &telemetry,
        EventKind::ClaimSuccess {
            bead_id: test_bead_id.clone(),
            priority: 2,
            strand: "claim".to_string(),
        },
    )
    .await
    .expect("ClaimSuccess event should succeed");

    // Worker queue empty
    emit_and_wait(&telemetry, EventKind::QueueEmpty)
        .await
        .expect("QueueEmpty event should succeed");

    // Worker idle
    emit_and_wait(
        &telemetry,
        EventKind::WorkerIdle {
            backoff_seconds: 60,
        },
    )
    .await
    .expect("WorkerIdle event should succeed");

    // Give time for all events to be captured
    tokio::time::sleep(Duration::from_millis(100)).await;

    let captured_events = events.lock().unwrap();
    println!(
        "Captured {} events during claim operation",
        captured_events.len()
    );

    // Verify baseline expectations
    baseline
        .verify(&captured_events)
        .expect("Telemetry output should match baseline");

    // Verify event data integrity
    verify_claim_operation_events(&captured_events, &test_bead_id);
}

#[tokio::test]
async fn end_to_end_claim_with_file_sink() {
    // Test: Complete claim operation with file sink
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let log_dir = temp_dir.path().join("logs");

    let config = TelemetryConfig {
        file_sink: FileSinkConfig {
            enabled: true,
            log_dir: Some(log_dir.clone()),
            retention_days: 30,
        },
        stdout_sink: StdoutSinkConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };

    let worker_id = "e2e-file-test-worker";
    let telemetry =
        Telemetry::from_config(worker_id.to_string(), &config).expect("failed to create telemetry");

    // Start the telemetry system
    telemetry.start();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Simulate claim operation
    let test_bead_id = BeadId::from("bf-file-test-456");

    telemetry
        .emit(EventKind::WorkerBooting {
            worker_name: worker_id.to_string(),
            version: "test-1.0.0".to_string(),
        })
        .expect("emit should succeed");

    telemetry
        .emit(EventKind::ClaimAttempt {
            bead_id: test_bead_id.clone(),
            attempt: 1,
        })
        .expect("ClaimAttempt should succeed");

    telemetry
        .emit(EventKind::ClaimSuccess {
            bead_id: test_bead_id.clone(),
            priority: 1,
            strand: "test-strand".to_string(),
        })
        .expect("ClaimSuccess should succeed");

    // Shutdown telemetry to ensure all events are flushed
    telemetry.shutdown().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify file sink received events
    let log_files = fs::read_dir(&log_dir).expect("failed to read log dir");
    let log_entries: Vec<_> = log_files.filter_map(|e| e.ok()).collect();

    assert!(!log_entries.is_empty(), "File sink should create log files");

    let log_file = log_entries[0].path();
    println!("File sink log: {:?}", log_file);

    let file = fs::File::open(&log_file).expect("failed to open log file");
    let reader = std::io::BufReader::new(file);
    let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

    println!("File sink captured {} events", lines.len());
    assert!(
        lines.len() >= 1,
        "File sink should capture at least 1 event"
    );

    // Verify file events are valid JSON
    for line in &lines {
        let _value: serde_json::Value =
            serde_json::from_str(line).expect(&format!("invalid JSON: {}", line));
    }

    // Verify specific events are present in file
    let file_content = lines.join("\n");
    assert!(
        file_content.contains("bead.claim.attempted"),
        "ClaimAttempt event should be in file"
    );
    assert!(
        file_content.contains("bead.claim.succeeded"),
        "ClaimSuccess event should be in file"
    );
}

#[tokio::test]
async fn end_to_end_claim_race_scenario() {
    // Test: Claim operation with race lost scenario
    let (sink, events) = MemorySink::new();
    let worker_id = "race-scenario-worker";

    let telemetry = Telemetry::with_sink(worker_id.to_string(), Arc::new(sink));

    let test_bead_id = BeadId::from("bf-race-test-789");

    // Claim attempt
    emit_and_wait(
        &telemetry,
        EventKind::ClaimAttempt {
            bead_id: test_bead_id.clone(),
            attempt: 1,
        },
    )
    .await
    .expect("ClaimAttempt should succeed");

    // Race lost
    emit_and_wait(
        &telemetry,
        EventKind::ClaimRaceLost {
            bead_id: test_bead_id.clone(),
        },
    )
    .await
    .expect("ClaimRaceLost should succeed");

    // Worker continues (queue empty)
    emit_and_wait(&telemetry, EventKind::QueueEmpty)
        .await
        .expect("QueueEmpty should succeed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let captured_events = events.lock().unwrap();

    // Verify race scenario events
    let event_types: Vec<String> = captured_events
        .iter()
        .map(|e| e.event_type.clone())
        .collect();

    assert!(
        event_types.contains(&"bead.claim.attempted".to_string()),
        "Should have ClaimAttempt event"
    );
    assert!(
        event_types.contains(&"bead.claim.race_lost".to_string()),
        "Should have ClaimRaceLost event"
    );
    assert!(
        event_types.contains(&"worker.queue_empty".to_string()),
        "Should have QueueEmpty event"
    );

    // Verify race lost event data
    let race_lost_event = captured_events
        .iter()
        .find(|e| e.event_type == "bead.claim.race_lost")
        .expect("Should have race lost event");

    assert_eq!(
        race_lost_event.bead_id.as_ref().unwrap().as_ref(),
        "bf-race-test-789",
        "Race lost event should have correct bead_id"
    );

    // Verify bead_id is in the event data payload
    if let Some(bead_id) = race_lost_event.data.get("bead_id") {
        assert_eq!(
            bead_id.as_str(),
            Some("bf-race-test-789"),
            "bead_id should be in event data"
        );
    } else {
        panic!("bead_id field missing from race lost event data");
    }
}

#[tokio::test]
async fn end_to_end_claim_error_handling() {
    // Test: Claim operation with error scenario
    let (sink, events) = MemorySink::new();
    let worker_id = "error-scenario-worker";

    let telemetry = Telemetry::with_sink(worker_id.to_string(), Arc::new(sink));

    let test_bead_id = BeadId::from("bf-error-test-999");

    // Claim attempt
    emit_and_wait(
        &telemetry,
        EventKind::ClaimAttempt {
            bead_id: test_bead_id.clone(),
            attempt: 1,
        },
    )
    .await
    .expect("ClaimAttempt should succeed");

    // Claim failed with error
    emit_and_wait(
        &telemetry,
        EventKind::ClaimFailed {
            bead_id: test_bead_id.clone(),
            reason: "Bead not found or inaccessible".to_string(),
        },
    )
    .await
    .expect("ClaimFailed should succeed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let captured_events = events.lock().unwrap();

    // Verify error scenario events
    let event_types: Vec<String> = captured_events
        .iter()
        .map(|e| e.event_type.clone())
        .collect();

    assert!(
        event_types.contains(&"bead.claim.attempted".to_string()),
        "Should have ClaimAttempt event"
    );
    assert!(
        event_types.contains(&"bead.claim.failed".to_string()),
        "Should have ClaimFailed event"
    );

    // Verify error event data
    let failed_event = captured_events
        .iter()
        .find(|e| e.event_type == "bead.claim.failed")
        .expect("Should have claim failed event");

    assert_eq!(
        failed_event.bead_id.as_ref().unwrap().as_ref(),
        "bf-error-test-999",
        "Failed event should have correct bead_id"
    );

    if let Some(reason) = failed_event.data.get("reason") {
        assert!(
            reason.as_str().unwrap().contains("not found"),
            "Reason message should be descriptive"
        );
    } else {
        panic!("reason field missing from failed event");
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Verification Helpers
// ═════════════════════════════════════════════════════════════════════════════

fn verify_claim_operation_events(events: &[TelemetryEvent], expected_bead_id: &BeadId) {
    println!("\n=== Verifying Claim Operation Events ===");

    for event in events {
        println!("Event: {} (seq: {})", event.event_type, event.sequence);

        // Verify required envelope fields
        assert!(
            !event.timestamp.to_string().is_empty(),
            "timestamp should be set"
        );
        assert!(!event.event_type.is_empty(), "event_type should be set");
        assert!(!event.worker_id.is_empty(), "worker_id should be set");
        assert!(!event.session_id.is_empty(), "session_id should be set");
        assert!(event.data.is_object(), "data should be JSON object");

        // Verify bead_id is set for bead-scoped events
        if event.event_type.starts_with("bead.claim.") {
            assert!(
                event.bead_id.is_some(),
                "bead_id should be set for bead-scoped event: {}",
                event.event_type
            );

            if let Some(bead_id) = &event.bead_id {
                assert_eq!(
                    bead_id.as_ref(),
                    expected_bead_id.as_ref(),
                    "bead_id should match expected bead"
                );
            }
        }
    }

    println!("✓ All {} events verified successfully", events.len());
}

#[tokio::test]
async fn end_to_end_multi_worker_isolation() {
    // Test: Multiple workers maintain separate telemetry streams
    let (sink1, events1) = MemorySink::new();
    let (sink2, events2) = MemorySink::new();

    let telemetry1 = Telemetry::with_sink("worker-1".to_string(), Arc::new(sink1));
    let telemetry2 = Telemetry::with_sink("worker-2".to_string(), Arc::new(sink2));

    let bead1 = BeadId::from("bf-worker-1-bead");
    let bead2 = BeadId::from("bf-worker-2-bead");

    // Worker 1 claims bead1
    telemetry1
        .emit(EventKind::ClaimAttempt {
            bead_id: bead1.clone(),
            attempt: 1,
        })
        .expect("Worker 1 claim attempt should succeed");

    // Worker 2 claims bead2
    telemetry2
        .emit(EventKind::ClaimAttempt {
            bead_id: bead2.clone(),
            attempt: 1,
        })
        .expect("Worker 2 claim attempt should succeed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let captured1 = events1.lock().unwrap();
    let captured2 = events2.lock().unwrap();

    // Verify isolation
    assert_eq!(captured1.len(), 1, "Worker 1 should have 1 event");
    assert_eq!(captured2.len(), 1, "Worker 2 should have 1 event");

    assert_eq!(
        captured1[0].worker_id, "worker-1",
        "Worker 1 event should have correct worker_id"
    );
    assert_eq!(
        captured1[0].bead_id.as_ref().unwrap().as_ref(),
        "bf-worker-1-bead",
        "Worker 1 event should have correct bead_id"
    );

    assert_eq!(
        captured2[0].worker_id, "worker-2",
        "Worker 2 event should have correct worker_id"
    );
    assert_eq!(
        captured2[0].bead_id.as_ref().unwrap().as_ref(),
        "bf-worker-2-bead",
        "Worker 2 event should have correct bead_id"
    );

    // Note: Session IDs may be the same in test mode due to deterministic generation
    // The important thing is that worker isolation is maintained (correct events)
}
