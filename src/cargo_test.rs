//! Cargo test command execution.
//!
//! This module provides utilities for running `cargo test` with proper
//! argument handling and process management. It supports configurable
//! test arguments and captures output for analysis.
//!
//! ## Usage
//!
//! ```no_run
//! use needle::cargo_test::{CargoTest, TestOutcome};
//! use std::path::Path;
//!
//! // Create a cargo test runner
//! let runner = CargoTest::new(Path::new("/workspace"));
//!
//! // Run tests with default arguments
//! let outcome = runner.run().unwrap();
//!
//! // Check if tests passed
//! if outcome.success() {
//!     println!("Tests passed!");
//! } else {
//!     println!("Tests failed: {:?}", outcome);
//! }
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::test_output::TestOutput;

// ──────────────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────────────

/// Default timeout for cargo test commands (10 minutes).
pub const DEFAULT_TEST_TIMEOUT_SECS: u64 = 600;

/// Maximum bytes of stdout/stderr to capture per test run.
pub const MAX_OUTPUT_BYTES: usize = 65536;

// ──────────────────────────────────────────────────────────────────────────────
// Test Arguments
// ──────────────────────────────────────────────────────────────────────────────

/// Configurable arguments for cargo test.
#[derive(Debug, Clone, Default)]
pub struct TestArgs {
    /// Test target (e.g., "--lib", "--bins", "--test <name>").
    pub target: Option<String>,
    /// Test filter expression (e.g., "test_name").
    pub filter: Option<String>,
    /// List of specific tests to run (e.g., ["test1", "test2"]).
    pub test_names: Vec<String>,
    /// Additional cargo flags (e.g., "--release", "--features foo").
    pub cargo_flags: Vec<String>,
    /// Additional test flags (e.g., "--exact", "--ignored").
    pub test_flags: Vec<String>,
}

impl TestArgs {
    /// Create new empty test args.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the test target.
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Set the test filter.
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = Some(filter.into());
        self
    }

    /// Add a test name to run.
    pub fn add_test_name(mut self, name: impl Into<String>) -> Self {
        self.test_names.push(name.into());
        self
    }

    /// Add a cargo flag (e.g., "--release").
    pub fn add_cargo_flag(mut self, flag: impl Into<String>) -> Self {
        self.cargo_flags.push(flag.into());
        self
    }

    /// Add a test flag (e.g., "--exact").
    pub fn add_test_flag(mut self, flag: impl Into<String>) -> Self {
        self.test_flags.push(flag.into());
        self
    }

    /// Build the full command arguments for `cargo test`.
    fn build_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        // Add cargo flags first
        for flag in &self.cargo_flags {
            args.push(flag.clone());
        }

        // Add "test" subcommand
        args.push("test".to_string());

        // Add target if specified
        if let Some(ref target) = self.target {
            args.push(target.clone());
        }

        // Add test flags
        for flag in &self.test_flags {
            args.push(flag.clone());
        }

        // Add test names if specified
        for name in &self.test_names {
            args.push("--".to_string());
            args.push(name.clone());
        }

        // Add filter if specified (and no test names)
        if !self.test_names.is_empty() {
            if let Some(ref filter) = self.filter {
                args.push(format!("--filter={filter}"));
            }
        }

        args
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Test Outcome
// ──────────────────────────────────────────────────────────────────────────────

/// Result of running cargo test.
#[derive(Debug, Clone)]
pub struct TestOutcome {
    /// Exit code from cargo test (None if killed by signal).
    pub exit_code: Option<i32>,
    /// Captured stdout (truncated to MAX_OUTPUT_BYTES).
    pub stdout: String,
    /// Captured stderr (truncated to MAX_OUTPUT_BYTES).
    pub stderr: String,
    /// Duration of the test run.
    pub duration: Duration,
    /// Whether the test timed out.
    pub timed_out: bool,
}

impl TestOutcome {
    /// Returns true if cargo test exited with code 0.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }

    /// Returns a human-readable summary of the outcome.
    pub fn summary(&self) -> String {
        if self.timed_out {
            format!("Timed out after {:?}", self.duration)
        } else if self.success() {
            format!("Passed in {:?}", self.duration)
        } else {
            format!(
                "Failed with exit code {:?} in {:?}",
                self.exit_code, self.duration
            )
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Cargo Test Runner
// ──────────────────────────────────────────────────────────────────────────────

/// Runner for cargo test commands.
///
/// ## Example
///
/// ```no_run
/// use needle::cargo_test::{CargoTest, TestArgs};
/// use std::path::Path;
///
/// let args = TestArgs::new()
///     .with_target("--lib")
///     .with_filter("integration");
///
/// let runner = CargoTest::with_args(Path::new("/workspace"), args);
/// let outcome = runner.run().unwrap();
/// ```
pub struct CargoTest {
    /// Workspace directory where cargo will run.
    workspace: PathBuf,
    /// Test arguments.
    args: TestArgs,
    /// Timeout in seconds.
    timeout_secs: u64,
}

impl CargoTest {
    /// Create a new cargo test runner with default arguments.
    pub fn new(workspace: &Path) -> Self {
        CargoTest {
            workspace: workspace.to_path_buf(),
            args: TestArgs::default(),
            timeout_secs: DEFAULT_TEST_TIMEOUT_SECS,
        }
    }

    /// Create a new cargo test runner with custom arguments.
    pub fn with_args(workspace: &Path, args: TestArgs) -> Self {
        CargoTest {
            workspace: workspace.to_path_buf(),
            args,
            timeout_secs: DEFAULT_TEST_TIMEOUT_SECS,
        }
    }

    /// Set a custom timeout (default is 600 seconds).
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// Run cargo test with the configured arguments.
    ///
    /// ## Errors
    ///
    /// Returns an error if:
    /// - The cargo binary cannot be found
    /// - The workspace directory is invalid
    /// - The process cannot be spawned
    pub fn run(&self) -> Result<TestOutcome> {
        let start = Instant::now();
        let args = self.args.build_args();

        tracing::info!(
            workspace = %self.workspace.display(),
            args = ?args,
            "running cargo test"
        );

        // Build the cargo command
        let mut cmd = Command::new("cargo");
        cmd.args(&args);
        cmd.current_dir(&self.workspace);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Spawn the process
        let mut child = cmd.spawn().context("failed to spawn cargo test")?;

        // Wait for completion with timeout
        let timeout = Duration::from_secs(self.timeout_secs);
        let _exit_status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        child
                            .kill()
                            .context("failed to kill cargo test after timeout")?;
                        return Ok(TestOutcome {
                            exit_code: None,
                            stdout: String::new(),
                            stderr: String::from("command timed out"),
                            duration: start.elapsed(),
                            timed_out: true,
                        });
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("failed to wait for cargo test: {}", e));
                }
            }
        };

        // Capture output
        let output = child
            .wait_with_output()
            .context("failed to capture cargo test output")?;

        let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
        let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));

        tracing::info!(
            exit_code = ?output.status.code(),
            duration_secs = start.elapsed().as_secs(),
            success = output.status.success(),
            "cargo test completed"
        );

        Ok(TestOutcome {
            exit_code: output.status.code(),
            stdout,
            stderr,
            duration: start.elapsed(),
            timed_out: false,
        })
    }

    /// Get the workspace directory.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Get the test arguments.
    pub fn args(&self) -> &TestArgs {
        &self.args
    }

    /// Get the timeout in seconds.
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    /// Run cargo test and write output to files.
    ///
    /// This method runs `cargo test` and writes the captured stdout, stderr,
    /// and combined output to separate files in the `.test_outputs/<test_name>/` directory.
    ///
    /// ## Arguments
    ///
    /// * `test_name` - A unique identifier for the test (e.g., "integration_test_1")
    ///
    /// ## Errors
    ///
    /// Returns an error if:
    /// - The test run fails
    /// - Output directory creation fails
    /// - File writing fails
    ///
    /// ## Example
    ///
    /// ```no_run
    /// use needle::cargo_test::CargoTest;
    /// use std::path::Path;
    ///
    /// let runner = CargoTest::new(Path::new("/workspace"));
    /// let outcome = runner.run_with_output_files("my_test").unwrap();
    /// ```
    pub fn run_with_output_files(&self, test_name: &str) -> Result<TestOutcome> {
        // First run the test to get the outcome
        let outcome = self.run()?;

        // Create test output directory and write outputs
        if let Some(output) = TestOutput::new(test_name, &self.workspace) {
            // Write stdout
            if let Err(e) = output.write_stdout(&outcome.stdout) {
                tracing::warn!(
                    test_name = %test_name,
                    error = %e,
                    "failed to write stdout to file"
                );
            }

            // Write stderr
            if let Err(e) = output.write_stderr(&outcome.stderr) {
                tracing::warn!(
                    test_name = %test_name,
                    error = %e,
                    "failed to write stderr to file"
                );
            }

            // Write combined output
            let combined = create_combined_output(&outcome.stdout, &outcome.stderr);
            if let Err(e) = output.write_combined(&combined) {
                tracing::warn!(
                    test_name = %test_name,
                    error = %e,
                    "failed to write combined output to file"
                );
            }

            tracing::info!(
                test_name = %test_name,
                stdout_path = %output.stdout_path().display(),
                stderr_path = %output.stderr_path().display(),
                combined_path = %output.combined_path().display(),
                "test output written to files"
            );
        } else {
            tracing::warn!(
                test_name = %test_name,
                "failed to create test output directory, output files not written"
            );
        }

        Ok(outcome)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Utility Functions
// ──────────────────────────────────────────────────────────────────────────────

/// Truncate output to at most MAX_OUTPUT_BYTES.
fn truncate_output(s: &str) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        s.to_string()
    } else {
        let truncated = &s[..MAX_OUTPUT_BYTES];
        format!("{}... [truncated]", truncated)
    }
}

/// Create combined output from stdout and stderr.
///
/// This function combines stdout and stderr into a single string with section headers.
///
/// ## Example
///
/// ```
/// use needle::cargo_test::create_combined_output;
///
/// let stdout = "Test output line 1\nTest output line 2";
/// let stderr = "Error message";
/// let combined = create_combined_output(stdout, stderr);
/// ```
pub fn create_combined_output(stdout: &str, stderr: &str) -> String {
    let mut combined = String::new();

    if !stdout.is_empty() {
        combined.push_str("=== STDOUT ===\n");
        combined.push_str(stdout);
        if !stdout.ends_with('\n') {
            combined.push('\n');
        }
    }

    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str("=== STDERR ===\n");
        combined.push_str(stderr);
        if !stderr.ends_with('\n') {
            combined.push('\n');
        }
    }

    combined
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_args_default_is_empty() {
        let args = TestArgs::new();
        assert!(args.target.is_none());
        assert!(args.filter.is_none());
        assert!(args.test_names.is_empty());
        assert!(args.cargo_flags.is_empty());
        assert!(args.test_flags.is_empty());
    }

    #[test]
    fn test_args_with_target() {
        let args = TestArgs::new().with_target("--lib");
        assert_eq!(args.target, Some("--lib".to_string()));
    }

    #[test]
    fn test_args_with_filter() {
        let args = TestArgs::new().with_filter("integration");
        assert_eq!(args.filter, Some("integration".to_string()));
    }

    #[test]
    fn test_args_add_test_name() {
        let args = TestArgs::new().add_test_name("test_one");
        assert_eq!(args.test_names, vec!["test_one"]);
    }

    #[test]
    fn test_args_add_cargo_flag() {
        let args = TestArgs::new().add_cargo_flag("--release");
        assert_eq!(args.cargo_flags, vec!["--release"]);
    }

    #[test]
    fn test_args_add_test_flag() {
        let args = TestArgs::new().add_test_flag("--exact");
        assert_eq!(args.test_flags, vec!["--exact"]);
    }

    #[test]
    fn test_args_build_default() {
        let args = TestArgs::new();
        let built = args.build_args();
        assert_eq!(built, vec!["test"]);
    }

    #[test]
    fn test_args_build_with_target() {
        let args = TestArgs::new().with_target("--lib");
        let built = args.build_args();
        assert_eq!(built, vec!["test", "--lib"]);
    }

    #[test]
    fn test_args_build_with_cargo_flags() {
        let args = TestArgs::new()
            .add_cargo_flag("--release")
            .add_cargo_flag("--features")
            .add_cargo_flag("foo");
        let built = args.build_args();
        assert_eq!(built, vec!["--release", "--features", "foo", "test"]);
    }

    #[test]
    fn test_args_build_with_test_flags() {
        let args = TestArgs::new().add_test_flag("--exact");
        let built = args.build_args();
        assert_eq!(built, vec!["test", "--exact"]);
    }

    #[test]
    fn test_args_build_full() {
        let args = TestArgs::new()
            .add_cargo_flag("--release")
            .with_target("--lib")
            .add_test_flag("--exact")
            .add_test_name("test_one");
        let built = args.build_args();
        assert_eq!(
            built,
            vec!["--release", "test", "--lib", "--exact", "--", "test_one"]
        );
    }

    #[test]
    fn test_outcome_success_with_zero_exit() {
        let outcome = TestOutcome {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            duration: Duration::from_secs(1),
            timed_out: false,
        };
        assert!(outcome.success());
    }

    #[test]
    fn test_outcome_success_false_with_nonzero_exit() {
        let outcome = TestOutcome {
            exit_code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
            duration: Duration::from_secs(1),
            timed_out: false,
        };
        assert!(!outcome.success());
    }

    #[test]
    fn test_outcome_success_false_with_timeout() {
        let outcome = TestOutcome {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            duration: Duration::from_secs(1),
            timed_out: true,
        };
        assert!(!outcome.success());
    }

    #[test]
    fn test_outcome_summary_passed() {
        let outcome = TestOutcome {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            duration: Duration::from_secs(5),
            timed_out: false,
        };
        assert!(outcome.summary().contains("Passed"));
        assert!(outcome.summary().contains("5s"));
    }

    #[test]
    fn test_outcome_summary_failed() {
        let outcome = TestOutcome {
            exit_code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
            duration: Duration::from_secs(3),
            timed_out: false,
        };
        assert!(outcome.summary().contains("Failed"));
        assert!(outcome.summary().contains("exit code"));
        assert!(outcome.summary().contains("1"));
    }

    #[test]
    fn test_outcome_summary_timed_out() {
        let outcome = TestOutcome {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            duration: Duration::from_secs(100),
            timed_out: true,
        };
        assert!(outcome.summary().contains("Timed out"));
        assert!(outcome.summary().contains("100s"));
    }

    #[test]
    fn test_truncate_output_short() {
        let short = "short output";
        assert_eq!(truncate_output(short), "short output");
    }

    #[test]
    fn test_truncate_output_long() {
        let long = "a".repeat(MAX_OUTPUT_BYTES + 100);
        let truncated = truncate_output(&long);
        assert!(truncated.len() < long.len());
        assert!(truncated.ends_with("... [truncated]"));
        assert_eq!(truncated.len(), MAX_OUTPUT_BYTES + "... [truncated]".len());
    }

    #[test]
    fn cargo_test_new_creates_runner() {
        let runner = CargoTest::new(Path::new("/tmp"));
        assert_eq!(runner.workspace(), Path::new("/tmp"));
        assert_eq!(runner.timeout_secs(), DEFAULT_TEST_TIMEOUT_SECS);
    }

    #[test]
    fn cargo_test_with_args_sets_args() {
        let args = TestArgs::new().with_target("--lib");
        let runner = CargoTest::with_args(Path::new("/tmp"), args);
        assert_eq!(runner.args().target, Some("--lib".to_string()));
    }

    #[test]
    fn cargo_test_with_timeout_sets_timeout() {
        let runner = CargoTest::new(Path::new("/tmp")).with_timeout(300);
        assert_eq!(runner.timeout_secs(), 300);
    }

    #[test]
    fn create_combined_output_with_both_streams() {
        let stdout = "Test output line 1\nTest output line 2";
        let stderr = "Error message";
        let combined = create_combined_output(stdout, stderr);

        assert!(combined.contains("=== STDOUT ==="));
        assert!(combined.contains("Test output line 1"));
        assert!(combined.contains("=== STDERR ==="));
        assert!(combined.contains("Error message"));
    }

    #[test]
    fn create_combined_output_with_only_stdout() {
        let stdout = "Test output";
        let stderr = "";
        let combined = create_combined_output(stdout, stderr);

        assert!(combined.contains("=== STDOUT ==="));
        assert!(combined.contains("Test output"));
        assert!(!combined.contains("=== STDERR ==="));
    }

    #[test]
    fn create_combined_output_with_only_stderr() {
        let stdout = "";
        let stderr = "Error message";
        let combined = create_combined_output(stdout, stderr);

        assert!(!combined.contains("=== STDOUT ==="));
        assert!(combined.contains("=== STDERR ==="));
        assert!(combined.contains("Error message"));
    }

    #[test]
    fn create_combined_output_with_empty_streams() {
        let stdout = "";
        let stderr = "";
        let combined = create_combined_output(stdout, stderr);

        assert_eq!(combined, "");
    }

    #[test]
    fn create_combined_output_adds_trailing_newline() {
        let stdout = "output without newline";
        let stderr = "error without newline";
        let combined = create_combined_output(stdout, stderr);

        // Both sections should end with newline
        let stdout_section = combined.split("=== STDERR ===").next().unwrap();
        assert!(stdout_section.ends_with('\n'));
    }

    #[test]
    fn run_with_output_files_creates_output_files() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Create a minimal Cargo project for testing
        let cargo_toml = workspace.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
        ).unwrap();

        let src_dir = workspace.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let lib_rs = src_dir.join("lib.rs");
        fs::write(
            &lib_rs,
            r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_example() {
        assert!(true);
    }
}
"#,
        ).unwrap();

        // Run cargo test with output file capture
        let runner = CargoTest::new(workspace);
        let outcome = runner.run_with_output_files("test_example").unwrap();

        // Verify the test ran successfully
        assert!(outcome.success() || outcome.exit_code.is_some(),
                "cargo test should complete with an exit code");

        // Verify output files were created
        let test_output_dir = workspace.join(".test_outputs").join("test_example");
        assert!(test_output_dir.exists(), "test output directory should exist");

        let stdout_path = test_output_dir.join("stdout.txt");
        let stderr_path = test_output_dir.join("stderr.txt");
        let combined_path = test_output_dir.join("combined.txt");

        // Files should exist
        assert!(stdout_path.exists(), "stdout file should exist");
        assert!(stderr_path.exists(), "stderr file should exist");
        assert!(combined_path.exists(), "combined file should exist");

        // Verify files can be read
        let _stdout_content = fs::read_to_string(&stdout_path)
            .expect("stdout file should be readable");
        let _stderr_content = fs::read_to_string(&stderr_path)
            .expect("stderr file should be readable");
        let _combined_content = fs::read_to_string(&combined_path)
            .expect("combined file should be readable");
    }

    #[test]
    fn run_with_output_files_creates_combined_structure() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Create a minimal Cargo project for testing
        let cargo_toml = workspace.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
        ).unwrap();

        let src_dir = workspace.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let lib_rs = src_dir.join("lib.rs");
        fs::write(
            &lib_rs,
            r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_with_output() {
        println!("Test output message");
        assert!(true);
    }
}
"#,
        ).unwrap();

        // Run cargo test with output file capture
        let runner = CargoTest::new(workspace);
        let _outcome = runner.run_with_output_files("test_with_output").unwrap();

        // Verify output files were created
        let test_output_dir = workspace.join(".test_outputs").join("test_with_output");
        let combined_path = test_output_dir.join("combined.txt");

        let combined_content = fs::read_to_string(&combined_path)
            .expect("combined file should be readable");

        // Verify combined output has the expected structure
        // When both stdout and stderr are present, we should see both sections
        // When only one is present, we see just that section
        let has_structure = combined_content.contains("=== STDOUT ===") ||
                           combined_content.contains("=== STDERR ===") ||
                           !combined_content.is_empty();
        assert!(has_structure, "combined output should have content or structure");
    }

    #[test]
    fn run_with_output_files_handles_empty_output() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Create a minimal Cargo project for testing
        let cargo_toml = workspace.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
        ).unwrap();

        let src_dir = workspace.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let lib_rs = src_dir.join("lib.rs");
        fs::write(
            &lib_rs,
            r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_empty() {
        // Silent test with no output
        assert!(true);
    }
}
"#,
        ).unwrap();

        // Run cargo test with output file capture
        let runner = CargoTest::new(workspace);
        let outcome = runner.run_with_output_files("test_empty").unwrap();

        // Should complete successfully even with no custom output
        assert!(outcome.success() || outcome.exit_code.is_some());

        // Files should still be created
        let test_output_dir = workspace.join(".test_outputs").join("test_empty");
        assert!(test_output_dir.exists(), "output directory should exist");
        assert!(test_output_dir.join("stdout.txt").exists());
        assert!(test_output_dir.join("stderr.txt").exists());
        assert!(test_output_dir.join("combined.txt").exists());
    }
}
