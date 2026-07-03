//! Heartbeat validation test - verifies heartbeat file creation and refresh.
//!
//! This test validates that:
//! 1. Workers create heartbeat file on startup
//! 2. File contains worker ID and last refresh timestamp
//! 3. File updates every ~heartbeat_interval_secs (30s by default)

use std::path::Path;
use std::time::Duration;

#[tokio::test]
async fn heartbeat_file_created_on_startup() {
    let dir = tempfile::tempdir().unwrap();
    let hb_dir = dir.path().join("state").join("heartbeats");
    let config = test_config(&hb_dir);

    let mut monitor = needle::health::HealthMonitor::new(
        config,
        "validation-worker".to_string(),
        needle::telemetry::Telemetry::new("test".to_string()),
        None,
    );

    // Start the emitter - should create heartbeat file immediately
    monitor.start_emitter().unwrap();

    // Verify heartbeat file exists
    let path = monitor.heartbeat_path();
    assert!(
        path.exists(),
        "heartbeat file should exist immediately after start_emitter()"
    );

    // Verify file contains required fields
    let content = std::fs::read_to_string(&path).unwrap();
    let data: serde_json::Value = serde_json::from_str(&content).unwrap();

    // Check worker_id field
    assert_eq!(
        data["worker_id"], "validation-worker",
        "heartbeat file should contain worker_id"
    );

    // Check qualified_id field
    assert!(
        data["qualified_id"].is_string(),
        "heartbeat file should contain qualified_id"
    );

    // Check last_heartbeat field (timestamp)
    assert!(
        data["last_heartbeat"].is_string(),
        "heartbeat file should contain last_heartbeat timestamp"
    );

    // Verify timestamp is recent (within last 5 seconds)
    let last_heartbeat = data["last_heartbeat"].as_str().unwrap();
    let timestamp = chrono::DateTime::parse_from_rfc3339(last_heartbeat).unwrap();
    let age = chrono::Utc::now() - timestamp.with_timezone(&chrono::Utc);
    assert!(
        age.num_seconds() < 5,
        "last_heartbeat timestamp should be recent (within 5 seconds)"
    );

    monitor.stop();
}

#[tokio::test]
async fn heartbeat_refreshes_every_30_seconds() {
    let dir = tempfile::tempdir().unwrap();

    let mut config = needle::config::Config::default();
    config.workspace.home = dir.path().to_path_buf();
    config.health.heartbeat_interval_secs = 2; // Use 2s for faster test
    config.health.heartbeat_ttl_secs = 10;

    let mut monitor = needle::health::HealthMonitor::new(
        config,
        "refresh-test-worker".to_string(),
        needle::telemetry::Telemetry::new("test".to_string()),
        None,
    );

    monitor.start_emitter().unwrap();
    let path = monitor.heartbeat_path();

    // Read initial timestamp
    let content1 = std::fs::read_to_string(&path).unwrap();
    let data1: serde_json::Value = serde_json::from_str(&content1).unwrap();
    let timestamp1 = data1["last_heartbeat"].as_str().unwrap();
    let time1 = chrono::DateTime::parse_from_rfc3339(timestamp1).unwrap();

    // Wait for heartbeat to refresh (2.5 seconds to account for interval + write time)
    std::thread::sleep(Duration::from_millis(2500));

    // Read updated timestamp
    let content2 = std::fs::read_to_string(&path).unwrap();
    let data2: serde_json::Value = serde_json::from_str(&content2).unwrap();
    let timestamp2 = data2["last_heartbeat"].as_str().unwrap();
    let time2 = chrono::DateTime::parse_from_rfc3339(timestamp2).unwrap();

    // Verify timestamp has been updated
    assert!(
        time2 > time1,
        "last_heartbeat timestamp should be updated after interval"
    );

    // Verify update happened within expected time window (1-3 seconds)
    let update_interval = (time2 - time1).num_seconds();
    assert!(
        (1..=3).contains(&update_interval),
        "heartbeat should update every ~2 seconds (interval), got: {} seconds",
        update_interval
    );

    monitor.stop();
}

#[tokio::test]
async fn heartbeat_contains_required_fields() {
    let dir = tempfile::tempdir().unwrap();
    let hb_dir = dir.path().join("state").join("heartbeats");
    let config = test_config(&hb_dir);

    let mut monitor = needle::health::HealthMonitor::new(
        config,
        "fields-test-worker".to_string(),
        needle::telemetry::Telemetry::new("test".to_string()),
        None,
    );

    monitor.start_emitter().unwrap();

    let content = std::fs::read_to_string(monitor.heartbeat_path()).unwrap();
    let data: needle::health::HeartbeatData = serde_json::from_str(&content).unwrap();

    // Verify all required fields are present
    assert!(!data.worker_id.is_empty(), "worker_id should not be empty");
    assert!(
        !data.qualified_id.is_empty(),
        "qualified_id should not be empty"
    );
    assert!(data.pid > 0, "pid should be set");

    // Verify timestamp is valid
    let age = chrono::Utc::now()
        .signed_duration_since(data.last_heartbeat)
        .num_seconds();
    assert!((0..5).contains(&age), "last_heartbeat should be recent");

    monitor.stop();
}

fn test_config(heartbeat_dir: &Path) -> needle::config::Config {
    let mut config = needle::config::Config::default();
    config.workspace.home = heartbeat_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    config.health.heartbeat_interval_secs = 1;
    config.health.heartbeat_ttl_secs = 5;
    config
}
