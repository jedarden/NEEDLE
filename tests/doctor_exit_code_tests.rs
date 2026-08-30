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
    // Use the path field to point to a non-existent binary to trigger a failure
    let needle_yaml = workspace.join(".needle.yaml");
    fs::write(
        &needle_yaml,
        format!(
            r#"
bead_cli:
  backend: {}
  path: /totally/fake/path/not/on/path/12345/{}
"#,
            backend_name, backend_name
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
    let workspace = create_test_workspace(temp_dir.path(), "bead-rs");

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
    let workspace = create_test_workspace(temp_dir.path(), "bead-rs");

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

#[test]
fn doctor_empty_store_no_checkpoint_is_warn_not_fail() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path().join("empty-store-workspace");
    fs::create_dir_all(&workspace).unwrap();

    // Initialize bead workspace (creates .beads/ with beads.db but zero beads, no checkpoint)
    let output = Command::new("bead")
        .arg("init")
        .current_dir(&workspace)
        .output();

    // If bead is not available, skip this test
    if output.is_err() || !output.as_ref().unwrap().status.success() {
        println!("WARNING: bead CLI not available, skipping test");
        return;
    }

    // Verify no checkpoint exists (bead init doesn't create one)
    let checkpoint = workspace.join(".beads/checkpoint/current.json");
    assert!(
        !checkpoint.exists(),
        "bead init should not create a checkpoint file"
    );

    // Run needle doctor
    let doctor_output = Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("doctor")
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .expect("failed to execute needle doctor");

    let stdout = String::from_utf8_lossy(&doctor_output.stdout);

    // Should show WARN for checkpoint, not FAIL
    assert!(
        stdout.contains("WARN") && stdout.contains("Checkpoint"),
        "empty store with no checkpoint should show WARN, not FAIL"
    );
    assert!(
        stdout.contains("empty store") || stdout.contains("after the first bead"),
        "WARN message should mention empty store"
    );

    // The checkpoint line should not be a FAIL
    let lines: Vec<&str> = stdout.lines().collect();
    for line in &lines {
        if line.contains("Checkpoint") && line.contains("FAIL") {
            panic!(
                "Checkpoint should be WARN for empty store, but got FAIL line: {}",
                line
            );
        }
    }

    // Exit code should be 0 because WARN doesn't count toward exit 1
    // (assuming all other checks pass or are also WARN/SKIP)
    let other_fails: Vec<&str> = lines
        .iter()
        .filter(|l| l.contains("FAIL") && !l.contains("Checkpoint"))
        .copied()
        .collect();

    if other_fails.is_empty() {
        assert!(
            doctor_output.status.success(),
            "needle doctor should exit 0 when only WARN (no FAIL): {}",
            stdout
        );
        assert!(
            !stdout.contains("Exit code 1"),
            "should not mention Exit code 1 when only WARN"
        );
    }
}

#[test]
fn doctor_store_with_beads_no_checkpoint_is_fail() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path().join("with-beads-workspace");
    fs::create_dir_all(&workspace).unwrap();

    // Initialize bead workspace
    let init_output = Command::new("bead")
        .arg("init")
        .current_dir(&workspace)
        .output();

    // If bead is not available, skip this test
    if init_output.is_err() || !init_output.as_ref().unwrap().status.success() {
        println!("WARNING: bead CLI not available, skipping test");
        return;
    }

    // Create a bead so the store is no longer empty
    let bead_output = Command::new("bead")
        .arg("create")
        .arg("--title")
        .arg("Test bead")
        .arg("--priority")
        .arg("0")
        .arg("--issue-type")
        .arg("task")
        .current_dir(&workspace)
        .output();

    // If bead creation fails, skip this test
    let bead_output = match bead_output {
        Ok(output) => output,
        Err(e) => {
            println!(
                "WARNING: bead create command failed to run, skipping test: {}",
                e
            );
            return;
        }
    };

    if !bead_output.status.success() {
        println!(
            "WARNING: bead create failed, skipping test: {}",
            String::from_utf8_lossy(&bead_output.stderr)
        );
        return;
    }

    // Verify we have at least one bead
    let list_output = Command::new("bead")
        .arg("list")
        .arg("--json")
        .current_dir(&workspace)
        .output();
    assert!(
        list_output.is_ok() && list_output.unwrap().status.success(),
        "bead list should succeed"
    );

    // Remove the checkpoint if it was created (to simulate the missing checkpoint case)
    let checkpoint_dir = workspace.join(".beads/checkpoint");
    if checkpoint_dir.exists() {
        fs::remove_dir_all(&checkpoint_dir).unwrap();
    }

    // Run needle doctor
    let doctor_output = Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("doctor")
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .expect("failed to execute needle doctor");

    let stdout = String::from_utf8_lossy(&doctor_output.stdout);

    // Should show FAIL for checkpoint (store has beads but no checkpoint)
    assert!(
        stdout.contains("FAIL") && stdout.contains("Checkpoint"),
        "store with beads but no checkpoint should show FAIL: {}",
        stdout
    );

    // Exit code should be 1 due to FAIL
    assert!(
        !doctor_output.status.success(),
        "needle doctor should exit non-zero when checkpoint FAIL (store has beads): {}",
        stdout
    );
    assert_eq!(doctor_output.status.code(), Some(1));
    assert!(
        stdout.contains("Exit code 1"),
        "should mention Exit code 1 when checkpoint FAIL"
    );
}

#[test]
fn doctor_checkpoint_warn_does_not_cause_exit_1() {
    // This test verifies that the WARN for "empty store + no checkpoint"
    // specifically does not cause exit code 1, even if it's the only check result.
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path().join("checkpoint-warn-workspace");
    fs::create_dir_all(&workspace).unwrap();

    // Initialize bead workspace
    let init_output = Command::new("bead")
        .arg("init")
        .current_dir(&workspace)
        .output();

    // If bead is not available, skip this test
    if init_output.is_err() || !init_output.as_ref().unwrap().status.success() {
        println!("WARNING: bead CLI not available, skipping test");
        return;
    }

    // Run needle doctor
    let doctor_output = Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("doctor")
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .expect("failed to execute needle doctor");

    let stdout = String::from_utf8_lossy(&doctor_output.stdout);

    // Check if we have the expected WARN for checkpoint
    if stdout.contains("WARN") && stdout.contains("Checkpoint") {
        // Count FAIL results (excluding checkpoint, which should be WARN)
        let lines: Vec<&str> = stdout.lines().collect();
        let fail_count = lines.iter().filter(|l| l.contains("FAIL")).count();

        // If only WARN (no FAIL), exit code should be 0
        if fail_count == 0 {
            assert!(
                doctor_output.status.success(),
                "needle doctor should exit 0 when checkpoint is WARN (no FAIL checks): {}",
                stdout
            );
            assert!(
                !stdout.contains("Exit code 1"),
                "should not mention Exit code 1 when only WARN: {}",
                stdout
            );
        }
    }
}
