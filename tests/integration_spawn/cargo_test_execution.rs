//! Process-backed coverage for [`needle::cargo_test::CargoTest`].
//!
//! These cases deliberately launch Cargo and therefore belong in the
//! `integration_spawn` target, not the process-free `--lib` gate.  Each fixture
//! owns its project directory, and related assertions share one project so the
//! nested Cargo invocations pay the compile cost once.

use needle::cargo_test::{CargoTest, TestArgs, TestMetrics};
use needle::telemetry::{Telemetry, TelemetryEvent};
use needle::types::BeadId;
use std::fs;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;

struct CargoProject {
    root: TempDir,
}

impl CargoProject {
    fn new(name: &str, source: &str) -> Self {
        let root = TempDir::new().expect("create isolated Cargo project");
        fs::write(
            root.path().join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ndoctest = false\n"
            ),
        )
        .expect("write Cargo.toml");
        fs::create_dir_all(root.path().join("src")).expect("create source directory");
        fs::write(root.path().join("src/lib.rs"), source).expect("write test crate");
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn runner(&self, filter: &str) -> CargoTest {
        CargoTest::with_args(self.path(), TestArgs::new().with_filter(filter))
    }
}

fn assert_output_files(workspace: &Path, name: &str) {
    let output_dir = workspace.join(".test_outputs").join(name);
    assert!(output_dir.is_dir(), "test output directory should exist");
    for file in ["stdout.txt", "stderr.txt", "combined.txt"] {
        let path = output_dir.join(file);
        assert!(path.is_file(), "{} should exist", path.display());
        fs::read_to_string(&path).expect("output file should be readable");
    }
}

fn read_events(log_dir: &Path) -> Vec<TelemetryEvent> {
    let mut events = Vec::new();
    for entry in fs::read_dir(log_dir).expect("read telemetry directory") {
        let path = entry.expect("read telemetry entry").path();
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let contents = fs::read_to_string(path).expect("read telemetry log");
        events.extend(
            contents
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("parse telemetry event")),
        );
    }
    events
}

#[test]
fn output_files_cover_success_output_and_empty_cases() {
    let project = CargoProject::new(
        "cargo-test-output-files",
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_example() { assert!(true); }

    #[test]
    fn test_with_output() { println!("Test output message"); }

    #[test]
    fn test_empty() {}
}
"#,
    );

    let success = project
        .runner("test_example")
        .run_with_output_files("test_example")
        .expect("run successful nested test");
    assert!(
        success.success() || success.exit_code.is_some(),
        "cargo test should complete with an exit code"
    );
    assert_output_files(project.path(), "test_example");

    project
        .runner("test_with_output")
        .run_with_output_files("test_with_output")
        .expect("run output-producing nested test");
    assert_output_files(project.path(), "test_with_output");
    let combined = fs::read_to_string(
        project
            .path()
            .join(".test_outputs/test_with_output/combined.txt"),
    )
    .expect("read combined output");
    assert!(
        combined.contains("=== STDOUT ===")
            || combined.contains("=== STDERR ===")
            || !combined.is_empty(),
        "combined output should have content or structure"
    );

    let empty = project
        .runner("test_empty")
        .run_with_output_files("test_empty")
        .expect("run quiet nested test");
    assert!(empty.success() || empty.exit_code.is_some());
    assert_output_files(project.path(), "test_empty");
}

#[test]
fn spawn_succeeds_and_captures_output_streams() {
    let project = CargoProject::new(
        "cargo-test-spawn-output",
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_spawn() {
        println!("STDOUT message");
        eprintln!("STDERR message");
    }
}
"#,
    );

    let result = project.runner("test_spawn").run();
    assert!(
        result.is_ok(),
        "cargo test spawn should succeed, got error: {:?}",
        result.err()
    );
    let outcome = result.expect("checked above");
    assert!(
        outcome.exit_code.is_some() || outcome.timed_out,
        "should have exit code or timeout flag"
    );
    assert!(
        !outcome.stdout.is_empty() || !outcome.stderr.is_empty(),
        "at least one output stream should be captured"
    );
}

#[test]
fn spawn_timeout_is_bounded() {
    let project = CargoProject::new("cargo-test-timeout", "");
    let outcome = CargoTest::new(project.path())
        .with_timeout(1)
        .run()
        .expect("timeout path returns an outcome");
    assert!(
        outcome.exit_code.is_some() || outcome.timed_out,
        "should have exit code or be marked as timed out"
    );
    assert!(
        outcome.duration.as_secs() < 10,
        "test should complete quickly, took {:?}",
        outcome.duration
    );
}

#[test]
fn bead_trace_covers_workspace_output_and_metrics_contracts() {
    let project = CargoProject::new(
        "cargo-test-bead-trace",
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_example() { assert!(true); }

    #[test]
    fn test_with_output() {
        println!("Test output message");
        eprintln!("Test error message");
    }

    #[test]
    fn test_empty() {}

    #[test]
    fn test_metrics_example() { assert!(true); }

    #[test]
    fn test_failing() { assert!(false, "intentional failure"); }
}
"#,
    );
    let beads_dir = project.path().join(".beads");

    project
        .runner("test_example")
        .run_with_bead_trace("bf-dir-test")
        .expect("run outside a bead workspace");
    assert!(
        !beads_dir.exists(),
        "trace capture must not create .beads in a non-bead workspace"
    );

    fs::create_dir_all(&beads_dir).expect("make fixture a bead workspace");
    let basic = project
        .runner("test_example")
        .run_with_bead_trace("bf-test-123")
        .expect("capture basic trace");
    assert!(basic.success() || basic.exit_code.is_some());
    let basic_trace = beads_dir.join("traces/bf-test-123");
    assert!(basic_trace.is_dir());
    assert!(basic_trace.join("stdout.txt").is_file());
    assert!(basic_trace.join("stderr.txt").is_file());
    assert!(
        !fs::read_to_string(basic_trace.join("stdout.txt"))
            .expect("read trace stdout")
            .is_empty()
            || !fs::read_to_string(basic_trace.join("stderr.txt"))
                .expect("read trace stderr")
                .is_empty(),
        "at least one output file should have content"
    );

    project
        .runner("test_with_output")
        .run_with_bead_trace("bf-output-test")
        .expect("capture output trace");
    let output_trace = beads_dir.join("traces/bf-output-test");
    assert!(output_trace.join("stdout.txt").is_file());
    assert!(output_trace.join("stderr.txt").is_file());
    assert!(
        !fs::read_to_string(output_trace.join("stdout.txt"))
            .expect("read output trace stdout")
            .is_empty()
            || !fs::read_to_string(output_trace.join("stderr.txt"))
                .expect("read output trace stderr")
                .is_empty(),
        "trace files should contain test output"
    );

    let empty = project
        .runner("test_empty")
        .run_with_bead_trace("bf-empty-test")
        .expect("capture quiet trace");
    assert!(empty.success() || empty.exit_code.is_some());
    let empty_trace = beads_dir.join("traces/bf-empty-test");
    assert!(empty_trace.is_dir());
    assert!(empty_trace.join("stdout.txt").is_file());
    assert!(empty_trace.join("stderr.txt").is_file());

    project
        .runner("test_metrics_example")
        .run_with_bead_trace("bf-metrics-test")
        .expect("capture metrics trace");
    let metrics: TestMetrics = serde_json::from_str(
        &fs::read_to_string(beads_dir.join("traces/bf-metrics-test/test_metrics.json"))
            .expect("read trace metrics"),
    )
    .expect("parse trace metrics");
    assert!(metrics.test_name.contains("cargo_test_"));
    assert!(metrics.test_name.contains("bf-metrics-test"));
    assert!(metrics.duration_ms > 0 || metrics.timed_out);
    assert!(
        chrono::Utc::now()
            .signed_duration_since(metrics.timestamp)
            .num_seconds()
            < 60
    );
    assert_eq!(
        metrics.success(),
        metrics.exit_code == Some(0) && !metrics.timed_out
    );

    let failed = project
        .runner("test_failing")
        .run_with_bead_trace("bf-exit-code-test")
        .expect("capture failing trace");
    let failure_metrics: TestMetrics = serde_json::from_str(
        &fs::read_to_string(beads_dir.join("traces/bf-exit-code-test/test_metrics.json"))
            .expect("read failing trace metrics"),
    )
    .expect("parse failing trace metrics");
    assert!(
        failure_metrics.exit_code.unwrap_or(0) != 0 || !failed.success(),
        "exit code should reflect test failure"
    );

    project
        .runner("test_example")
        .run_with_bead_trace("bf-dir-test")
        .expect("capture trace after bead workspace creation");
    assert!(beads_dir.join("traces/bf-dir-test").is_dir());
}

#[tokio::test]
async fn start_telemetry_survives_command_failure_to_start() {
    let temp = TempDir::new().expect("create isolated telemetry fixture");
    let log_dir = temp.path().join("logs");
    let telemetry = Telemetry::with_log_dir("cargo-test-start-tel".to_string(), &log_dir);
    telemetry.start();
    let missing = temp.path().join("no-such-workspace");

    let outcome = CargoTest::new(&missing)
        .with_telemetry(telemetry.clone())
        .with_bead_id(BeadId::from("needle-d6864d47"))
        .run();
    assert!(outcome.is_err());

    telemetry
        .force_flush_async(Duration::from_secs(2))
        .await
        .expect("flush telemetry");
    let entries: Vec<_> = read_events(&log_dir)
        .into_iter()
        .filter(|event| event.event_type == "log.entry")
        .collect();
    telemetry.shutdown().await;

    assert_eq!(entries.len(), 1, "exactly one start entry expected");
    let entry = &entries[0];
    assert_eq!(entry.data["phase"], "test_execution");
    assert_eq!(entry.data["level"], "info");
    assert_eq!(entry.data["bead_id"], "needle-d6864d47");
    let context = &entry.data["context"];
    let launch_ts = context["launch_timestamp"]
        .as_str()
        .expect("launch timestamp is a string");
    chrono::DateTime::parse_from_rfc3339(launch_ts)
        .expect("launch timestamp must be valid RFC 3339");
    assert_eq!(context["workspace"], missing.display().to_string());
    assert!(context["args"].as_array().is_some());
    assert_eq!(context["timeout_secs"], 600);
}

#[tokio::test]
async fn start_telemetry_timestamp_matches_outcome_record() {
    let temp = TempDir::new().expect("create isolated telemetry fixture");
    let log_dir = temp.path().join("logs");
    fs::write(temp.path().join("Cargo.toml"), "this is not valid toml =")
        .expect("write invalid manifest");
    let telemetry = Telemetry::with_log_dir("cargo-test-start-tel".to_string(), &log_dir);
    telemetry.start();

    let outcome = CargoTest::new(temp.path())
        .with_telemetry(telemetry.clone())
        .run()
        .expect("Cargo returns a failed outcome for an invalid manifest");
    assert!(!outcome.success());

    telemetry
        .force_flush_async(Duration::from_secs(2))
        .await
        .expect("flush telemetry");
    let entries: Vec<_> = read_events(&log_dir)
        .into_iter()
        .filter(|event| event.event_type == "log.entry")
        .collect();
    telemetry.shutdown().await;
    assert_eq!(entries.len(), 1, "exactly one start entry expected");
    assert_eq!(
        entries[0].data["context"]["launch_timestamp"],
        outcome.launch_timestamp
    );
}

#[test]
fn start_telemetry_is_noop_without_an_emitter() {
    let temp = TempDir::new().expect("create isolated no-emitter fixture");
    let missing = temp.path().join("no-such-workspace");
    let log_dir = temp.path().join("logs");
    assert!(CargoTest::new(&missing).run().is_err());
    assert!(
        !log_dir.exists(),
        "a runner without telemetry must not create a telemetry sink"
    );
}
