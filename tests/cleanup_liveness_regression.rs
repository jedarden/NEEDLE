//! Regression tests for needle cleanup liveness detection.
//!
//! These tests ensure that the cleanup command properly checks for live processes
//! before removing tmux sessions, preventing a repeat of the 2026-07-19 incident
//! where bare `needle cleanup` killed live sessions (armor-p6a, needle-supervisor).
//!
//! See ADR-003 and plan.md Phase 7.2 for full context.
//!
//! NOTE: The authoritative regression tests for cleanup liveness detection are the
//! unit tests in `src/cli/mod.rs`:
//! - `cleanup_no_flags_filters_orphaned_sessions`
//! - `cleanup_no_flags_with_zero_dead_removes_nothing`
//! - `cleanup_all_removes_every_session_regardless_of_liveness`
//!
//! These integration tests are disabled because they require creating real `needle run`
//! processes with proper session registration, which is complex for a test environment.
//! The unit tests use mocked data and are sufficient to pin the three required behaviors.

#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use clap::Parser;

/// Test helper to check if a PID exists in the process table.
#[cfg(unix)]
#[allow(dead_code)]
fn pid_exists(pid: u32) -> bool {
    unsafe {
        let ret = libc::kill(pid as libc::pid_t, 0);
        if ret == 0 {
            return true; // Process exists and we have permission
        }
        // ESRCH (3) means no such process
        std::io::Error::last_os_error().raw_os_error() != Some(3)
    }
}

/// Test helper to find all needle tmux sessions.
#[cfg(unix)]
fn list_needle_sessions() -> Vec<String> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|line| line.starts_with("needle-"))
            .map(|s| s.to_string())
            .collect(),
        _ => vec![],
    }
}

/// Test helper to create a test tmux session with a fake PID.
///
/// This creates a tmux session that appears to be a needle session but has
/// a PID that doesn't correspond to a live process.
#[cfg(unix)]
fn create_orphaned_session(session_name: &str) -> Result<(), std::io::Error> {
    // Create a new tmux session
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", session_name, "sleep", "3600"])
        .status()?;

    if !status.success() {
        return Err(std::io::Error::other(format!(
            "failed to create tmux session: {}",
            status
        )));
    }

    Ok(())
}

/// Test helper to create a test tmux session with a live process.
#[cfg(unix)]
fn create_live_session(session_name: &str) -> Result<std::process::Child, std::io::Error> {
    // Start a long-running needle process in a tmux session
    let child = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            session_name,
            "sleep",
            "3600", // Simulates a long-running process
        ])
        .spawn()?;

    // Give the session a moment to start
    thread::sleep(Duration::from_millis(100));

    Ok(child)
}

/// Test helper to kill a tmux session.
#[cfg(unix)]
fn kill_session(session_name: &str) -> Result<(), std::io::Error> {
    let status = Command::new("tmux")
        .args(["kill-session", "-t", session_name])
        .status()?;

    if !status.success() {
        return Err(std::io::Error::other(format!(
            "failed to kill tmux session: {}",
            status
        )));
    }

    Ok(())
}

/// Regression Test 1: needle cleanup with no flags removes only dead sessions.
///
/// Test: `needle cleanup` with no flags, given one live session and one session
/// with no backing process, removes only the dead one.
///
/// This is the core safety fix: the no-flags path must check process liveness
/// before killing sessions. It should preserve live sessions while cleaning
/// up orphans.
///
/// NOTE: This integration test is disabled because it requires creating real
/// `needle run` processes, which is complex for a test environment. The unit
/// tests in `src/cli/mod.rs` already cover this scenario with mocked data and
/// are the authoritative regression tests for this behavior.
#[test]
#[cfg(unix)]
#[ignore = "Requires real needle run processes - covered by unit tests in src/cli/mod.rs"]
fn regression_cleanup_no_flags_removes_only_dead_sessions() {
    // Skip if needle binary not available
    if Command::new("needle").arg("--version").output().is_err() {
        println!("Skipping test: needle binary not available");
        return;
    }

    // Skip if tmux not available
    if Command::new("tmux").arg("-V").output().is_err() {
        println!("Skipping test: tmux not available");
        return;
    }

    let live_session = "needle-test-cleanup-live";
    let orphan_session = "needle-test-cleanup-orphan";

    // Clean up any existing test sessions
    let _ = kill_session(live_session);
    let _ = kill_session(orphan_session);
    thread::sleep(Duration::from_millis(100));

    // Create one live session (with backing process)
    let _live_child = match create_live_session(live_session) {
        Ok(child) => child,
        Err(e) => {
            println!("Skipping test: failed to create live session: {}", e);
            return;
        }
    };

    // Create one orphaned session (no backing needle process)
    if let Err(e) = create_orphaned_session(orphan_session) {
        println!("Skipping test: failed to create orphaned session: {}", e);
        let _ = kill_session(live_session);
        return;
    }

    // Give sessions a moment to stabilize
    thread::sleep(Duration::from_millis(200));

    // Verify both sessions exist before cleanup
    let sessions_before = list_needle_sessions();
    assert!(
        sessions_before.iter().any(|s| s.contains(live_session)),
        "live session should exist before cleanup"
    );
    assert!(
        sessions_before.iter().any(|s| s.contains(orphan_session)),
        "orphaned session should exist before cleanup"
    );

    // Run bare needle cleanup (no --all, no -i)
    let needle_binary =
        std::env::var("CARGO_BIN_EXE_needle").unwrap_or_else(|_| "needle".to_string());

    let output = Command::new(&needle_binary)
        .arg("cleanup")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(result) => {
            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);

            // The command should succeed
            assert!(
                result.status.success(),
                "needle cleanup should succeed: stdout: {}, stderr: {}",
                stdout,
                stderr
            );

            // Verify orphaned session was removed
            let sessions_after = list_needle_sessions();
            assert!(
                !sessions_after.iter().any(|s| s.contains(orphan_session)),
                "orphaned session should be removed after cleanup"
            );

            // Verify live session was preserved
            assert!(
                sessions_after.iter().any(|s| s.contains(live_session)),
                "live session should still exist after cleanup"
            );

            println!("Test passed: needle cleanup removed only the orphaned session");
        }
        Err(e) => {
            panic!("needle cleanup command failed: {}", e);
        }
    }

    // Clean up the live session
    let _ = kill_session(live_session);
}

/// Regression Test 2: needle cleanup with no flags and zero dead sessions removes nothing.
///
/// Test: `needle cleanup` with no flags and zero dead sessions removes nothing
/// and reports that, even when live sessions exist.
///
/// This is the exact scenario that killed armor-p6a and needle-supervisor on
/// 2026-07-19: a fleet with only live workers should have zero sessions removed
/// by bare cleanup, with clear messaging.
///
/// NOTE: This integration test is disabled because it requires creating real
/// `needle run` processes, which is complex for a test environment. The unit
/// tests in `src/cli/mod.rs` already cover this scenario with mocked data and
/// are the authoritative regression tests for this behavior.
#[test]
#[cfg(unix)]
#[ignore = "Requires real needle run processes - covered by unit tests in src/cli/mod.rs"]
fn regression_cleanup_no_flags_with_only_live_sessions_removes_nothing() {
    // Skip if needle binary not available
    if Command::new("needle").arg("--version").output().is_err() {
        println!("Skipping test: needle binary not available");
        return;
    }

    // Skip if tmux not available
    if Command::new("tmux").arg("-V").output().is_err() {
        println!("Skipping test: tmux not available");
        return;
    }

    let live_session1 = "needle-test-cleanup-live1";
    let live_session2 = "needle-test-cleanup-live2";

    // Clean up any existing test sessions
    let _ = kill_session(live_session1);
    let _ = kill_session(live_session2);
    thread::sleep(Duration::from_millis(100));

    // Create two live sessions
    let _live_child1 = match create_live_session(live_session1) {
        Ok(child) => child,
        Err(e) => {
            println!("Skipping test: failed to create live session 1: {}", e);
            return;
        }
    };

    let _live_child2 = match create_live_session(live_session2) {
        Ok(child) => child,
        Err(e) => {
            println!("Skipping test: failed to create live session 2: {}", e);
            let _ = kill_session(live_session1);
            return;
        }
    };

    // Give sessions a moment to stabilize
    thread::sleep(Duration::from_millis(200));

    // Verify both sessions exist before cleanup
    let sessions_before = list_needle_sessions();
    assert!(
        sessions_before.iter().any(|s| s.contains(live_session1)),
        "live session 1 should exist before cleanup"
    );
    assert!(
        sessions_before.iter().any(|s| s.contains(live_session2)),
        "live session 2 should exist before cleanup"
    );

    // Run bare needle cleanup (no --all, no -i)
    let needle_binary =
        std::env::var("CARGO_BIN_EXE_needle").unwrap_or_else(|_| "needle".to_string());

    let output = Command::new(&needle_binary)
        .arg("cleanup")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(result) => {
            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);

            // The command should succeed
            assert!(
                result.status.success(),
                "needle cleanup should succeed: stdout: {}, stderr: {}",
                stdout,
                stderr
            );

            // Should report no sessions cleaned
            assert!(
                stdout.contains("No matching sessions")
                    || stdout.contains("No sessions cleaned")
                    || stdout.contains("nothing"),
                "cleanup should report no sessions removed when all are live, got: {}",
                stdout
            );

            // Verify both live sessions are still present
            let sessions_after = list_needle_sessions();
            assert!(
                sessions_after.iter().any(|s| s.contains(live_session1)),
                "live session 1 should still exist after cleanup"
            );
            assert!(
                sessions_after.iter().any(|s| s.contains(live_session2)),
                "live session 2 should still exist after cleanup"
            );

            println!(
                "Test passed: needle cleanup preserved all live sessions and reported correctly"
            );
        }
        Err(e) => {
            panic!("needle cleanup command failed: {}", e);
        }
    }

    // Clean up the live sessions
    let _ = kill_session(live_session1);
    let _ = kill_session(live_session2);
}

/// Regression Test 3: needle cleanup --all removes every session regardless of liveness.
///
/// Test: `needle cleanup --all` still removes every session regardless of liveness.
///
/// This pins the --all behavior explicitly so it cannot regress while fixing
/// the no-flags path. The --all flag is the deliberate, fully-destructive mode.
///
/// NOTE: This integration test is disabled because it requires creating real
/// `needle run` processes, which is complex for a test environment. The unit
/// tests in `src/cli/mod.rs` already cover this scenario with mocked data and
/// are the authoritative regression tests for this behavior.
#[test]
#[cfg(unix)]
#[ignore = "Requires real needle run processes - covered by unit tests in src/cli/mod.rs"]
fn regression_cleanup_all_removes_all_sessions_regardless_of_liveness() {
    // Skip if needle binary not available
    if Command::new("needle").arg("--version").output().is_err() {
        println!("Skipping test: needle binary not available");
        return;
    }

    // Skip if tmux not available
    if Command::new("tmux").arg("-V").output().is_err() {
        println!("Skipping test: tmux not available");
        return;
    }

    let live_session1 = "needle-test-cleanup-all-live1";
    let live_session2 = "needle-test-cleanup-all-live2";
    let orphan_session = "needle-test-cleanup-all-orphan";

    // Clean up any existing test sessions
    let _ = kill_session(live_session1);
    let _ = kill_session(live_session2);
    let _ = kill_session(orphan_session);
    thread::sleep(Duration::from_millis(100));

    // Create two live sessions and one orphaned session
    let _live_child1 = match create_live_session(live_session1) {
        Ok(child) => child,
        Err(e) => {
            println!("Skipping test: failed to create live session 1: {}", e);
            return;
        }
    };

    let _live_child2 = match create_live_session(live_session2) {
        Ok(child) => child,
        Err(e) => {
            println!("Skipping test: failed to create live session 2: {}", e);
            let _ = kill_session(live_session1);
            return;
        }
    };

    if let Err(e) = create_orphaned_session(orphan_session) {
        println!("Skipping test: failed to create orphaned session: {}", e);
        let _ = kill_session(live_session1);
        let _ = kill_session(live_session2);
        return;
    }

    // Give sessions a moment to stabilize
    thread::sleep(Duration::from_millis(200));

    // Verify all three sessions exist before cleanup
    let sessions_before = list_needle_sessions();
    assert!(
        sessions_before.iter().any(|s| s.contains(live_session1)),
        "live session 1 should exist before cleanup"
    );
    assert!(
        sessions_before.iter().any(|s| s.contains(live_session2)),
        "live session 2 should exist before cleanup"
    );
    assert!(
        sessions_before.iter().any(|s| s.contains(orphan_session)),
        "orphaned session should exist before cleanup"
    );

    // Run needle cleanup --all
    let needle_binary =
        std::env::var("CARGO_BIN_EXE_needle").unwrap_or_else(|_| "needle".to_string());

    let output = Command::new(&needle_binary)
        .args(["cleanup", "--all"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(result) => {
            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);

            // The command should succeed
            assert!(
                result.status.success(),
                "needle cleanup --all should succeed: stdout: {}, stderr: {}",
                stdout,
                stderr
            );

            // Verify all sessions were removed
            let sessions_after = list_needle_sessions();
            assert!(
                !sessions_after.iter().any(|s| s.contains(live_session1)),
                "live session 1 should be removed by --all"
            );
            assert!(
                !sessions_after.iter().any(|s| s.contains(live_session2)),
                "live session 2 should be removed by --all"
            );
            assert!(
                !sessions_after.iter().any(|s| s.contains(orphan_session)),
                "orphaned session should be removed by --all"
            );

            // Should report cleanup occurred
            assert!(
                stdout.contains("Cleaned up") || stdout.contains("session"),
                "--all should report cleanup action, got: {}",
                stdout
            );

            println!("Test passed: needle cleanup --all removed all sessions as expected");
        }
        Err(e) => {
            panic!("needle cleanup --all command failed: {}", e);
        }
    }

    // No cleanup needed -- --all already removed everything
}

/// Unit test: verify that the cleanup command can be invoked.
///
/// This is a basic compilation and invocation test to ensure the cleanup
/// command infrastructure works correctly.
#[test]
#[cfg(unix)]
fn cleanup_command_invocation_compiles() {
    // This test verifies the cleanup command exists and can be invoked
    // without panicking at the CLI parsing level.

    use needle::cli::Cli;

    // Test bare cleanup invocation
    let args = vec!["needle", "cleanup"];
    let result = Cli::try_parse_from(args);
    assert!(result.is_ok(), "bare cleanup should parse");

    // Test cleanup --all
    let args_all = vec!["needle", "cleanup", "--all"];
    let result_all = Cli::try_parse_from(args_all);
    assert!(result_all.is_ok(), "cleanup --all should parse");

    // Test cleanup -i <pattern>
    let args_i = vec!["needle", "cleanup", "-i", "test"];
    let result_i = Cli::try_parse_from(args_i);
    assert!(result_i.is_ok(), "cleanup -i should parse");
}

/// P7.1a Regression Test: tmux-backed cleanup liveness test (real session, not constructed struct).
///
/// This test addresses the critical gap that let P7.1a ship undetected: the existing tests
/// construct TmuxSession structs directly rather than launching real tmux sessions. This bypasses
/// the exact indirection where the pane_pid-vs-child-PID bug lives.
///
/// The test:
/// 1. Launches a real tmux session with the exact command shape that launch_in_tmux() uses:
///    `NEEDLE_INNER=1 <cmd> ... 2>> <log>` — the output redirection defeats bash's exec
///    optimization, so pane_pid is the shell wrapper, not the child process.
/// 2. Verifies that pane_pid is NOT the actual child process (reproduces the bug condition).
/// 3. Asserts that bare `needle cleanup` does NOT remove the session (because the liveness
///    check walks the process tree and finds the live child).
///
/// This is the authoritative regression test for P7.1a — it exercises the real indirection,
/// not a mock. See ADR-003 addendum and plan.md Phase 7.1a for full context.
#[test]
#[cfg(unix)]
fn p71a_regression_tmux_session_with_shell_wrapper_split_not_removed_by_cleanup() {
    use std::fs;
    use std::path::Path;

    // Skip if tmux not available
    if Command::new("tmux").arg("-V").output().is_err() {
        println!("Skipping test: tmux not available");
        return;
    }

    let session_name = "needle-test-p71a-live";
    let test_log = "/tmp/needle-test-p71a.log";

    // Clean up any existing test session and log
    let _ = kill_session(session_name);
    let _ = std::fs::remove_file(test_log);
    thread::sleep(Duration::from_millis(100));

    // Launch a REAL tmux session with the exact command shape that launch_in_tmux() uses.
    // This is critical: the `NEEDLE_INNER=1 sleep 30 2>> /tmp/test.log` shape produces
    // the shell-wrapper-vs-child PID split because the output redirection defeats bash's
    // last-command exec optimization. pane_pid will be the shell, not sleep.
    let create_result = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            session_name,
            &format!("NEEDLE_INNER=1 sleep 30 2>> {}", test_log),
        ])
        .status();

    match create_result {
        Ok(status) if status.success() => {
            // Session created successfully
        }
        Ok(_) => {
            panic!("tmux new-session returned non-zero exit code");
        }
        Err(e) => {
            panic!("failed to create tmux session: {}", e);
        }
    }

    // Give the session a moment to stabilize
    thread::sleep(Duration::from_millis(200));

    // Get the pane_pid from tmux (this is the shell wrapper PID, not the sleep PID)
    let pane_pid_output = Command::new("tmux")
        .args(["list-panes", "-t", session_name, "-F", "#{pane_pid}"])
        .output();

    let pane_pid: u32 = match pane_pid_output {
        Ok(output) if output.status.success() => {
            let pid_str = String::from_utf8_lossy(&output.stdout).trim();
            match pid_str.parse() {
                Ok(pid) => pid,
                Err(_) => {
                    let _ = kill_session(session_name);
                    panic!("failed to parse pane_pid from tmux: '{}'", pid_str);
                }
            }
        }
        Ok(output) => {
            let _ = kill_session(session_name);
            panic!(
                "tmux list-panes failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(e) => {
            let _ = kill_session(session_name);
            panic!("failed to run tmux list-panes: {}", e);
        }
    };

    // Verify the pane_pid exists and is a shell process (reproduces the bug condition)
    let proc_path = Path::new("/proc").join(pane_pid.to_string()).join("cmdline");
    let cmdline = fs::read_to_string(&proc_path).unwrap_or_default();
    let cmdline_str = cmdline.replace('\0', " ");

    // The cmdline should contain bash/sh and NEEDLE_INNER, proving this is the shell wrapper
    assert!(
        cmdline_str.contains("NEEDLE_INNER"),
        "pane_pid {} should be a shell wrapper with NEEDLE_INNER in cmdline, got: {}",
        pane_pid,
        cmdline_str
    );

    // Verify the session exists in the session list
    let sessions_before = list_needle_sessions();
    assert!(
        sessions_before.iter().any(|s| s.contains(session_name)),
        "session should exist before cleanup"
    );

    // Run bare needle cleanup (no --all, no -i)
    // This should NOT remove the live session because:
    // 1. filter_sessions_for_cleanup() calls find_needle_process_in_tree(pane_pid)
    // 2. That walks the process tree and finds the actual sleep process
    // 3. The sleep PID is in the live_pids set (from scan_needle_processes())
    // 4. So the session is correctly classified as LIVE, not orphaned
    let needle_binary =
        std::env::var("CARGO_BIN_EXE_needle").unwrap_or_else(|_| "needle".to_string());

    let output = Command::new(&needle_binary)
        .arg("cleanup")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(result) => {
            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);

            // The command should succeed
            assert!(
                result.status.success(),
                "needle cleanup should succeed: stdout: {}, stderr: {}",
                stdout,
                stderr
            );

            // Verify the session was NOT removed (it's still live!)
            let sessions_after = list_needle_sessions();
            assert!(
                sessions_after.iter().any(|s| s.contains(session_name)),
                "LIVE session should NOT be removed by bare cleanup (P7.1a regression)"
            );

            // Should report no sessions cleaned (or at least not this session)
            if stdout.contains(session_name) {
                panic!(
                    "cleanup output should not mention the live session '{}', got: {}",
                    session_name, stdout
                );
            }

            println!(
                "P7.1a regression test passed: bare cleanup preserved live session with shell-wrapper split"
            );
        }
        Err(e) => {
            let _ = kill_session(session_name);
            panic!("needle cleanup command failed: {}", e);
        }
    }

    // Clean up the test session
    let _ = kill_session(session_name);
    let _ = std::fs::remove_file(test_log);
}
