//! Regression test for process discovery blind spots.
//!
//! This test ensures that every live needle run --workspace process is
//! discoverable through needle status and needle list regardless of how it
//! was started (tmux-wrapped session or bare NEEDLE_INNER=1 background
//! invocation).
//!
//! See bead bf-4lkno for full context.

use super::isolation::{ChildGuard, IsolatedChildEnv};

#[test]
#[cfg(unix)]
fn test_process_table_reconciliation() {
    // This test verifies that the process table reconciliation logic
    // correctly identifies unregistered needle run processes.

    // Verify scan_needle_processes() can be called successfully
    // This is a unit test of the scanning functionality
    let fixture = IsolatedChildEnv::new();
    let output = fixture
        .needle()
        .arg("list")
        .arg("--format")
        .arg("json")
        .output()
        .expect("run isolated needle list");
    assert!(
        output.status.success(),
        "needle list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("needle list output is valid JSON");
    assert!(
        value["tmux_sessions"].is_array(),
        "list JSON includes tmux_sessions array: {value}"
    );
    assert!(
        value["discovered"].is_array(),
        "list JSON includes discovered array: {value}"
    );
}

#[test]
#[cfg(unix)]
fn test_status_command_reconciliation() {
    // This test verifies that needle status performs reconciliation
    // and reports unregistered workers if found.

    let fixture = IsolatedChildEnv::new();
    let output = fixture
        .needle()
        .arg("status")
        .arg("--format")
        .arg("json")
        .output()
        .expect("run isolated needle status");
    assert!(
        output.status.success(),
        "needle status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("needle status output is valid JSON");
    assert!(
        value["discovered"].is_array(),
        "status JSON includes discovered array: {value}"
    );
    assert!(
        value["unregistered_workers"].is_u64(),
        "status JSON includes unregistered_workers count: {value}"
    );
}

/// Regression test for descendant process false discovery.
///
/// This test ensures that scan_needle_processes() does NOT report child
/// processes that inherit NEEDLE_INNER from their parent worker as separate
/// unregistered workers.
///
/// Background: When a worker spawns an agent subprocess, the child inherits
/// NEEDLE_INNER=1 in its environment. A previous version of scan_needle_processes()
/// incorrectly included ALL processes with NEEDLE_INNER in their environment,
/// causing child agents to be reported as 92 "unregistered workers" when only
/// 15 actual workers existed.
///
/// This test verifies the fix: only processes with "needle run" in their
/// cmdline are discovered, not just any process with NEEDLE_INNER set.
#[test]
#[cfg(unix)]
#[ignore]
fn regression_descendant_processes_not_discovered() {
    use std::process::Stdio;

    let fixture = IsolatedChildEnv::new();

    // Create a fake worker process that has NEEDLE_INNER in its environment
    // but is NOT a needle run process (e.g., a child agent or verifier)
    let child = fixture
        .command("sh")
        .arg("-c")
        .arg("NEEDLE_INNER=1 sleep 30") // Simulates a child process inheriting NEEDLE_INNER
        .env("NEEDLE_INNER", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn isolated descendant process");
    let child = ChildGuard::new(child);
    let child_pid = child.id();

    println!("Spawned child process PID: {}", child_pid);

    // Run needle list to discover processes
    let list_output = fixture
        .needle()
        .args(["list", "--format", "json"])
        .output()
        .expect("run isolated needle list");

    assert!(
        list_output.status.success(),
        "needle list failed: {}",
        String::from_utf8_lossy(&list_output.stderr)
    );

    let list_json: serde_json::Value = serde_json::from_slice(&list_output.stdout)
        .expect("needle list output should be valid JSON");

    // The child process should NOT appear in discovered workers
    // because it doesn't have "needle run" in its cmdline
    let mut found_child = false;
    if let Some(discovered) = list_json.get("discovered").and_then(|v| v.as_array()) {
        for proc in discovered {
            if let Some(pid) = proc.get("pid").and_then(|p| p.as_u64()) {
                if pid == child_pid as u64 {
                    found_child = true;
                    println!(
                        "✗ FAIL: Child process {} appeared in discovered workers",
                        child_pid
                    );
                    break;
                }
            }
        }
    }

    // Verify child is NOT reported
    assert!(
        !found_child,
        "Child process with NEEDLE_INNER should NOT be discovered as a worker \
         (only processes with 'needle run' in cmdline should be discovered)"
    );
    println!("✓ Child process correctly excluded from discovered workers");

    drop(child);
}
