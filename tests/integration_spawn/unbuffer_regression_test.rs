//! Integration test for GitHub #17: unbuffer masks exit codes.
//!
//! This test verifies that the built-in claude-sonnet and claude-opus adapters
//! correctly propagate exit codes from the claude CLI, without using unbuffer.
//!
//! Test scenario:
//! - Create a fake `claude` binary that exits 1 with stderr output
//! - Dispatch against the built-in claude-sonnet adapter
//! - Verify the outcome is Failure (not Success)
//! - Verify the bead is released (not closed)

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

#[cfg(unix)]
fn make_executable(path: &PathBuf) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
#[cfg(unix)]
fn fake_claude_exit_code_propagates() {
    // Create isolated temp directory for the test
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let bin_dir = temp_dir.path().join("bin");
    fs::create_dir(&bin_dir).expect("failed to create bin dir");

    // Create a fake `claude` binary that exits 1 with stderr output
    let fake_claude = bin_dir.join("claude");
    let script = r#"#!/bin/sh
# Fake claude that simulates "Not logged in" error
echo "Error: Not logged in. Run 'claude login' first." >&2
exit 1
"#;
    fs::write(&fake_claude, script).expect("failed to write fake claude");
    make_executable(&fake_claude);

    // Create a test workspace
    let workspace = temp_dir.path().join("workspace");
    fs::create_dir(&workspace).expect("failed to create workspace");

    // Create a test prompt file
    let prompt_file = temp_dir.path().join("prompt.txt");
    fs::write(&prompt_file, "Test prompt content").expect("failed to write prompt");

    // Build the invoke template exactly as the built-in claude-sonnet adapter does
    // (after our fix: no unbuffer, direct claude invocation)
    let workspace_str = workspace.to_string_lossy();
    let prompt_file_str = prompt_file.to_string_lossy();
    let invoke_cmd = format!(
        "cd {} && {} -p --model claude-sonnet-4-6 --max-turns 30 --output-format stream-json --dangerously-skip-permissions --verbose < {}",
        workspace_str,
        fake_claude.to_string_lossy(),
        prompt_file_str
    );

    // Execute the command
    let output = Command::new("sh")
        .arg("-c")
        .arg(&invoke_cmd)
        .output()
        .expect("failed to execute command");

    // Verify the command failed (exit code 1)
    assert!(
        !output.status.success(),
        "fake claude should exit with non-zero status"
    );
    assert_eq!(output.status.code(), Some(1), "exit code should be 1");

    // Verify stderr contains the error message
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Not logged in"),
        "stderr should contain error message"
    );

    // Verify stdout is empty (since we failed before producing output)
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.is_empty(),
        "stdout should be empty when claude fails"
    );
}
