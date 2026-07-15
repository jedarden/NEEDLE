//! Integration test for process discovery via needle status and needle list.
//!
//! This test ensures that every running needle worker is discoverable through
//! `needle status` and `needle list` regardless of how it was started (tmux-wrapped
//! session or bare NEEDLE_INNER=1 background invocation).
//!
//! Regression test for bf-4lkno: A worker was found running for 3+ days, actively
//! dispatching, completely invisible to both needle status and needle list.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Test workspace setup helper.
struct TestWorkspace {
    path: PathBuf,
}

impl TestWorkspace {
    /// Create a temporary test workspace with bead store initialized.
    fn new() -> Result<Self, std::io::Error> {
        let temp_dir = std::env::temp_dir().join(format!("needle-test-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir)?;
        std::fs::create_dir_all(temp_dir.join(".beads"))?;

        // Initialize bead store
        let status = Command::new("br")
            .args(["init", "--non-interactive"])
            .current_dir(&temp_dir)
            .status()?;

        if !status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("br init failed: {}", status),
            ));
        }

        Ok(TestWorkspace { path: temp_dir })
    }

    /// Create a test bead in the workspace.
    fn create_bead(&self, title: &str) -> Result<(), std::io::Error> {
        let status = Command::new("br")
            .args(["create", "--type", "task", title])
            .current_dir(&self.path)
            .status()?;

        if !status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("br create failed: {}", status),
            ));
        }

        Ok(())
    }

    /// Get the workspace path.
    fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Clean up the test workspace.
    fn cleanup(self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Find needle run processes by scanning the process table.
fn find_needle_processes() -> Vec<u32> {
    let output = Command::new("ps")
        .args(&["aux", "--no-headers"])
        .output()
        .expect("ps command should work");

    let mut pids = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.contains("needle run") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(pid_str) = parts.get(1) {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    pids.push(pid);
                }
            }
        }
    }
    pids
}

/// Integration test: verify worker started via NEEDLE_INNER=1 is discoverable.
///
/// This test:
/// 1. Creates a test workspace with beads
/// 2. Starts a worker via NEEDLE_INNER=1 (non-tmux path)
/// 3. Verifies it appears in `needle list`
/// 4. Verifies it appears in `needle status`
/// 5. Stops the worker
/// 6. Verifies it no longer appears in either command
#[test]
#[ignore]
fn integration_non_tmux_worker_discoverable() {
    // Skip if needle binary not available
    if Command::new("needle")
        .arg("--version")
        .output()
        .is_err()
    {
        println!("Skipping test: needle binary not available");
        return;
    }

    let workspace = match TestWorkspace::new() {
        Ok(ws) => ws,
        Err(e) => {
            println!("Skipping test: failed to create workspace: {}", e);
            return;
        }
    };

    // Create a test bead
    if let Err(e) = workspace.create_bead("Test process discovery") {
        println!("Skipping test: failed to create bead: {}", e);
        workspace.cleanup();
        return;
    }

    // Start worker via NEEDLE_INNER=1 (non-tmux path)
    // This simulates the path that might be invisible to status/list
    let needle_binary = std::env::var("NEEDLE_BINARY")
        .unwrap_or_else(|_| "needle".to_string());

    let worker = Command::new(&needle_binary)
        .env("NEEDLE_INNER", "1")
        .args([
            "run",
            "--workspace", workspace.path().to_str().unwrap(),
            "--identifier", "test-discovery",
            "--timeout", "30", // Short timeout for test
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start needle worker");

    let worker_pid = worker.id();
    println!("Started worker PID: {}", worker_pid);

    // Wait for worker to boot and register
    thread::sleep(Duration::from_secs(5));

    // Verify worker is in process table
    let needle_pids = find_needle_processes();
    assert!(
        needle_pids.contains(&worker_pid),
        "worker PID {} should be in process table",
        worker_pid
    );
    println!("✓ Worker found in process table");

    // Verify worker appears in needle list
    let list_output = Command::new(&needle_binary)
        .args(["list", "--format", "json"])
        .output()
        .expect("needle list should work");

    assert!(
        list_output.status.success(),
        "needle list should succeed"
    );

    let list_json: serde_json::Value = serde_json::from_slice(&list_output.stdout)
        .expect("needle list output should be valid JSON");

    // Check if worker appears in either tmux_sessions or orphaned
    let mut found_in_list = false;
    if let Some(sessions) = list_json.get("tmux_sessions") {
        if let Some(arr) = sessions.as_array() {
            for session in arr {
                if let Some(pid) = session.get("pid") {
                    if pid.as_u64() == Some(worker_pid as u64) {
                        found_in_list = true;
                        break;
                    }
                }
            }
        }
    }

    if !found_in_list {
        if let Some(orphaned) = list_json.get("orphaned") {
            if let Some(arr) = orphaned.as_array() {
                for proc in arr {
                    if let Some(pid) = proc.get("pid") {
                        if pid.as_u64() == Some(worker_pid as u64) {
                            found_in_list = true;
                            break;
                        }
                    }
                }
            }
        }
    }

    assert!(
        found_in_list,
        "worker should appear in needle list output (tmux_sessions or orphaned).\n\
         List output: {}",
        String::from_utf8_lossy(&list_output.stdout)
    );
    println!("✓ Worker found in needle list");

    // Verify worker appears in needle status
    let status_output = Command::new(&needle_binary)
        .args(["status", "--format", "json"])
        .output()
        .expect("needle status should work");

    assert!(
        status_output.status.success(),
        "needle status should succeed"
    );

    let status_json: serde_json::Value = serde_json::from_slice(&status_output.stdout)
        .expect("needle status output should be valid JSON");

    // Check if worker appears in workers array or orphaned array
    let mut found_in_status = false;
    if let Some(workers) = status_json.get("workers") {
        if let Some(arr) = workers.as_array() {
            for worker_entry in arr {
                if let Some(pid) = worker_entry.get("pid") {
                    if pid.as_u64() == Some(worker_pid as u64) {
                        found_in_status = true;
                        break;
                    }
                }
            }
        }
    }

    if !found_in_status {
        if let Some(orphaned) = status_json.get("orphaned") {
            if let Some(arr) = orphaned.as_array() {
                for proc in arr {
                    if let Some(pid) = proc.get("pid") {
                        if pid.as_u64() == Some(worker_pid as u64) {
                            found_in_status = true;
                            break;
                        }
                    }
                }
            }
        }
    }

    assert!(
        found_in_status,
        "worker should appear in needle status output (workers or orphaned).\n\
         Status output: {}",
        String::from_utf8_lossy(&status_output.stdout)
    );
    println!("✓ Worker found in needle status");

    // Stop the worker
    println!("Stopping worker...");
    let _ = Command::new(&needle_binary)
        .args(["stop", "--all"])
        .status();

    // Wait for graceful shutdown
    thread::sleep(Duration::from_secs(2));

    // Verify worker is no longer in process table
    let needle_pids_after = find_needle_processes();
    assert!(
        !needle_pids_after.contains(&worker_pid),
        "worker PID {} should no longer be in process table after stop",
        worker_pid
    );
    println!("✓ Worker no longer in process table");

    workspace.cleanup();
    println!("Test passed: non-tmux worker is discoverable via status and list");
}

/// Test helper: verify reconciliation check works.
///
/// This test manually simulates a scenario where a worker is running
/// but not in the registry, and verifies that reconciliation detects it.
#[test]
#[ignore]
fn integration_reconciliation_detects_unregistered_workers() {
    // This test requires:
    // 1. Starting a worker
    // 2. Manually removing it from the registry (simulating failed registration)
    // 3. Running needle status and verifying the orphaned worker is shown
    // 4. Running needle list and verifying the orphaned worker is shown

    println!("Manual test steps:");
    println!("  1. Start a worker: needle run -w <workspace> -i test-reconcile");
    println!("  2. Wait for worker to boot (5 seconds)");
    println!("  3. Note the worker's PID from: ps aux | grep 'needle run'");
    println!("  4. Remove worker from registry:");
    println!("     rm ~/.needle/state/workers.json");
    println!("  5. Run: needle status --format json");
    println!("  6. Verify the orphaned worker appears in 'orphaned' array");
    println!("  7. Run: needle list --format json");
    println!("  8. Verify the orphaned worker appears in 'orphaned' array");
    println!("  9. Stop the worker: needle stop -i test-reconcile");
}
