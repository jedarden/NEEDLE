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
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

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

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
}
