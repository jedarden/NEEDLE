//! Integration tests for telemetry query functionality.
//!
//! Tests the `needle query` CLI command and the underlying query infrastructure.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Create a mock telemetry event for testing.
fn create_mock_event(
    worker_id: &str,
    session_id: &str,
    event_type: &str,
    bead_id: Option<&str>,
) -> serde_json::Value {
    let mut event = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event_type": event_type,
        "worker_id": worker_id,
        "session_id": session_id,
        "sequence": 1,
        "bead_id": bead_id,
        "workspace": "/test/workspace",
        "data": {"test": "data"},
        "duration_ms": null,
        "trace_id": null,
        "span_id": null,
    });

    // Set bead_id to null if None
    if bead_id.is_none() {
        event["bead_id"] = serde_json::Value::Null;
    }

    event
}

/// Create a test log file with mock telemetry events.
fn create_test_log(log_dir: &Path, filename: &str, events: &[serde_json::Value]) -> PathBuf {
    let log_path = log_dir.join(filename);
    let mut file = fs::File::create(&log_path).unwrap();

    for event in events {
        let line = serde_json::to_string(event).unwrap();
        writeln!(file, "{}", line).unwrap();
    }

    file.flush().unwrap();
    log_path
}

#[test]
fn test_query_basic_functionality() {
    use needle::telemetry::query::{query_logs, QueryFilter};

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path();

    // Create test log file
    let events = [
        create_mock_event("worker1", "sess001", "worker.started", None),
        create_mock_event("worker1", "sess001", "claim.success", Some("bead-1")),
        create_mock_event("worker2", "sess002", "worker.started", None),
    ];

    create_test_log(log_dir, "worker1-sess001-2024-01-01.jsonl", &events[..2]);
    create_test_log(log_dir, "worker2-sess002-2024-01-01.jsonl", &events[2..]);

    // Query all events
    let filter = QueryFilter::default();
    let result = query_logs(log_dir, &filter).unwrap();

    assert_eq!(result.total_matched, 3);
    assert_eq!(result.events.len(), 3);
    assert_eq!(result.files_scanned, 2);
}

#[test]
fn test_query_worker_filter() {
    use needle::telemetry::query::{query_logs, QueryFilter};

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path();

    // Create test log file with multiple workers
    let events = [
        create_mock_event("worker-alpha", "sess001", "worker.started", None),
        create_mock_event("worker-bravo", "sess002", "worker.started", None),
        create_mock_event("worker-alpha", "sess001", "claim.success", Some("bead-1")),
    ];

    create_test_log(
        log_dir,
        "worker-alpha-sess001-2024-01-01.jsonl",
        &events[..1],
    );
    create_test_log(
        log_dir,
        "worker-bravo-sess002-2024-01-01.jsonl",
        &events[1..2],
    );
    create_test_log(
        log_dir,
        "worker-alpha-sess001-2024-01-01-002.jsonl",
        &events[2..],
    );

    // Query for specific worker
    let filter = QueryFilter {
        worker_id: Some("worker-alpha".to_string()),
        ..Default::default()
    };

    let result = query_logs(log_dir, &filter).unwrap();

    assert_eq!(result.total_matched, 2);
    assert_eq!(result.events.len(), 2);
    assert!(result.events.iter().all(|e| e.worker_id == "worker-alpha"));
}

#[test]
fn test_query_event_type_filter() {
    use needle::telemetry::query::{query_logs, QueryFilter};

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path();

    // Create test log file with different event types
    let events = [
        create_mock_event("worker1", "sess001", "worker.started", None),
        create_mock_event("worker1", "sess001", "claim.success", Some("bead-1")),
        create_mock_event("worker1", "sess001", "claim.failed", Some("bead-2")),
    ];

    create_test_log(log_dir, "worker1-sess001-2024-01-01.jsonl", &events);

    // Query for claim events
    let filter = QueryFilter {
        event_type: Some("claim".to_string()),
        ..Default::default()
    };

    let result = query_logs(log_dir, &filter).unwrap();

    assert_eq!(result.total_matched, 2);
    assert!(result.events.iter().all(|e| e.event_type.contains("claim")));
}

#[test]
fn test_query_bead_id_filter() {
    use needle::telemetry::query::{query_logs, QueryFilter};

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path();

    // Create test log file with different beads
    let events = [
        create_mock_event("worker1", "sess001", "claim.success", Some("bead-1")),
        create_mock_event("worker1", "sess001", "claim.success", Some("bead-2")),
        create_mock_event("worker1", "sess001", "worker.started", None),
    ];

    create_test_log(log_dir, "worker1-sess001-2024-01-01.jsonl", &events);

    // Query for specific bead
    let filter = QueryFilter {
        bead_id: Some("bead-1".to_string()),
        ..Default::default()
    };

    let result = query_logs(log_dir, &filter).unwrap();

    assert_eq!(result.total_matched, 1);
    assert_eq!(
        result.events[0].bead_id.as_ref().map(|b| b.to_string()),
        Some("bead-1".to_string())
    );
}

#[test]
fn test_query_limit() {
    use needle::telemetry::query::{query_logs, QueryFilter};

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path();

    // Create test log file with many events
    let events: Vec<_> = (0..10)
        .map(|i| {
            create_mock_event(
                "worker1",
                "sess001",
                &format!("event.{}", i),
                Some(&format!("bead-{}", i)),
            )
        })
        .collect();

    create_test_log(log_dir, "worker1-sess001-2024-01-01.jsonl", &events);

    // Query with limit
    let filter = QueryFilter {
        limit: Some(5),
        ..Default::default()
    };

    let result = query_logs(log_dir, &filter).unwrap();

    assert_eq!(result.events.len(), 5);
    assert_eq!(result.total_matched, 10); // Total before limit
}

#[test]
fn test_compute_stats_basic() {
    use needle::telemetry::query::compute_stats;

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path();

    // Create test log files
    let events1 = [
        create_mock_event("worker1", "sess001", "worker.started", None),
        create_mock_event("worker1", "sess001", "claim.success", Some("bead-1")),
        create_mock_event("worker1", "sess001", "claim.success", Some("bead-2")),
    ];

    let events2 = vec![
        create_mock_event("worker2", "sess002", "worker.started", None),
        create_mock_event("worker2", "sess002", "dispatch.started", Some("bead-3")),
    ];

    create_test_log(log_dir, "worker1-sess001-2024-01-01.jsonl", &events1);
    create_test_log(log_dir, "worker2-sess002-2024-01-01.jsonl", &events2);

    let stats = compute_stats(log_dir, None).unwrap();

    assert_eq!(stats.total_workers, 2);
    assert_eq!(stats.total_events, 5);
    assert_eq!(stats.workers.len(), 2);

    // Check worker1 stats (find it by ID since order is not guaranteed)
    let worker1 = stats
        .workers
        .iter()
        .find(|w| w.worker_id == "worker1")
        .expect("worker1 should be in stats");
    assert_eq!(worker1.event_count, 3);
    assert_eq!(worker1.beads_processed.len(), 2);
    assert_eq!(worker1.event_types.get("claim.success"), Some(&2));
}

#[test]
fn test_compute_stats_worker_filter() {
    use needle::telemetry::query::compute_stats;

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path();

    // Create test log files
    let events1 = [
        create_mock_event("worker-alpha", "sess001", "worker.started", None),
        create_mock_event("worker-bravo", "sess002", "worker.started", None),
    ];

    create_test_log(
        log_dir,
        "worker-alpha-sess001-2024-01-01.jsonl",
        &events1[..1],
    );
    create_test_log(
        log_dir,
        "worker-bravo-sess002-2024-01-01.jsonl",
        &events1[1..],
    );

    let stats = compute_stats(log_dir, Some("alpha")).unwrap();

    assert_eq!(stats.total_workers, 1);
    assert_eq!(stats.workers[0].worker_id, "worker-alpha");
}

#[test]
fn test_discover_log_files() {
    use needle::telemetry::query::discover_log_files;

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path();

    // Create test log files
    let log1 = log_dir.join("worker1-sess001-2024-01-01.jsonl");
    let log2 = log_dir.join("worker2-sess002-2024-01-01.jsonl");
    let not_json = log_dir.join("readme.txt");
    let subdir = log_dir.join("subdir");

    fs::create_dir(&subdir).unwrap();
    fs::write(&log1, "test").unwrap();
    fs::write(&log2, "test").unwrap();
    fs::write(&not_json, "test").unwrap();
    fs::write(subdir.join("nested.jsonl"), "test").unwrap();

    let files = discover_log_files(log_dir).unwrap();

    assert_eq!(files.len(), 2);
    assert!(files.contains(&log1));
    assert!(files.contains(&log2));
    assert!(!files.contains(&not_json));
}

#[test]
fn test_query_empty_directory() {
    use needle::telemetry::query::{query_logs, QueryFilter};

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path();

    // No log files created
    let filter = QueryFilter::default();
    let result = query_logs(log_dir, &filter).unwrap();

    assert_eq!(result.events.len(), 0);
    assert_eq!(result.files_scanned, 0);
}

#[test]
fn test_compute_stats_empty_directory() {
    use needle::telemetry::query::compute_stats;

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path();

    let stats = compute_stats(log_dir, None).unwrap();

    assert_eq!(stats.total_workers, 0);
    assert_eq!(stats.total_events, 0);
    assert!(stats.workers.is_empty());
}

#[test]
fn test_query_nonexistent_directory() {
    use needle::telemetry::query::{query_logs, QueryFilter};

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path().join("nonexistent");

    let filter = QueryFilter::default();
    let result = query_logs(&log_dir, &filter).unwrap();

    assert_eq!(result.events.len(), 0);
    assert_eq!(result.files_scanned, 0);
}

#[test]
fn test_query_malformed_json_handling() {
    use needle::telemetry::query::{query_logs, QueryFilter};

    let temp_dir = TempDir::new().unwrap();
    let log_dir = temp_dir.path();

    // Create test log file with malformed JSON
    let log_path = log_dir.join("worker1-sess001-2024-01-01.jsonl");
    let mut file = fs::File::create(&log_path).unwrap();

    writeln!(
        file,
        "{}",
        serde_json::to_string(&create_mock_event(
            "worker1",
            "sess001",
            "worker.started",
            None
        ))
        .unwrap()
    )
    .unwrap();
    writeln!(file, "invalid json").unwrap();
    writeln!(file, "{{ broken").unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&create_mock_event(
            "worker1",
            "sess001",
            "claim.success",
            Some("bead-1")
        ))
        .unwrap()
    )
    .unwrap();

    file.flush().unwrap();

    let filter = QueryFilter::default();
    let result = query_logs(log_dir, &filter).unwrap();

    // Should only parse valid events
    assert_eq!(result.events.len(), 2);
}
