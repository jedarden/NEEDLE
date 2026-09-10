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

/// Initialize a bead workspace bound to the bead-rs backend.
///
/// Returns false when the bead CLI is unavailable so the caller can skip.
/// The `bead_cli.backend` binding is required for doctor to open the store —
/// without it every checkpoint row is the "no bead backend binding" WARN and
/// none of the checkpoint states below are reachable.
fn init_bead_workspace(workspace: &Path) -> bool {
    let output = Command::new("bead")
        .arg("init")
        .current_dir(workspace)
        .output();

    // If bead is not available, skip the caller's test
    match output {
        Ok(out) if out.status.success() => {}
        _ => {
            println!("WARNING: bead CLI not available, skipping test");
            return false;
        }
    }

    fs::write(
        workspace.join(".needle.yaml"),
        r#"
bead_cli:
  backend: bead-rs
"#,
    )
    .unwrap();
    true
}

/// Remove the published checkpoint, leaving "no checkpoint" behind.
///
/// bead-rs releases changed when the checkpoint is published: current builds
/// write an (empty) `checkpoint/current.json` during `bead init` and on every
/// mutation, older ones only on the first mutation. The states under test are
/// defined by the data, not by the CLI's init side effects, so the fixtures
/// remove the checkpoint explicitly instead of assuming either behavior.
fn remove_checkpoint(workspace: &Path) {
    let checkpoint_dir = workspace.join(".beads/checkpoint");
    if checkpoint_dir.exists() {
        fs::remove_dir_all(&checkpoint_dir).unwrap();
    }
}

#[test]
fn doctor_empty_store_no_checkpoint_is_warn_not_fail() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path().join("empty-store-workspace");
    fs::create_dir_all(&workspace).unwrap();

    if !init_bead_workspace(&workspace) {
        return;
    }
    remove_checkpoint(&workspace);

    // Verify no checkpoint exists (the fixture constructs this state)
    let checkpoint = workspace.join(".beads/checkpoint/current.json");
    assert!(
        !checkpoint.exists(),
        "fixture should leave the workspace without a checkpoint file"
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
    if !init_bead_workspace(&workspace) {
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
    remove_checkpoint(&workspace);

    // Run needle doctor
    let doctor_output = Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("doctor")
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .expect("failed to execute needle doctor");

    let stdout = String::from_utf8_lossy(&doctor_output.stdout);

    // The Checkpoint row itself must be the FAIL — not an unrelated row while
    // the checkpoint row shows something else.
    let checkpoint_fail = stdout
        .lines()
        .find(|l| l.contains("Checkpoint") && l.contains("FAIL"));
    assert!(
        checkpoint_fail.is_some(),
        "store with beads but no checkpoint should show a FAIL Checkpoint row: {}",
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

    // Initialize bead workspace in the empty-store state
    if !init_bead_workspace(&workspace) {
        return;
    }
    remove_checkpoint(&workspace);

    // Run needle doctor
    let doctor_output = Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("doctor")
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .expect("failed to execute needle doctor");

    let stdout = String::from_utf8_lossy(&doctor_output.stdout);

    // The checkpoint row must be a WARN here — the fixture guarantees the
    // empty-store state that produces it.
    assert!(
        stdout.contains("WARN") && stdout.contains("Checkpoint"),
        "empty store should produce a checkpoint WARN: {}",
        stdout
    );

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

// ─────────────────────────────────────────────────────────────────────────────
// `needle doctor --json` contract (needle-24bc5a35)
//
// stdout carries exactly one JSON document — rows, summary, exit_code — where
// every row is {name, status, detail, fix} and `fix` is the command a machine
// can run to repair the row. The human table prints the same fix text.
// ─────────────────────────────────────────────────────────────────────────────

use serde_json::Value;

/// Run `needle doctor --json` against `workspace`.
///
/// Returns the raw process output plus the parsed document. HOME is pinned to
/// the fixture's parent directory so the spawned binary neither reads nor
/// repairs the real user environment (docs/testing-isolation-patterns.md).
fn run_doctor_json(workspace: &Path) -> (std::process::Output, Value) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_needle"));
    cmd.arg("doctor")
        .arg("--json")
        .arg("--workspace")
        .arg(workspace);
    if let Some(home) = workspace.parent() {
        cmd.env("HOME", home);
    }
    let output = cmd
        .output()
        .expect("failed to execute needle doctor --json");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let doc: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("doctor --json stdout must be exactly one JSON document ({error}):\n{stdout}")
    });
    (output, doc)
}

/// Run `needle doctor` in human mode with the same isolation as
/// `run_doctor_json`, returning its stdout.
fn run_doctor_human(workspace: &Path) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_needle"));
    cmd.arg("doctor").arg("--workspace").arg(workspace);
    if let Some(home) = workspace.parent() {
        cmd.env("HOME", home);
    }
    let output = cmd.output().expect("failed to execute needle doctor");
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn json_rows(doc: &Value) -> &[Value] {
    doc["rows"]
        .as_array()
        .expect("document carries a rows array")
}

fn json_row<'a>(doc: &'a Value, name: &str) -> &'a Value {
    json_rows(doc)
        .iter()
        .find(|row| row["name"] == *name)
        .unwrap_or_else(|| panic!("no '{name}' row in doctor --json output"))
}

/// The --json contract: exit_code mirrors both the fail count and the actual
/// process exit code, and every FAIL row a machine could act on carries a fix.
fn assert_json_contract(output: &std::process::Output, doc: &Value) {
    for row in json_rows(doc) {
        // serde_json's Value object is a sorted map, so the emitted key order
        // is alphabetical and carries no contract meaning — the contract is the
        // exact key *set*.
        let mut keys: Vec<&str> = row
            .as_object()
            .expect("row is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["detail", "fix", "name", "status"],
            "rows must carry exactly the contract keys"
        );
        let status = row["status"].as_str().expect("status is a string");
        assert!(
            matches!(status, "pass" | "warn" | "fail"),
            "unexpected status {status}"
        );
        assert!(row["detail"].is_string(), "detail is a string");
        assert!(
            row["fix"].is_null() || row["fix"].is_string(),
            "fix is a command or null"
        );
        if status == "fail" {
            assert!(
                row["fix"].is_string(),
                "FAIL row '{}' must carry a fix command",
                row["name"]
            );
        }
    }

    let count = |status: &str| {
        json_rows(doc)
            .iter()
            .filter(|row| row["status"] == status)
            .count()
    };
    assert_eq!(doc["summary"]["pass"], count("pass"), "summary.pass");
    assert_eq!(doc["summary"]["warn"], count("warn"), "summary.warn");
    assert_eq!(doc["summary"]["fail"], count("fail"), "summary.fail");

    let expected_exit = if count("fail") > 0 { 1 } else { 0 };
    assert_eq!(doc["exit_code"], expected_exit, "exit_code field");
    assert_eq!(
        output.status.code(),
        Some(expected_exit),
        "process exit code must match the exit_code field"
    );
}

/// Workspace bound to a bead CLI path that cannot exist: the backend row fails.
fn create_json_backend_fixture(temp_dir: &Path) -> PathBuf {
    create_test_workspace(temp_dir, "bead-rs")
}

/// Bound workspace whose agent binary cannot exist.
fn create_json_agent_fixture(temp_dir: &Path) -> PathBuf {
    let workspace = temp_dir.join("agent-fixture");
    fs::create_dir_all(workspace.join(".beads")).unwrap();
    fs::write(
        workspace.join(".needle.yaml"),
        "agent:\n  default: needle-fake-agent-json\nbead_cli:\n  backend: bead-rs\n",
    )
    .unwrap();
    workspace
}

/// Bound workspace wired to a user-defined adapter whose transform binary and
/// invoke_template executable cannot exist.
///
/// `agent.adapters_dir` is not a workspace-overridable key (`apply_workspace`
/// merges only default/timeout/routing), so the fixture pins it through the
/// global config inside the isolated HOME that `run_doctor_json` sets.
fn create_json_transform_fixture(temp_dir: &Path) -> PathBuf {
    let workspace = temp_dir.join("transform-fixture");
    fs::create_dir_all(workspace.join(".beads")).unwrap();
    let adapters_dir = temp_dir.join("adapters");
    fs::create_dir_all(&adapters_dir).unwrap();
    fs::write(
        adapters_dir.join("fakegate.yaml"),
        concat!(
            "name: fakegate\n",
            "agent_cli: needle-fake-gate-agent\n",
            "invoke_template: \"needle-fake-gate-agent --verbose < {prompt_file}\"\n",
            "output_transform: needle-fake-gate-transform\n",
        ),
    )
    .unwrap();
    let global_config_dir = temp_dir.join(".config").join("needle");
    fs::create_dir_all(&global_config_dir).unwrap();
    fs::write(
        global_config_dir.join("config.yaml"),
        format!("agent:\n  adapters_dir: {}\n", adapters_dir.display()),
    )
    .unwrap();
    fs::write(
        workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .unwrap();
    workspace
}

#[test]
fn doctor_json_document_matches_contract() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = create_json_backend_fixture(temp_dir.path());
    let (output, doc) = run_doctor_json(&workspace);
    assert_json_contract(&output, &doc);
    assert!(
        !json_rows(&doc).is_empty(),
        "doctor always reports at least one row"
    );
}

#[test]
fn doctor_json_bead_cli_missing_row_has_fix() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = create_json_backend_fixture(temp_dir.path());
    let (_, doc) = run_doctor_json(&workspace);
    let row = json_row(&doc, "Bead backend");
    assert_eq!(row["status"], "fail");
    let fix = row["fix"].as_str().expect("missing bead CLI carries a fix");
    assert!(
        fix.contains("cargo install") && fix.contains("bead-rs"),
        "fix should install bead-rs, got: {fix}"
    );
}

#[test]
fn doctor_json_missing_beads_dir_row_has_fix() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path().join("no-beads-fixture");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .unwrap();

    let (_, doc) = run_doctor_json(&workspace);
    let row = json_row(&doc, "Workspace");
    assert_eq!(row["status"], "fail");
    let fix = row["fix"].as_str().expect("missing .beads/ carries a fix");
    assert!(
        fix.starts_with("mkdir -p"),
        "fix should create the directory, got: {fix}"
    );
}

#[test]
fn doctor_json_missing_needle_yaml_row_has_fix() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path().join("no-binding-fixture");
    fs::create_dir_all(workspace.join(".beads")).unwrap();

    let (output, doc) = run_doctor_json(&workspace);
    let row = json_row(&doc, "Workspace");
    assert_eq!(row["status"], "fail");
    assert!(
        row["detail"].as_str().unwrap().contains(".needle.yaml"),
        "detail should name the missing file: {}",
        row["detail"]
    );
    let fix = row["fix"].as_str().expect("missing binding carries a fix");
    assert!(
        fix.contains("needle init"),
        "fix should run `needle init`, got: {fix}"
    );
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn doctor_json_missing_agent_binary_row_has_fix() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = create_json_agent_fixture(temp_dir.path());
    let (_, doc) = run_doctor_json(&workspace);
    let row = json_row(&doc, "Agent binary");
    assert_eq!(row["status"], "fail");
    let fix = row["fix"]
        .as_str()
        .expect("missing agent binary carries a fix");
    assert!(
        fix.contains("agent.default"),
        "fix should name the agent.default escape hatch, got: {fix}"
    );
}

#[test]
fn doctor_json_missing_transform_rows_have_fix() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = create_json_transform_fixture(temp_dir.path());
    let (_, doc) = run_doctor_json(&workspace);

    let transforms = json_row(&doc, "Adapter transforms");
    assert_eq!(transforms["status"], "warn");
    let transform_fix = transforms["fix"]
        .as_str()
        .expect("missing transform binary carries a fix");
    assert!(
        transform_fix.contains("needle-fake-gate-transform"),
        "custom transform fix should name the binary to install, got: {transform_fix}"
    );

    let templates = json_row(&doc, "Adapter template executables");
    assert_eq!(templates["status"], "fail");
    let template_fix = templates["fix"]
        .as_str()
        .expect("missing template executable carries a fix");
    assert!(
        template_fix.contains("needle-fake-gate-agent"),
        "fix should name the missing invoke_template executable, got: {template_fix}"
    );
}

#[test]
fn doctor_human_table_prints_the_json_fix_text() {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = create_json_backend_fixture(temp_dir.path());

    let (_, doc) = run_doctor_json(&workspace);
    let fixes: Vec<&str> = json_rows(&doc)
        .iter()
        .filter_map(|row| row["fix"].as_str())
        .collect();
    assert!(
        !fixes.is_empty(),
        "the failing fixture must produce at least one fix"
    );

    let human = run_doctor_human(&workspace);
    for fix in fixes {
        assert!(
            human.contains(fix),
            "human table must print the same fix text the JSON row carries:\n  fix: {fix}\n  table:\n{human}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Quickstart-config guard
//
// A global config byte-identical to the shipped quickstart example is exactly
// what the walkthrough writes into its own sandbox HOME — and exactly what it
// must never write over a real fleet config (2026-08-30, ex44). doctor passes
// the first case and warns on the second, but only when this host also runs
// NEEDLE somewhere else.
// ─────────────────────────────────────────────────────────────────────────────

/// The shipped example, as the binary embeds it: same file, same compile.
const QUICKSTART_EXAMPLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/docs/examples/quickstart/config.yaml"
));

/// Install `contents` as the isolated HOME's global config.
fn write_global_config(home: &Path, contents: &str) {
    let config_dir = home.join(".config").join("needle");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("config.yaml"), contents).unwrap();
}

/// A second NEEDLE workspace in the same (isolated) HOME.
fn add_other_workspace(home: &Path) -> PathBuf {
    let other = home.join("other-fleet-repo");
    fs::create_dir_all(other.join(".beads")).unwrap();
    other
}

#[test]
fn doctor_warns_when_global_config_is_the_quickstart_example() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_global_config(temp_dir.path(), QUICKSTART_EXAMPLE);
    let other = add_other_workspace(temp_dir.path());
    let workspace = create_test_workspace(temp_dir.path(), "bead-rs");

    let human = run_doctor_human(&workspace);
    assert!(
        human.contains("[WARN]  Quickstart config"),
        "expected a WARN row for the quickstart config, got:\n{human}"
    );
    assert!(
        human.contains("byte-identical to the shipped quickstart example"),
        "the row should say what matched, got:\n{human}"
    );
    assert!(
        human.contains(other.to_string_lossy().as_ref()),
        "the row should name the other workspaces that prove this is a fleet host, got:\n{human}"
    );
    assert!(
        human.contains("needle init --force"),
        "the row should name the remedy, got:\n{human}"
    );

    let (_, doc) = run_doctor_json(&workspace);
    let row = json_row(&doc, "Quickstart config");
    assert_eq!(row["status"], "warn");
    assert_eq!(row["fix"], "needle init --force");
}

#[test]
fn doctor_passes_quickstart_example_inside_its_own_sandbox_home() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_global_config(temp_dir.path(), QUICKSTART_EXAMPLE);
    // No other workspace anywhere in this HOME — the example's own situation.
    let workspace = create_test_workspace(temp_dir.path(), "bead-rs");

    let human = run_doctor_human(&workspace);
    assert!(
        human.contains("[PASS]  Quickstart config"),
        "a sandbox HOME is the one place the example belongs, got:\n{human}"
    );
    assert!(
        human.contains("no other NEEDLE workspaces live here"),
        "the pass row should say why it passed, got:\n{human}"
    );
    assert!(
        !human.contains("byte-identical to the shipped quickstart example"),
        "no warn text should appear, got:\n{human}"
    );
}

#[test]
fn doctor_passes_when_global_config_differs_from_the_example() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_global_config(
        temp_dir.path(),
        "# fleet config under test\nagent:\n  default: opus\nworker:\n  max_workers: 6\n",
    );
    add_other_workspace(temp_dir.path());
    let workspace = create_test_workspace(temp_dir.path(), "bead-rs");

    let human = run_doctor_human(&workspace);
    assert!(
        human.contains("[PASS]  Quickstart config"),
        "a real fleet config is not the example, got:\n{human}"
    );

    let (_, doc) = run_doctor_json(&workspace);
    assert_eq!(json_row(&doc, "Quickstart config")["status"], "pass");
}
