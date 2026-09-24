//! Regression test for process discovery blind spots.
//!
//! This test ensures that every live needle run --workspace process is
//! discoverable through needle status and needle list regardless of how it
//! was started (tmux-wrapped session or bare NEEDLE_INNER=1 background
//! invocation).
//!
//! See bead bf-4lkno for full context.

use super::isolation::{ChildGuard, IsolatedChildEnv};
use std::collections::BTreeSet;
use std::process::Stdio;

fn run_list_json(fixture: &IsolatedChildEnv) -> serde_json::Value {
    let output = fixture
        .needle()
        .args(["list", "--format", "json"])
        .output()
        .expect("run isolated needle list");
    assert!(
        output.status.success(),
        "needle list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("needle list output is valid JSON")
}

fn run_status_json(fixture: &IsolatedChildEnv) -> serde_json::Value {
    let output = fixture
        .needle()
        .args(["status", "--format", "json"])
        .output()
        .expect("run isolated needle status");
    assert!(
        output.status.success(),
        "needle status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("needle status output is valid JSON")
}

fn write_stale_registry(fixture: &IsolatedChildEnv) {
    let state_dir = fixture.path().join(".needle/state");
    std::fs::create_dir_all(&state_dir).expect("create isolated registry directory");
    let registry = serde_json::json!({
        "workers": [{
            "id": "stale-worker",
            "pid": u32::MAX,
            "workspace": fixture.path(),
            "agent": "stale-agent",
            "model": null,
            "provider": null,
            "started_at": "2026-09-24T00:00:00Z",
            "beads_processed": 4,
            "beads_completed": 2,
            "config_reload_generation": 0,
            "state": null
        }],
        "updated_at": "2026-09-24T00:00:00Z"
    });
    std::fs::write(
        state_dir.join("workers.json"),
        serde_json::to_vec_pretty(&registry).expect("serialize stale registry fixture"),
    )
    .expect("write stale registry fixture");
}

fn spawn_fake_worker(fixture: &IsolatedChildEnv, identifier: &str) -> ChildGuard {
    // A copied shell has the exact argv shape of a worker process while its
    // script blocks on stdin. This exercises the real process-table scanner
    // without needing a bead store or a second worker lifecycle.
    let fake_needle = fixture.path().join("needle");
    let shell = which::which("sh").expect("sh executable is available");
    std::fs::copy(shell, &fake_needle).expect("copy process fixture");
    std::fs::write(fixture.path().join("run"), "read ignored\n")
        .expect("write blocking process fixture");
    let child = fixture
        .command(&fake_needle)
        .args(["run", "--workspace"])
        .arg(fixture.path())
        .args(["--agent", "process-test-agent", "--identifier", identifier])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn process fixture");
    ChildGuard::new(child)
}

fn assert_exact_list_object_shape(value: &serde_json::Value) {
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("list JSON must always be an object: {value}"));
    let keys = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    assert_eq!(
        keys,
        BTreeSet::from([
            "discovered",
            "stale_registrations",
            "stale_sessions",
            "tmux_sessions",
            "unregistered_workers",
        ]),
        "list JSON has one stable top-level schema: {value}"
    );
    assert!(value["tmux_sessions"].is_array());
    assert!(value["discovered"].is_array());
    // Reported since 1bbbb724: tmux sessions whose worker process is gone.
    assert!(value["stale_sessions"].is_array());
}

#[test]
#[cfg(unix)]
fn test_process_table_reconciliation() {
    // This test verifies that the process table reconciliation logic
    // correctly identifies unregistered needle run processes.

    // Verify scan_needle_processes() can be called successfully
    // This is a unit test of the scanning functionality
    let fixture = IsolatedChildEnv::new();
    let empty_or_host_populated = run_list_json(&fixture);
    assert_exact_list_object_shape(&empty_or_host_populated);
    if empty_or_host_populated["tmux_sessions"]
        .as_array()
        .is_some_and(Vec::is_empty)
        && empty_or_host_populated["discovered"]
            .as_array()
            .is_some_and(Vec::is_empty)
    {
        assert_eq!(
            empty_or_host_populated,
            serde_json::json!({
                "tmux_sessions": [],
                "discovered": [],
                "stale_sessions": [],
                "stale_registrations": [],
                "unregistered_workers": [],
            }),
            "an empty fleet still emits the complete object schema"
        );
    }

    // Create a long-lived process with exactly the argv shape the production
    // scanner recognizes. A copied shell reads the local `run` script and
    // blocks on its piped stdin, so no Worker or bead store is needed and
    // ChildGuard still owns deterministic cleanup.
    let fake_needle = fixture.path().join("needle");
    let shell = which::which("sh").expect("sh executable is available");
    std::fs::copy(shell, &fake_needle).expect("copy process fixture");
    std::fs::write(fixture.path().join("run"), "read ignored\n")
        .expect("write blocking process fixture");
    let child = fixture
        .command(&fake_needle)
        .args(["run", "--workspace"])
        .arg(fixture.path())
        .args([
            "--agent",
            "schema-test-agent",
            "--identifier",
            "schema-test-worker",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn populated-list process fixture");
    let child = ChildGuard::new(child);

    let populated = (0..100)
        .find_map(|_| {
            let value = run_list_json(&fixture);
            assert_exact_list_object_shape(&value);
            let entry = value["discovered"]
                .as_array()
                .and_then(|entries| entries.iter().find(|entry| entry["pid"] == child.id()))
                .cloned();
            if entry.is_none() {
                std::thread::yield_now();
            }
            entry
        })
        .unwrap_or_else(|| panic!("fixture PID {} was not discovered", child.id()));
    let populated_keys = populated
        .as_object()
        .expect("discovered entry is an object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        populated_keys,
        BTreeSet::from([
            "agent",
            "cmdline",
            "identifier",
            "in_tmux",
            "pid",
            "workspace",
        ]),
        "populated entries keep their exact schema"
    );
    assert_eq!(populated["pid"], child.id());
    assert_eq!(populated["workspace"], fixture.path().display().to_string());
    assert_eq!(populated["agent"], "schema-test-agent");
    assert_eq!(populated["identifier"], "schema-test-worker");
    assert_eq!(populated["in_tmux"], false);
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

#[test]
#[cfg(unix)]
fn live_unregistered_worker_and_stale_registration_are_reconciled() {
    let fixture = IsolatedChildEnv::new();
    write_stale_registry(&fixture);

    let worker = spawn_fake_worker(&fixture, "live-unregistered-worker");
    let worker_pid = worker.id();

    let mut list = run_list_json(&fixture);
    for _ in 0..100 {
        let found = list["discovered"]
            .as_array()
            .is_some_and(|workers| workers.iter().any(|entry| entry["pid"] == worker_pid));
        if found {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        list = run_list_json(&fixture);
    }

    assert!(
        list["discovered"]
            .as_array()
            .is_some_and(|workers| workers.iter().any(|entry| entry["pid"] == worker_pid)),
        "live process-table worker must be visible in list: {list}"
    );
    assert!(
        list["unregistered_workers"]
            .as_array()
            .is_some_and(|workers| workers.iter().any(|entry| entry["pid"] == worker_pid)),
        "worker without registry metadata must be identified by list: {list}"
    );
    assert!(
        list["stale_registrations"]
            .as_array()
            .is_some_and(|entries| { entries.iter().any(|entry| entry["id"] == "stale-worker") }),
        "list must identify registry entries with no live process: {list}"
    );

    let status = run_status_json(&fixture);
    assert!(
        status["discovered"]
            .as_array()
            .is_some_and(|workers| workers.iter().any(|entry| entry["pid"] == worker_pid)),
        "live process-table worker must be visible in status: {status}"
    );
    assert!(
        status["unregistered_workers"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "status must report an unregistered live worker: {status}"
    );
    assert!(
        status["stale_registrations"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "status must report the stale registration: {status}"
    );
    assert!(
        status["stale_registration_details"]
            .as_array()
            .is_some_and(|entries| { entries.iter().any(|entry| entry["id"] == "stale-worker") }),
        "status must expose stale registration details: {status}"
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
