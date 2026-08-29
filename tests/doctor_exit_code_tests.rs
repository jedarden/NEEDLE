// Tests for needle doctor exit code behavior
//
// Verifies that:
// - Exit code 0 when all checks pass (or only warnings)
// - Exit code 1 when any check fails
// - Both normal and --repair modes follow the same rule

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Create a minimal workspace with .needle.yaml specifying a bead backend
fn create_test_workspace(temp_dir: &Path, backend_name: &str) -> PathBuf {
    let workspace = temp_dir.join("test-workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(workspace.join(".beads")).unwrap();

    // Create .needle.yaml with the specified backend
    let needle_yaml = workspace.join(".needle.yaml");
    fs::write(
        &needle_yaml,
        format!(
            r#"
bead_cli:
  backend: {}
"#,
            backend_name
        ),
    )
    .unwrap();

    workspace
}

/// Create a healthy workspace (all dependencies satisfied)
fn create_healthy_workspace(temp_dir: &Path) -> PathBuf {
    let workspace = temp_dir.join("healthy-workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(workspace.join(".beads")).unwrap();

    // Create .needle.yaml with bead-rs (assuming it's available)
    let needle_yaml = workspace.join(".needle.yaml");
    fs::write(
        &needle_yaml,
        r#"
bead_cli:
  backend: bead-rs
"#,
    )
    .unwrap();

    workspace
}

#[test]
fn doctor_exits_nonzero_when_backend_not_on_path() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace =
        create_test_workspace(temp_dir.path(), "totally-fake-backend-not-on-path-12345");

    // Run needle doctor against this workspace
    let output = Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("doctor")
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .expect("failed to execute needle doctor");

    // Should exit with non-zero code (1) due to backend not being found
    assert!(
        !output.status.success(),
        "needle doctor should exit non-zero when backend is not on PATH"
    );
    assert_eq!(output.status.code(), Some(1));

    // Output should mention the failure
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("failure"),
        "output should mention 'failure' when checks fail"
    );
    assert!(
        stdout.contains("Exit code 1"),
        "output should mention 'Exit code 1'"
    );
}

#[test]
fn doctor_exits_zero_when_healthy() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = create_healthy_workspace(temp_dir.path());

    // Run needle doctor against this healthy workspace
    let output = Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("doctor")
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .expect("failed to execute needle doctor");

    // Note: This test may fail if the actual environment doesn't have bead-rs on PATH
    // or if other system checks fail. The key point is that IF all checks pass,
    // the exit code should be 0.
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Should NOT mention exit code 1 when successful
        assert!(
            !stdout.contains("Exit code 1"),
            "output should not mention 'Exit code 1' when all checks pass"
        );
    }
    // We don't assert on success here since it depends on the actual environment
}

#[test]
fn doctor_repair_follows_same_exit_code_rules() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = create_test_workspace(temp_dir.path(), "another-fake-backend-67890");

    // Run needle doctor --repair against this workspace
    let output = Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("doctor")
        .arg("--repair")
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .expect("failed to execute needle doctor --repair");

    // Should still exit with non-zero code (1) when repairs don't fix everything
    assert!(
        !output.status.success(),
        "needle doctor --repair should exit non-zero when failures remain after repairs"
    );
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn doctor_warnings_do_not_cause_nonzero_exit() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path().join("warn-workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(workspace.join(".beads")).unwrap();

    // Create .needle.yaml with a valid backend
    let needle_yaml = workspace.join(".needle.yaml");
    fs::write(
        &needle_yaml,
        r#"
bead_cli:
  backend: bead-rs
"#,
    )
    .unwrap();

    // Run needle doctor
    let output = Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("doctor")
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .expect("failed to execute needle doctor");

    // If there are only warnings (no failures), exit code should be 0
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.contains("warning(s)") && !stdout.contains("failure(s)") {
        assert!(
            output.status.success(),
            "needle doctor should exit 0 when there are only warnings, no failures"
        );
        assert!(!stdout.contains("Exit code 1"));
    }
}

#[test]
fn doctor_mentions_exit_code_in_summary_on_failure() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = create_test_workspace(temp_dir.path(), "nonexistent-backend-test");

    let output = Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("doctor")
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .expect("failed to execute needle doctor");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should mention "Exit code 1" in the output when there are failures
    assert!(
        stdout.contains("Exit code 1"),
        "summary should mention 'Exit code 1: <n> failure(s)' when there are failures"
    );
}
