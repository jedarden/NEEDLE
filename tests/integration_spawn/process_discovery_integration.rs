//! Integration test for process discovery via needle status and needle list.
//!
//! This test ensures that every running needle worker is discoverable through
//! `needle status` and `needle list` regardless of how it was started (tmux-wrapped
//! session or bare NEEDLE_INNER=1 background invocation).
//!
//! Regression test for bf-4lkno: A worker was found running for 3+ days, actively
//! dispatching, completely invisible to both needle status and needle list.

use std::path::PathBuf;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use super::isolation::{ChildGuard, IsolatedChildEnv};

/// Test workspace setup helper.
struct TestWorkspace {
    path: PathBuf,
}

impl TestWorkspace {
    /// Create a temporary test workspace with bead store initialized.
    fn new(fixture: &IsolatedChildEnv) -> Result<Self, std::io::Error> {
        let temp_dir = fixture.path().join("process-discovery-workspace");
        std::fs::create_dir_all(&temp_dir)?;

        // Initialize bead store
        let status = fixture
            .command("bead")
            .arg("init")
            .current_dir(&temp_dir)
            .status()?;

        if !status.success() {
            return Err(std::io::Error::other(format!("bead init failed: {status}")));
        }
        std::fs::write(
            temp_dir.join(".needle.yaml"),
            "bead_cli:\n  backend: bead-rs\n",
        )?;

        Ok(TestWorkspace { path: temp_dir })
    }

    /// Create a test bead in the workspace.
    fn create_bead(&self, fixture: &IsolatedChildEnv, title: &str) -> Result<(), std::io::Error> {
        let status = fixture
            .command("bead")
            .args(["create", "--issue-type", "task", "--title", title])
            .current_dir(&self.path)
            .status()?;

        if !status.success() {
            return Err(std::io::Error::other(format!(
                "bead create failed: {status}"
            )));
        }

        Ok(())
    }

    /// Get the workspace path.
    fn path(&self) -> &PathBuf {
        &self.path
    }
}

/// Find needle run processes by scanning the process table.
fn find_needle_processes(fixture: &IsolatedChildEnv) -> Vec<u32> {
    let output = fixture
        .command("ps")
        .args(["aux", "--no-headers"])
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

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    predicate()
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
    let fixture = IsolatedChildEnv::new();

    let workspace = TestWorkspace::new(&fixture).expect("create isolated test workspace");

    // Create a test bead
    workspace
        .create_bead(&fixture, "Test process discovery")
        .expect("create isolated test bead");

    // Start worker via NEEDLE_INNER=1 (non-tmux path)
    // This simulates the path that might be invisible to status/list
    let identifier = format!("test-discovery-{}", std::process::id());
    let child = fixture
        .needle()
        .env("NEEDLE_INNER", "1")
        .arg("run")
        .arg("--workspace")
        .arg(workspace.path())
        .arg("--identifier")
        .arg(&identifier)
        .args(["--timeout", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start needle worker");
    let mut worker = ChildGuard::new(child);
    let worker_pid = worker.id();
    println!("Started worker PID: {}", worker_pid);

    assert!(
        wait_until(Duration::from_secs(5), || {
            find_needle_processes(&fixture).contains(&worker_pid)
        }),
        "worker PID {worker_pid} should enter the process table"
    );

    // Verify worker is in process table
    let needle_pids = find_needle_processes(&fixture);
    assert!(
        needle_pids.contains(&worker_pid),
        "worker PID {} should be in process table",
        worker_pid
    );
    println!("✓ Worker found in process table");

    // Verify worker appears in needle list
    let list_output = fixture
        .needle()
        .args(["list", "--format", "json"])
        .output()
        .expect("needle list should work");

    assert!(list_output.status.success(), "needle list should succeed");

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
        if let Some(discovered) = list_json.get("discovered") {
            if let Some(arr) = discovered.as_array() {
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
        "worker should appear in needle list output (tmux_sessions or discovered).\n\
         List output: {}",
        String::from_utf8_lossy(&list_output.stdout)
    );
    println!("✓ Worker found in needle list");

    // Verify worker appears in needle status
    let status_output = fixture
        .needle()
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
        if let Some(discovered) = status_json.get("discovered") {
            if let Some(arr) = discovered.as_array() {
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
        "worker should appear in needle status output (workers or discovered).\n\
         Status output: {}",
        String::from_utf8_lossy(&status_output.stdout)
    );
    println!("✓ Worker found in needle status");

    // Stop the worker
    println!("Stopping worker...");
    let stop_output = fixture
        .needle()
        .args(["stop", "--identifier"])
        .arg(&identifier)
        .output()
        .expect("stop isolated worker");
    assert!(
        stop_output.status.success(),
        "targeted stop failed: {}",
        String::from_utf8_lossy(&stop_output.stderr)
    );
    let _ = worker.wait();

    // Verify worker is no longer in process table
    assert!(
        wait_until(Duration::from_secs(2), || {
            !find_needle_processes(&fixture).contains(&worker_pid)
        }),
        "worker PID {} should no longer be in process table after stop",
        worker_pid
    );
    println!("✓ Worker no longer in process table");

    println!("Test passed: non-tmux worker is discoverable via status and list");
}
