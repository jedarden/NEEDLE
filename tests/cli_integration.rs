//! CLI integration tests for needle binary.
//!
//! These tests spawn the actual needle binary to verify end-to-end CLI behavior.
//! All tests use proper test isolation by setting HOME to a temporary directory.

use std::process::Command;
use tempfile::TempDir;

/// Helper to create a Command for the needle binary with isolated HOME.
fn needle_command(home: &TempDir) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_needle"));
    cmd.env("HOME", home.path());
    cmd
}

/// Test that `needle config --set worker.max_workers 10` (space syntax) parses without clap error.
///
/// This is an integration test that verifies the actual binary can parse the --set flag
/// with space-separated KEY VALUE format (not just the unit test that checks CLAP parsing).
#[test]
fn config_set_space_syntax_parses() {
    let temp_home = TempDir::new().expect("failed to create temp HOME");

    // Invoke needle binary with: config --set worker.max_workers 10
    let output = needle_command(&temp_home)
        .args(["config", "--set", "worker.max_workers", "10"])
        .output()
        .expect("failed to execute needle binary");

    // Verify command returns success (exit code 0)
    assert!(
        output.status.success(),
        "needle config --set should succeed with space syntax. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify no clap parsing error occurs
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Clap errors typically contain these keywords
    assert!(
        !stderr.to_lowercase().contains("error") && !stdout.to_lowercase().contains("error"),
        "needle config --set should not produce clap parsing errors. stdout: {}, stderr: {}",
        stdout,
        stderr
    );

    // Verify clap-specific error messages are absent
    assert!(
        !stderr.contains("unrecognized")
            && !stderr.contains("unexpected")
            && !stderr.contains("invalid")
            && !stdout.contains("unrecognized")
            && !stdout.contains("unexpected")
            && !stdout.contains("invalid"),
        "needle config --set should not contain clap error messages. stdout: {}, stderr: {}",
        stdout,
        stderr
    );
}

/// Test that `needle config --set worker.max_workers=10` (equals syntax) parses without clap error.
#[test]
fn config_set_equals_syntax_parses() {
    let temp_home = TempDir::new().expect("failed to create temp HOME");

    // Invoke needle binary with: config --set worker.max_workers=10
    let output = needle_command(&temp_home)
        .args(["config", "--set", "worker.max_workers=10"])
        .output()
        .expect("failed to execute needle binary");

    // Verify command returns success
    assert!(
        output.status.success(),
        "needle config --set should succeed with equals syntax. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify no clap parsing error
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.to_lowercase().contains("error"),
        "needle config --set should not produce clap errors with equals syntax. stderr: {}",
        stderr
    );
}

/// Test multiple --set flags in space syntax.
#[test]
fn config_set_multiple_space_syntax_parses() {
    let temp_home = TempDir::new().expect("failed to create temp HOME");

    let output = needle_command(&temp_home)
        .args([
            "config",
            "--set",
            "worker.max_workers",
            "10",
            "--set",
            "agent.timeout",
            "3600",
        ])
        .output()
        .expect("failed to execute needle binary");

    assert!(
        output.status.success(),
        "needle config --set should succeed with multiple space-syntax flags. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Test mixed space and equals syntax.
#[test]
fn config_set_mixed_syntax_parses() {
    let temp_home = TempDir::new().expect("failed to create temp HOME");

    let output = needle_command(&temp_home)
        .args([
            "config",
            "--set",
            "worker.max_workers",
            "10",
            "--set",
            "agent.timeout=3600",
        ])
        .output()
        .expect("failed to execute needle binary");

    assert!(
        output.status.success(),
        "needle config --set should succeed with mixed space/equals syntax. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Test that --set functionality is not yet fully implemented.
///
/// This test verifies that while --set parsing works, the actual set operation
/// is not yet implemented. Once set is implemented, this test should be updated
/// to verify successful config modification.
#[test]
fn config_set_not_yet_implemented() {
    let temp_home = TempDir::new().expect("failed to create temp HOME");

    // Use any valid key - the implementation doesn't matter since set isn't implemented
    let output = needle_command(&temp_home)
        .args(["config", "--set", "worker.max_workers", "10"])
        .output()
        .expect("failed to execute needle binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Currently, --set parses successfully but the functionality isn't implemented
    // The binary should print a message indicating set is not yet implemented
    assert!(
        stdout.contains("set not yet implemented") || stderr.contains("set not yet implemented"),
        "needle config --set should indicate it's not yet implemented. stdout: {}, stderr: {}",
        stdout,
        stderr
    );

    // Verify it's not a clap parsing error
    assert!(
        !stderr.to_lowercase().contains("unrecognized")
            && !stderr.to_lowercase().contains("unexpected")
            && !stdout.to_lowercase().contains("unrecognized")
            && !stdout.to_lowercase().contains("unexpected"),
        "Should not be a clap parsing error. stdout: {}, stderr: {}",
        stdout,
        stderr
    );
}
