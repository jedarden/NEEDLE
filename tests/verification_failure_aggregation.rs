//! Verification runner failure aggregation tests
//!
//! These tests verify that the verification runner properly aggregates
//! failures from multiple checks, ensuring:
//! - All checks run even when earlier checks fail
//! - Exit codes and stdout/stderr are captured for each check
//! - Final report lists ALL failures, not just the first
//! - Script exits with non-zero if ANY check failed
//! - Report format is clear and parseable (JSON and structured text)

use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn test_verification_runner_aggregates_all_failures() {
    // Create a temporary directory for the test
    let test_dir = tempfile::tempdir().unwrap();
    let test_config = test_dir.path().join("test-config.yaml");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Write a test configuration with multiple failing checks
    let config_content = r#"version: "1.0"

fast_lane:
  - name: "First failing check"
    command: "bash"
    args: ["-c", "echo 'stdout1' && echo 'stderr1' >&2 && exit 1"]
    timeout: 10

  - name: "Second failing check"
    command: "bash"
    args: ["-c", "echo 'stdout2' && echo 'stderr2' >&2 && exit 2"]
    timeout: 10

  - name: "Passing check between failures"
    command: "true"
    args: []
    timeout: 10

  - name: "Third failing check"
    command: "bash"
    args: ["-c", "echo 'stdout3' && echo 'stderr3' >&2 && exit 3"]
    timeout: 10
"#;

    fs::write(&test_config, config_content).unwrap();

    // Run the verification runner
    let runner_script = repo_root.join("scripts/verification-runner.sh");
    let output = Command::new("bash")
        .arg(&runner_script)
        .arg("--config")
        .arg(&test_config)
        .arg("--fast")
        .current_dir(&repo_root)
        .output()
        .expect("Failed to execute verification runner");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Should have non-zero exit code (some checks failed)
    assert!(
        !output.status.success(),
        "Expected non-zero exit code, got: {}",
        output.status
    );

    // Verify all failures are reported in the summary
    let summary = extract_section(&stdout, "=== Verification Summary ===");
    assert!(
        summary.contains("Failed: 3"),
        "Should report 3 failed checks"
    );

    // Verify each failure is listed
    assert!(
        summary.contains("First failing check"),
        "Should list first failure"
    );
    assert!(
        summary.contains("Second failing check"),
        "Should list second failure"
    );
    assert!(
        summary.contains("Third failing check"),
        "Should list third failure"
    );

    // Verify each failure has its exit code
    assert!(summary.contains("Exit code: 1"), "Should show exit code 1");
    assert!(summary.contains("Exit code: 2"), "Should show exit code 2");
    assert!(summary.contains("Exit code: 3"), "Should show exit code 3");

    // Verify stderr output is captured
    assert!(
        summary.contains("stderr1") || summary.contains("STDERR"),
        "Should capture stderr output"
    );
    assert!(
        summary.contains("stderr2") || summary.contains("STDERR"),
        "Should capture stderr for second check"
    );

    println!("=== Test Output ===");
    println!("STDOUT:\n{}", stdout);
    println!("STDERR:\n{}", stderr);
    println!("Exit code: {}", output.status.code().unwrap_or(-1));
}

#[test]
fn test_verification_runner_captures_stdout_and_stderr_separately() {
    let test_dir = tempfile::tempdir().unwrap();
    let test_config = test_dir.path().join("test-config.yaml");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Configuration with both stdout and stderr output
    let config_content = r#"version: "1.0"

fast_lane:
  - name: "Check with both outputs"
    command: "bash"
    args: ["-c", "echo 'This is standard output' && echo 'This is error output' >&2 && exit 1"]
    timeout: 10
"#;

    fs::write(&test_config, config_content).unwrap();

    let runner_script = repo_root.join("scripts/verification-runner.sh");
    let output = Command::new("bash")
        .arg(&runner_script)
        .arg("--config")
        .arg(&test_config)
        .arg("--fast")
        .arg("--verbose")
        .current_dir(&repo_root)
        .output()
        .expect("Failed to execute verification runner");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify both outputs are captured
    assert!(
        stdout.contains("This is standard output"),
        "Should capture stdout"
    );
    assert!(
        stdout.contains("This is error output"),
        "Should capture stderr"
    );
    assert!(stdout.contains("Output:"), "Should have 'Output:' section");
    assert!(stdout.contains("Errors:"), "Should have 'Errors:' section");

    println!("=== Output with both stdout and stderr ===");
    println!("{}", stdout);
}

#[test]
fn test_verification_runner_all_checks_run_despite_failures() {
    let test_dir = tempfile::tempdir().unwrap();
    let test_config = test_dir.path().join("test-config.yaml");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Create a marker file for each check to verify execution
    let marker_dir = test_dir.path().join("markers");
    fs::create_dir(&marker_dir).unwrap();

    let config_content = format!(
        r#"version: "1.0"

fast_lane:
  - name: "Check 1 (fails)"
    command: "bash"
    args: ["-c", "touch {marker_dir}/check1 && exit 1"]
    timeout: 10

  - name: "Check 2 (should still run)"
    command: "bash"
    args: ["-c", "touch {marker_dir}/check2 && exit 0"]
    timeout: 10

  - name: "Check 3 (also fails)"
    command: "bash"
    args: ["-c", "touch {marker_dir}/check3 && exit 1"]
    timeout: 10

  - name: "Check 4 (should still run)"
    command: "bash"
    args: ["-c", "touch {marker_dir}/check4 && exit 0"]
    timeout: 10
"#,
        marker_dir = marker_dir.display()
    );

    fs::write(&test_config, config_content).unwrap();

    let runner_script = repo_root.join("scripts/verification-runner.sh");
    Command::new("bash")
        .arg(&runner_script)
        .arg("--config")
        .arg(&test_config)
        .arg("--fast")
        .current_dir(&repo_root)
        .output()
        .expect("Failed to execute verification runner");

    // Verify all checks ran by checking for marker files
    assert!(
        marker_dir.join("check1").exists(),
        "Check 1 should have run despite failure"
    );
    assert!(
        marker_dir.join("check2").exists(),
        "Check 2 should have run after first failure"
    );
    assert!(
        marker_dir.join("check3").exists(),
        "Check 3 should have run despite second failure"
    );
    assert!(
        marker_dir.join("check4").exists(),
        "Check 4 should have run after third failure"
    );

    println!("All checks executed despite failures");
}

#[test]
fn test_verification_runner_json_report_structure() {
    let test_dir = tempfile::tempdir().unwrap();
    let test_config = test_dir.path().join("test-config.yaml");
    let json_output = test_dir.path().join("results.json");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let config_content = r#"version: "1.0"

fast_lane:
  - name: "Passing check"
    command: "true"
    args: []
    timeout: 10

  - name: "Failing check"
    command: "bash"
    args: ["-c", "echo 'Error output' >&2 && exit 1"]
    timeout: 10
"#;

    fs::write(&test_config, config_content).unwrap();

    let runner_script = repo_root.join("scripts/verification-runner.sh");
    Command::new("bash")
        .arg(&runner_script)
        .arg("--config")
        .arg(&test_config)
        .arg("--fast")
        .env("VERIFICATION_JSON_OUTPUT", "true")
        .env("VERIFICATION_JSON_PATH", &json_output)
        .current_dir(&repo_root)
        .output()
        .expect("Failed to execute verification runner");

    // Verify JSON file was created
    assert!(json_output.exists(), "JSON report should be generated");

    let json_content = fs::read_to_string(&json_output).expect("Failed to read JSON report");

    // Parse and verify JSON structure
    let json: serde_json::Value =
        serde_json::from_str(&json_content).expect("JSON should be valid");

    // Verify required fields
    assert!(json.get("lane").is_some(), "Should have 'lane' field");
    assert!(
        json.get("total_checks").is_some(),
        "Should have 'total_checks' field"
    );
    assert!(json.get("passed").is_some(), "Should have 'passed' field");
    assert!(json.get("failed").is_some(), "Should have 'failed' field");
    assert!(
        json.get("passed_checks").is_some(),
        "Should have 'passed_checks' array"
    );
    assert!(
        json.get("failed_checks").is_some(),
        "Should have 'failed_checks' array"
    );

    // Verify counts
    assert_eq!(json["total_checks"], 2, "Should report 2 total checks");
    assert_eq!(json["passed"], 1, "Should report 1 passed check");
    assert_eq!(json["failed"], 1, "Should report 1 failed check");

    // Verify failed checks structure
    let failed_checks = json["failed_checks"].as_array().unwrap();
    assert_eq!(failed_checks.len(), 1, "Should have 1 failed check");

    let failed = &failed_checks[0];
    assert!(
        failed.get("name").is_some(),
        "Failed check should have 'name'"
    );
    assert!(
        failed.get("exit_code").is_some(),
        "Failed check should have 'exit_code'"
    );
    assert!(
        failed.get("stdout").is_some(),
        "Failed check should have 'stdout'"
    );
    assert!(
        failed.get("stderr").is_some(),
        "Failed check should have 'stderr'"
    );
    assert!(
        failed.get("output").is_some(),
        "Failed check should have 'output'"
    );

    assert_eq!(failed["name"], "Failing check");
    assert_eq!(failed["exit_code"], 1);

    println!("=== JSON Report ===");
    println!("{}", serde_json::to_string_pretty(&json).unwrap());
}

#[test]
fn test_verification_runner_all_pass_exits_zero() {
    let test_dir = tempfile::tempdir().unwrap();
    let test_config = test_dir.path().join("test-config.yaml");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let config_content = r#"version: "1.0"

fast_lane:
  - name: "First passing check"
    command: "true"
    args: []
    timeout: 10

  - name: "Second passing check"
    command: "true"
    args: []
    timeout: 10

  - name: "Third passing check"
    command: "true"
    args: []
    timeout: 10
"#;

    fs::write(&test_config, config_content).unwrap();

    let runner_script = repo_root.join("scripts/verification-runner.sh");
    let output = Command::new("bash")
        .arg(&runner_script)
        .arg("--config")
        .arg(&test_config)
        .arg("--fast")
        .current_dir(&repo_root)
        .output()
        .expect("Failed to execute verification runner");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "Should exit with 0 when all checks pass"
    );

    assert!(
        stdout.contains("✓ All checks passed"),
        "Should show success message"
    );
    assert!(stdout.contains("Failed: 0"), "Should report 0 failures");

    println!("=== All Pass Output ===");
    println!("{}", stdout);
}

/// Helper function to extract a section from output
fn extract_section(output: &str, section_marker: &str) -> String {
    let mut in_section = false;
    let mut section_content = String::new();

    for line in output.lines() {
        if line.contains(section_marker) {
            in_section = true;
            continue;
        }
        if in_section {
            section_content.push_str(line);
            section_content.push('\n');
        }
    }

    section_content
}

#[test]
fn test_verification_runner_preserves_argument_boundaries_and_runs_both_lanes() {
    let test_dir = tempfile::tempdir().unwrap();
    let test_config = test_dir.path().join("test-config.yaml");
    let fast_marker = test_dir.path().join("fast marker.txt");
    let slow_marker = test_dir.path().join("slow marker.txt");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // The marker path and payload both contain spaces. A runner that rebuilds
    // the YAML list into one shell string will not pass these argv elements
    // correctly; direct argv execution will.
    let config_content = format!(
        r#"version: "1.0"

fast_lane:
  - name: "Fast argument boundary check"
    command: "bash"
    args: ["-c", "printf '%s' \"$1\" > \"$2\"", "--", "fast payload with spaces", "{fast_marker}"]
    timeout: 10

slow_lane:
  - name: "Slow lane marker"
    command: "bash"
    args: ["-c", "printf '%s' \"$1\" > \"$2\"", "--", "slow payload", "{slow_marker}"]
    timeout: 10
"#,
        fast_marker = fast_marker.display(),
        slow_marker = slow_marker.display(),
    );
    fs::write(&test_config, config_content).unwrap();

    let runner_script = repo_root.join("scripts/verification-runner.sh");
    let fast_output = Command::new("bash")
        .arg(&runner_script)
        .arg("--config")
        .arg(&test_config)
        .arg("--fast")
        .env("NO_COLOR", "1")
        .current_dir(&repo_root)
        .output()
        .expect("Failed to execute fast lane");

    assert!(
        fast_output.status.success(),
        "fast lane should pass: {}",
        String::from_utf8_lossy(&fast_output.stderr)
    );
    assert!(
        fast_marker.exists(),
        "fast check should receive its path as one argument"
    );
    assert_eq!(
        fs::read_to_string(&fast_marker).unwrap(),
        "fast payload with spaces"
    );
    assert!(
        !slow_marker.exists(),
        "--fast must not run slow lane checks"
    );

    let all_output = Command::new("bash")
        .arg(&runner_script)
        .arg("--config")
        .arg(&test_config)
        .arg("--all")
        .env("NO_COLOR", "1")
        .current_dir(&repo_root)
        .output()
        .expect("Failed to execute all lanes");
    let all_stdout = String::from_utf8_lossy(&all_output.stdout);

    assert!(
        all_output.status.success(),
        "all lanes should pass: {}",
        String::from_utf8_lossy(&all_output.stderr)
    );
    assert!(all_stdout.contains("Checks run: 2"));
    assert!(slow_marker.exists(), "--all must run slow lane checks");
}

#[test]
fn test_verification_runner_generates_empty_failures_json_when_all_pass() {
    let test_dir = tempfile::tempdir().unwrap();
    let test_config = test_dir.path().join("test-config.yaml");
    let json_output = test_dir.path().join("nested/results.json");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    fs::write(
        &test_config,
        r#"version: "1.0"
fast_lane:
  - name: "Passing check"
    command: "true"
    args: []
    timeout: 10
"#,
    )
    .unwrap();

    let output = Command::new("bash")
        .arg(repo_root.join("scripts/verification-runner.sh"))
        .arg("--config")
        .arg(&test_config)
        .arg("--fast")
        .arg("--json")
        .arg(&json_output)
        .env("NO_COLOR", "1")
        .current_dir(&repo_root)
        .output()
        .expect("Failed to execute verification runner");

    assert!(output.status.success());
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&json_output).unwrap()).unwrap();
    assert_eq!(report["totals"]["checks"], 1);
    assert_eq!(report["totals"]["failed"], 0);
    assert_eq!(report["failures"], serde_json::json!([]));
}

#[test]
fn test_verification_runner_records_explicit_bypass() {
    let test_dir = tempfile::tempdir().unwrap();
    let bypass_log = test_dir.path().join("bypasses.jsonl");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let output = Command::new("bash")
        .arg(repo_root.join("scripts/verification-runner.sh"))
        .arg("--fast")
        .arg("--no-verify")
        .env("NO_COLOR", "1")
        .env("VERIFICATION_BYPASS_LOG", &bypass_log)
        .current_dir(&repo_root)
        .output()
        .expect("Failed to execute bypass path");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Definition of Done bypass detected"));

    let bypass_contents = fs::read_to_string(&bypass_log).expect("bypass log should be written");
    let event: serde_json::Value =
        serde_json::from_str(bypass_contents.lines().next().unwrap()).unwrap();
    assert_eq!(event["pattern"], "--no-verify");
    assert_eq!(event["lanes_skipped"], serde_json::json!(["fast"]));
    assert!(event["timestamp"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert!(event["commit_sha"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
}
