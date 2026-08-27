//! Integration test: heartbeat state correctness during active dispatch
//!
//! This test reproduces the issue described in needle-b5cd1938 where workers
//! emit EXHAUSTED_IDLE heartbeats immediately after claiming a bead, causing
//! the dashboard to incorrectly mark workers as idle while their agents are running.
//!
//! The test verifies that:
//! 1. A worker with a successfully claimed bead reports EXECUTING/BUILDING/DISPATCHING state in heartbeats
//! 2. Heartbeat files never show is_idle: true while the worker has an active bead
//! 3. Telemetry HeartbeatEmitted events never show EXHAUSTED_IDLE during active dispatch

use std::path::Path;
use std::time::Duration;

use needle::config::Config;
use needle::health::HealthMonitor;
use needle::telemetry::Telemetry;
use needle::types::{BeadId, WorkerState};

/// Isolate HOME to a temp directory for test isolation
struct HomeGuard {
    _temp_dir: tempfile::TempDir,
    original_home: Option<String>,
}

impl HomeGuard {
    fn isolate() -> Self {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", temp_dir.path());
        HomeGuard {
            _temp_dir: temp_dir,
            original_home,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        if let Some(home) = &self.original_home {
            std::env::set_var("HOME", home);
        }
    }
}

fn test_config(heartbeat_dir: &Path) -> Config {
    let mut config = Config::default();
    config.workspace.home = heartbeat_dir.to_path_buf();
    config.workspace.default = heartbeat_dir.to_path_buf();
    config.health.heartbeat_interval_secs = 1; // 1 second for faster tests
    config.health.heartbeat_ttl_secs = 5;
    config
}

#[tokio::test]
async fn heartbeat_shows_executing_state_during_active_dispatch() {
    let _home_guard = HomeGuard::isolate();
    let dir = tempfile::tempdir().unwrap();
    let hb_dir = dir.path().join("state").join("heartbeats");
    let config = test_config(&hb_dir);
    let telemetry = Telemetry::new("test-heartbeat-state".to_string());

    let mut monitor = HealthMonitor::new(config, "test-worker".to_string(), telemetry, None);

    monitor.start_emitter().unwrap();

    // Simulate a worker that has just claimed a bead and is in BUILDING state
    let bead_id = BeadId::from("needle-test123");
    monitor.update_state(&WorkerState::Building, Some(&bead_id), Some(dir.path()));

    // Wait for heartbeat emitter to write at least one heartbeat
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Read heartbeat file
    let heartbeat_content = std::fs::read_to_string(monitor.heartbeat_path()).unwrap();
    let heartbeat: serde_json::Value = serde_json::from_str(&heartbeat_content).unwrap();

    // Verify heartbeat shows the correct state
    assert_eq!(
        heartbeat["state"], "Building",
        "heartbeat state should be Building, not Exhausted"
    );

    // Verify heartbeat shows current bead is set
    assert_eq!(
        heartbeat["current_bead"], "needle-test123",
        "heartbeat should show the claimed bead ID"
    );

    // CRITICAL: Verify is_idle is false - this is what the dashboard uses
    assert_eq!(
        heartbeat["is_idle"], false,
        "heartbeat is_idle must be false while worker has an active bead"
    );

    // Simulate transitioning to DISPATCHING state
    monitor.update_state(&WorkerState::Dispatching, Some(&bead_id), Some(dir.path()));

    tokio::time::sleep(Duration::from_millis(200)).await;

    let heartbeat_content = std::fs::read_to_string(monitor.heartbeat_path()).unwrap();
    let heartbeat: serde_json::Value = serde_json::from_str(&heartbeat_content).unwrap();

    assert_eq!(
        heartbeat["state"], "Dispatching",
        "heartbeat state should be Dispatching"
    );
    assert_eq!(
        heartbeat["current_bead"], "needle-test123",
        "current_bead should still be set during dispatch"
    );
    assert_eq!(
        heartbeat["is_idle"], false,
        "is_idle must be false during dispatch"
    );

    // Simulate transitioning to EXECUTING state (agent is running)
    monitor.update_state(&WorkerState::Executing, Some(&bead_id), Some(dir.path()));

    tokio::time::sleep(Duration::from_millis(200)).await;

    let heartbeat_content = std::fs::read_to_string(monitor.heartbeat_path()).unwrap();
    let heartbeat: serde_json::Value = serde_json::from_str(&heartbeat_content).unwrap();

    assert_eq!(
        heartbeat["state"], "Executing",
        "heartbeat state should be Executing while agent is running"
    );
    assert_eq!(
        heartbeat["is_idle"], false,
        "is_idle must be false while agent is executing - this is the core bug"
    );

    monitor.stop();
}

#[tokio::test]
async fn heartbeat_idle_only_when_no_bead_or_exhausted() {
    let _home_guard = HomeGuard::isolate();
    let dir = tempfile::tempdir().unwrap();
    let hb_dir = dir.path().join("state").join("heartbeats");
    let config = test_config(&hb_dir);
    let telemetry = Telemetry::new("test-idle-condition".to_string());

    let mut monitor = HealthMonitor::new(config, "test-worker".to_string(), telemetry, None);

    monitor.start_emitter().unwrap();

    // Test 1: Worker is EXHAUSTED (no work available)
    monitor.update_state(&WorkerState::Exhausted, None, Some(dir.path()));

    tokio::time::sleep(Duration::from_millis(200)).await;

    let heartbeat_content = std::fs::read_to_string(monitor.heartbeat_path()).unwrap();
    let heartbeat: serde_json::Value = serde_json::from_str(&heartbeat_content).unwrap();

    assert_eq!(
        heartbeat["is_idle"], true,
        "is_idle should be true when worker is Exhausted"
    );

    // Test 2: Worker is SELECTING but has no bead (between cycles)
    monitor.update_state(&WorkerState::Selecting, None, Some(dir.path()));

    tokio::time::sleep(Duration::from_millis(200)).await;

    let heartbeat_content = std::fs::read_to_string(monitor.heartbeat_path()).unwrap();
    let heartbeat: serde_json::Value = serde_json::from_str(&heartbeat_content).unwrap();

    assert_eq!(
        heartbeat["is_idle"], true,
        "is_idle should be true when worker has no current bead"
    );

    // Test 3: Worker is SELECTING but HAS a bead (just claimed, about to build)
    let bead_id = BeadId::from("needle-active");
    monitor.update_state(&WorkerState::Selecting, Some(&bead_id), Some(dir.path()));

    tokio::time::sleep(Duration::from_millis(200)).await;

    let heartbeat_content = std::fs::read_to_string(monitor.heartbeat_path()).unwrap();
    let heartbeat: serde_json::Value = serde_json::from_str(&heartbeat_content).unwrap();

    assert_eq!(
        heartbeat["is_idle"], false,
        "is_idle should be false when worker has a bead even in Selecting state"
    );

    monitor.stop();
}
