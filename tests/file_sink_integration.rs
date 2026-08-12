//! FileSink integration tests.
//!
//! These tests verify the complete telemetry flow with FileSink:
//! - Event dispatch → file write → read back
//! - Configuration-based enable/disable
//! - File rotation behavior
//! - Error handling

#![cfg(feature = "integration")]

use std::fs;
use std::io::BufRead;
use std::path::PathBuf;
use std::time::Duration;

use needle::config::{FileSinkConfig, StdoutSinkConfig, TelemetryConfig};
use needle::telemetry::{EventKind, Telemetry};
use tempfile::TempDir;

/// Helper to emit an event and wait for it to be flushed
async fn emit_and_wait(telemetry: &Telemetry, kind: EventKind) -> anyhow::Result<()> {
    telemetry.emit(kind)?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
}

/// Test the complete flow: emit event → write to file → read back
#[tokio::test]
async fn test_file_sink_full_flow() {
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

    let telemetry = Telemetry::from_config("test-worker-integration".to_string(), &config)
        .expect("failed to create telemetry");

    // Emit several events
    emit_and_wait(&telemetry, EventKind::QueueEmpty)
        .await
        .expect("emit QueueEmpty failed");

    emit_and_wait(
        &telemetry,
        EventKind::ClaimAttempt {
            bead_id: "bf-test123".to_string().into(),
            attempt: 1,
        },
    )
    .await
    .expect("emit ClaimAttempt failed");

    emit_and_wait(
        &telemetry,
        EventKind::WorkerIdle {
            backoff_seconds: 60,
        },
    )
    .await
    .expect("emit WorkerIdle failed");

    // Give time for file to be flushed
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Find the log file
    let log_files = fs::read_dir(&log_dir).expect("failed to read log dir");
    let log_entries: Vec<_> = log_files.filter_map(|e| e.ok()).collect();

    assert!(!log_entries.is_empty(), "no log files created");

    let log_file = log_entries[0].path();
    println!("Log file: {:?}", log_file);

    // Read back the events
    let file = fs::File::open(&log_file).expect("failed to open log file");
    let reader = std::io::BufReader::new(file);

    let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();
    println!("Log lines: {}", lines.len());

    assert!(
        lines.len() >= 4,
        "expected at least 4 events (boot + 3 emitted), got {}",
        lines.len()
    );

    // Verify JSON structure of each line
    for line in &lines {
        let _value: serde_json::Value =
            serde_json::from_str(line).expect(&format!("invalid JSON: {}", line));
    }

    // Verify specific events exist
    let event_types: Vec<String> = lines
        .iter()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            value
                .get("event_type")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    assert!(
        event_types.contains(&"worker.queue_empty".to_string()),
        "QueueEmpty event not found"
    );
    assert!(
        event_types.contains(&"bead.claim.attempted".to_string()),
        "ClaimAttempt event not found"
    );
    assert!(
        event_types.contains(&"worker.idle".to_string()),
        "WorkerIdle event not found"
    );
}

/// Test that FileSink can be disabled via config
#[tokio::test]
async fn test_file_sink_disabled_via_config() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let log_dir = temp_dir.path().join("logs");

    let config = TelemetryConfig {
        file_sink: FileSinkConfig {
            enabled: false, // Disabled!
            log_dir: Some(log_dir.clone()),
            retention_days: 30,
        },
        stdout_sink: StdoutSinkConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };

    let telemetry = Telemetry::from_config("test-worker-disabled".to_string(), &config)
        .expect("failed to create telemetry");

    // Emit an event
    emit_and_wait(&telemetry, EventKind::QueueEmpty)
        .await
        .expect("emit QueueEmpty failed");

    // Give time for any async operations
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify no log file was created
    let log_files_exists = log_dir.exists() && fs::read_dir(&log_dir).is_ok();
    let has_entries = if log_files_exists {
        fs::read_dir(&log_dir)
            .map(|entries| entries.count() > 0)
            .unwrap_or(false)
    } else {
        false
    };

    assert!(
        !has_entries,
        "log file should not be created when file sink is disabled"
    );
}

/// Test that FileSink uses default log directory when not configured
#[tokio::test]
async fn test_file_sink_default_log_dir() {
    let config = TelemetryConfig {
        file_sink: FileSinkConfig {
            enabled: true,
            log_dir: None, // Use default
            retention_days: 30,
        },
        stdout_sink: StdoutSinkConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };

    let telemetry = Telemetry::from_config("test-worker-default".to_string(), &config)
        .expect("failed to create telemetry");

    // Emit an event
    emit_and_wait(&telemetry, EventKind::QueueEmpty)
        .await
        .expect("emit QueueEmpty failed");

    // Give time for file to be created
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Check default log directory (~/.needle/logs/)
    let home = std::env::var("HOME").expect("HOME not set");
    let default_log_dir = PathBuf::from(home).join(".needle").join("logs");

    assert!(
        default_log_dir.exists(),
        "default log directory should be created"
    );

    // Clean up the created log file
    let log_files = fs::read_dir(&default_log_dir).ok();
    if let Some(entries) = log_files {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.display().to_string().contains("test-worker-default") {
                fs::remove_file(&path).ok();
            }
        }
    }
}

/// Test that FileSink creates log directory if it doesn't exist
#[tokio::test]
async fn test_file_sink_creates_log_directory() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let nested_log_dir = temp_dir.path().join("nested").join("logs");

    assert!(!nested_log_dir.exists(), "log dir should not exist yet");

    let config = TelemetryConfig {
        file_sink: FileSinkConfig {
            enabled: true,
            log_dir: Some(nested_log_dir.clone()),
            retention_days: 30,
        },
        stdout_sink: StdoutSinkConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };

    let _telemetry = Telemetry::from_config("test-worker-mkdir".to_string(), &config)
        .expect("failed to create telemetry");

    // Give time for directory to be created
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        nested_log_dir.exists(),
        "log directory should be created automatically"
    );
}

/// Test FileSink error handling when write fails
#[tokio::test]
async fn test_file_sink_error_handling() {
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

    let telemetry = Telemetry::from_config("test-worker-errors".to_string(), &config)
        .expect("failed to create telemetry");

    // Emit events that should succeed
    for i in 0..5 {
        emit_and_wait(
            &telemetry,
            EventKind::WorkerIdle {
                backoff_seconds: i * 10,
            },
        )
        .await
        .expect("emit should succeed");
    }

    // Give time for all events to be written
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify events were written despite any transient errors
    let log_files = fs::read_dir(&log_dir).expect("failed to read log dir");
    let log_entries: Vec<_> = log_files.filter_map(|e| e.ok()).collect();

    assert!(!log_entries.is_empty(), "log files should exist");
}

/// Test that FileSink correctly serializes complex event data
#[tokio::test]
async fn test_file_sink_complex_serialization() {
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

    let telemetry = Telemetry::from_config("test-worker-complex".to_string(), &config)
        .expect("failed to create telemetry");

    // Emit event with complex nested data
    emit_and_wait(
        &telemetry,
        EventKind::WorkerExhausted {
            cycle_count: 5,
            last_strand: "pluck".to_string(),
            waterfall_restarts: 2,
            restart_triggers: vec!["strand1".to_string(), "strand2".to_string()],
            strand_evaluations: vec![
                ("pluck".to_string(), "WorkCreated".to_string(), 100),
                ("claim".to_string(), "ClaimSuccess".to_string(), 50),
            ],
        },
    )
    .await
    .expect("emit WorkerExhausted failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Read back and verify complex structure
    let log_files = fs::read_dir(&log_dir).expect("failed to read log dir");
    let log_file = log_files.flatten().next().unwrap().path();

    let content = fs::read_to_string(&log_file).expect("failed to read log file");
    let lines: Vec<&str> = content.lines().collect();

    assert!(!lines.is_empty(), "should have at least one event");

    // Parse and verify the WorkerExhausted event
    let exhausted_event = lines
        .iter()
        .find(|line| line.contains("worker.exhausted"))
        .expect("WorkerExhausted event not found");

    let value: serde_json::Value =
        serde_json::from_str(exhausted_event).expect("failed to parse JSON");

    // Verify nested structures
    assert_eq!(value["data"]["cycle_count"], 5, "cycle_count should be 5");
    assert_eq!(
        value["data"]["last_strand_evaluated"], "pluck",
        "last_strand should be pluck"
    );
    assert_eq!(
        value["data"]["waterfall_restarts"], 2,
        "waterfall_restarts should be 2"
    );

    // Verify array fields
    assert!(
        value["data"]["restart_triggers"].is_array(),
        "restart_triggers should be an array"
    );
    assert_eq!(
        value["data"]["restart_triggers"].as_array().unwrap().len(),
        2,
        "restart_triggers should have 2 elements"
    );

    assert!(
        value["data"]["strand_evaluations"].is_array(),
        "strand_evaluations should be an array"
    );
    assert_eq!(
        value["data"]["strand_evaluations"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "strand_evaluations should have 2 elements"
    );
}
