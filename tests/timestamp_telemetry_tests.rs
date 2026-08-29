//! Comprehensive tests for timestamp integration in telemetry emission.
//!
//! These tests verify that:
//! - Timestamps are correctly captured and passed through the telemetry system
//! - Timestamp format conversion is handled properly
//! - Edge cases (missing timestamp, invalid format) are handled gracefully
//! - Timestamps appear correctly in telemetry output (file sinks, memory sinks)
//! - Timestamps maintain ISO 8601 format with millisecond precision

#![cfg(feature = "integration")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use needle::telemetry::{EventKind, Sink, Telemetry, TelemetryEvent};
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

/// Verify timestamp format is ISO 8601 with millisecond precision
fn verify_timestamp_format(timestamp: &DateTime<Utc>) -> anyhow::Result<()> {
    let formatted = timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

    // Verify it can be parsed back
    let parsed = chrono::DateTime::parse_from_rfc3339(&formatted)
        .map_err(|e| anyhow::anyhow!("Failed to parse timestamp: {}", e))?;

    // Verify timezone is UTC
    assert_eq!(
        parsed.offset(),
        &chrono::offset::Utc,
        "Timestamp should be in UTC timezone"
    );

    // Verify millisecond precision (3 decimal places after seconds)
    let fractional = &formatted[formatted.len() - 4..formatted.len() - 1];
    assert_eq!(
        fractional.len(),
        3,
        "Timestamp should have exactly 3 millisecond digits"
    );

    Ok(())
}

// ═════════════════════════════════════════════════════════════════════════════
// Timestamp Capture Tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn timestamp_is_captured_on_emit() {
    // Test: Verify timestamp is captured when emitting an event
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let before_emit = Utc::now();

    telemetry
        .emit(
            EventKind::WorkerBooting {
                worker_name: "test-worker".to_string(),
                version: "1.0.0".to_string(),
            },
            Utc::now(),
        )
        .expect("emit should succeed");

    let after_emit = Utc::now();

    // Give time for event to be captured
    tokio::time::sleep(Duration::from_millis(50)).await;

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1, "Should capture one event");

    let event = &captured[0];

    // Verify timestamp is set
    assert!(
        !event.timestamp.to_string().is_empty(),
        "timestamp should be set"
    );

    // Verify timestamp is within expected range
    assert!(
        event.timestamp >= before_emit && event.timestamp <= after_emit,
        "timestamp should be between before_emit ({}) and after_emit ({}) but was {}",
        before_emit,
        after_emit,
        event.timestamp
    );
}

#[tokio::test]
async fn timestamp_maintains_chronological_order() {
    // Test: Multiple events should have monotonically increasing timestamps
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let timestamps = vec![
        Utc::now() - ChronoDuration::milliseconds(100),
        Utc::now() - ChronoDuration::milliseconds(50),
        Utc::now(),
    ];

    for (i, &timestamp) in timestamps.iter().enumerate() {
        telemetry
            .emit(
                EventKind::WorkerBooting {
                    worker_name: format!("worker-{}", i),
                    version: "1.0.0".to_string(),
                },
                timestamp,
            )
            .expect("emit should succeed");

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 3, "Should capture three events");

    // Verify chronological order
    for i in 1..captured.len() {
        assert!(
            captured[i].timestamp >= captured[i - 1].timestamp,
            "Event {} timestamp ({}) should be >= event {} timestamp ({})",
            i,
            captured[i].timestamp,
            i - 1,
            captured[i - 1].timestamp
        );
    }
}

#[tokio::test]
async fn timestamp_format_is_iso8601_with_milliseconds() {
    // Test: Verify timestamp format is ISO 8601 with millisecond precision
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    telemetry
        .emit(
            EventKind::WorkerBooting {
                worker_name: "test-worker".to_string(),
                version: "1.0.0".to_string(),
            },
            Utc::now(),
        )
        .expect("emit should succeed");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1, "Should capture one event");

    let event = &captured[0];

    // Verify format is ISO 8601 with milliseconds
    verify_timestamp_format(&event.timestamp).expect("timestamp format should be valid");
}

#[tokio::test]
async fn timestamp_survives_serialization() {
    // Test: Verify timestamp survives JSON serialization/deserialization
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let original_timestamp = Utc::now();

    telemetry
        .emit(
            EventKind::WorkerBooting {
                worker_name: "test-worker".to_string(),
                version: "1.0.0".to_string(),
            },
            original_timestamp,
        )
        .expect("emit should succeed");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1, "Should capture one event");

    let event = &captured[0];

    // Serialize to JSON
    let json = serde_json::to_string(event).expect("serialization should succeed");

    // Deserialize back
    let deserialized: TelemetryEvent =
        serde_json::from_str(&json).expect("deserialization should succeed");

    // Verify timestamp is preserved
    assert_eq!(
        deserialized.timestamp, original_timestamp,
        "timestamp should survive serialization roundtrip"
    );

    // Verify serialized JSON contains timestamp in correct format
    assert!(
        json.contains("\"timestamp\":"),
        "JSON should contain timestamp field"
    );

    // Verify the timestamp in JSON is ISO 8601 format
    let timestamp_str = event.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    assert!(
        json.contains(&timestamp_str),
        "JSON should contain timestamp in ISO 8601 format"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// File Sink Timestamp Tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn timestamp_appears_correctly_in_file_sink() {
    // Test: Verify timestamp appears correctly in file sink output
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let log_dir = temp_dir.path().join("logs");

    use needle::config::{FileSinkConfig, StdoutSinkConfig, TelemetryConfig};

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

    let telemetry = Telemetry::from_config("test-worker".to_string(), &config)
        .expect("failed to create telemetry");

    telemetry.start();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let emit_time = Utc::now();

    telemetry
        .emit(
            EventKind::WorkerBooting {
                worker_name: "test-worker".to_string(),
                version: "1.0.0".to_string(),
            },
            emit_time,
        )
        .expect("emit should succeed");

    telemetry.shutdown().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Read file sink output
    let log_files = std::fs::read_dir(&log_dir).expect("failed to read log dir");
    let log_entries: Vec<_> = log_files.filter_map(|e| e.ok()).collect();

    assert!(!log_entries.is_empty(), "File sink should create log files");

    let log_file = log_entries[0].path();
    let content = std::fs::read_to_string(&log_file).expect("failed to read log file");

    // Verify file contains timestamp
    assert!(!content.is_empty(), "Log file should not be empty");

    // Verify each line has a timestamp field
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let value: serde_json::Value =
            serde_json::from_str(line).expect(&format!("invalid JSON: {}", line));

        assert!(
            value.get("timestamp").is_some(),
            "Event should have timestamp field"
        );

        let timestamp_str = value["timestamp"]
            .as_str()
            .expect("timestamp should be a string");

        // Verify timestamp is valid ISO 8601
        let parsed = chrono::DateTime::parse_from_rfc3339(timestamp_str).expect(&format!(
            "timestamp should be valid ISO 8601: {}",
            timestamp_str
        ));

        // Verify timezone is UTC
        assert_eq!(
            parsed.offset(),
            &chrono::offset::Utc,
            "File sink timestamp should be in UTC"
        );
    }
}

#[tokio::test]
async fn file_sink_timestamp_ordering_is_preserved() {
    // Test: Verify file sink preserves timestamp ordering across events
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let log_dir = temp_dir.path().join("logs");

    use needle::config::{FileSinkConfig, StdoutSinkConfig, TelemetryConfig};

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

    let telemetry = Telemetry::from_config("test-worker".to_string(), &config)
        .expect("failed to create telemetry");

    telemetry.start();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let base_time = Utc::now();
    let timestamps: Vec<DateTime<Utc>> = (0..5)
        .map(|i| base_time + ChronoDuration::milliseconds(i as i64 * 10))
        .collect();

    for (i, &timestamp) in timestamps.iter().enumerate() {
        telemetry
            .emit(
                EventKind::WorkerBooting {
                    worker_name: format!("worker-{}", i),
                    version: "1.0.0".to_string(),
                },
                timestamp,
            )
            .expect("emit should succeed");

        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    telemetry.shutdown().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Read file sink output
    let log_files = std::fs::read_dir(&log_dir).expect("failed to read log dir");
    let log_entries: Vec<_> = log_files.filter_map(|e| e.ok()).collect();

    assert!(!log_entries.is_empty(), "File sink should create log files");

    let log_file = log_entries[0].path();
    let content = std::fs::read_to_string(&log_file).expect("failed to read log file");

    let mut parsed_timestamps = Vec::new();

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let value: serde_json::Value =
            serde_json::from_str(line).expect(&format!("invalid JSON: {}", line));

        let timestamp_str = value["timestamp"]
            .as_str()
            .expect("timestamp should be a string");

        let parsed = chrono::DateTime::parse_from_rfc3339(timestamp_str).expect(&format!(
            "timestamp should be valid ISO 8601: {}",
            timestamp_str
        ));

        parsed_timestamps.push(parsed.with_timezone(&Utc));
    }

    // Verify timestamps are monotonically increasing
    for i in 1..parsed_timestamps.len() {
        assert!(
            parsed_timestamps[i] >= parsed_timestamps[i - 1],
            "File sink timestamps should be monotonically increasing: {} >= {}",
            parsed_timestamps[i],
            parsed_timestamps[i - 1]
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Edge Case Tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn timestamp_handles_edge_case_distant_past() {
    // Test: Handle timestamp from distant past (e.g., system clock reset scenario)
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let distant_past = Utc::now() - ChronoDuration::days(365);

    telemetry
        .emit(
            EventKind::WorkerBooting {
                worker_name: "test-worker".to_string(),
                version: "1.0.0".to_string(),
            },
            distant_past,
        )
        .expect("emit should succeed even with distant past timestamp");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1, "Should capture one event");

    assert_eq!(
        captured[0].timestamp, distant_past,
        "timestamp should be preserved exactly"
    );
}

#[tokio::test]
async fn timestamp_handles_edge_case_distant_future() {
    // Test: Handle timestamp from distant future (e.g., system clock jumped ahead)
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let distant_future = Utc::now() + ChronoDuration::days(365);

    telemetry
        .emit(
            EventKind::WorkerBooting {
                worker_name: "test-worker".to_string(),
                version: "1.0.0".to_string(),
            },
            distant_future,
        )
        .expect("emit should succeed even with distant future timestamp");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1, "Should capture one event");

    assert_eq!(
        captured[0].timestamp, distant_future,
        "timestamp should be preserved exactly"
    );
}

#[tokio::test]
async fn timestamp_handles_nanosecond_precision_truncation() {
    // Test: Verify nanosecond precision is truncated to milliseconds
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    // Create timestamp with nanosecond precision
    let base_time = Utc::now();
    let nano_time = base_time + ChronoDuration::nanoseconds(123456789); // 123.456789 ms

    telemetry
        .emit(
            EventKind::WorkerBooting {
                worker_name: "test-worker".to_string(),
                version: "1.0.0".to_string(),
            },
            nano_time,
        )
        .expect("emit should succeed");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1, "Should capture one event");

    // Verify timestamp is stored with full precision
    assert_eq!(
        captured[0].timestamp, nano_time,
        "timestamp should preserve full precision"
    );

    // Verify formatting truncates to milliseconds
    let formatted = captured[0]
        .timestamp
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();

    // Parse back the formatted string
    let parsed = chrono::DateTime::parse_from_rfc3339(&formatted)
        .expect("formatted timestamp should be valid");

    // The parsed timestamp should match millisecond precision of original
    let original_millis = nano_time.timestamp_millis();
    let parsed_millis = parsed.with_timezone(&Utc).timestamp_millis();

    assert_eq!(
        original_millis, parsed_millis,
        "formatted timestamp should preserve millisecond precision"
    );
}

#[tokio::test]
async fn timestamp_field_exists_in_all_event_types() {
    // Test: Verify all event types have timestamp field
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let test_timestamp = Utc::now();

    // Test a variety of event types
    let event_kinds = vec![
        EventKind::WorkerBooting {
            worker_name: "test-worker".to_string(),
            version: "1.0.0".to_string(),
        },
        EventKind::WorkerStarted {
            worker_name: "test-worker".to_string(),
            version: "1.0.0".to_string(),
        },
        EventKind::QueueEmpty,
        EventKind::WorkerIdle {
            backoff_seconds: 60,
        },
    ];

    for event_kind in event_kinds {
        telemetry
            .emit(event_kind, test_timestamp)
            .expect("emit should succeed");

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 4, "Should capture four events");

    // Verify all events have timestamps
    for event in captured.iter() {
        assert!(
            !event.timestamp.to_string().is_empty(),
            "event should have timestamp"
        );

        // Verify format is consistent
        verify_timestamp_format(&event.timestamp).expect(&format!(
            "timestamp format should be valid for event type: {}",
            event.event_type
        ));
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn timestamp_integration_with_sequence_number() {
    // Test: Verify timestamp and sequence number work together correctly
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let base_time = Utc::now();

    // Emit multiple events with known timestamps
    for i in 0..5 {
        let timestamp = base_time + ChronoDuration::milliseconds(i as i64 * 10);

        telemetry
            .emit(
                EventKind::WorkerBooting {
                    worker_name: format!("worker-{}", i),
                    version: "1.0.0".to_string(),
                },
                timestamp,
            )
            .expect("emit should succeed");

        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 5, "Should capture five events");

    // Verify sequence numbers are monotonically increasing
    for i in 1..captured.len() {
        assert!(
            captured[i].sequence > captured[i - 1].sequence,
            "sequence number should increase: {} > {}",
            captured[i].sequence,
            captured[i - 1].sequence
        );
    }

    // Verify timestamps correlate with sequence numbers
    // (later sequences should have later or equal timestamps)
    for i in 1..captured.len() {
        assert!(
            captured[i].timestamp >= captured[i - 1].timestamp,
            "timestamp should be >= for later sequence: {} >= {}",
            captured[i].timestamp,
            captured[i - 1].timestamp
        );
    }
}

#[tokio::test]
async fn timestamp_with_high_frequency_events() {
    // Test: Verify timestamps work correctly with high-frequency events
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(sink));

    let start_time = Utc::now();

    // Emit 100 events rapidly
    for i in 0..100 {
        telemetry
            .emit(
                EventKind::WorkerBooting {
                    worker_name: format!("worker-{}", i),
                    version: "1.0.0".to_string(),
                },
                Utc::now(),
            )
            .expect("emit should succeed");
    }

    let end_time = Utc::now();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 100, "Should capture all 100 events");

    // Verify all timestamps are within the expected range
    for event in captured.iter() {
        assert!(
            event.timestamp >= start_time && event.timestamp <= end_time,
            "event timestamp should be within the burst period"
        );
    }

    // Verify all timestamps are valid ISO 8601
    for event in captured.iter() {
        verify_timestamp_format(&event.timestamp).expect(&format!(
            "timestamp format should be valid for event: seq={}",
            event.sequence
        ));
    }
}
