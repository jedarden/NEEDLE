//! Basic test runner module for executing cargo test commands.
//!
//! This module provides a simple interface for running cargo test
//! with proper process spawning, output capture, and result handling.
//!
//! ## Usage
//!
//! ```no_run
//! use needle::test_runner::{TestRunner, TestResult};
//! use std::path::Path;
//!
//! let runner = TestRunner::new(Path::new("/workspace"));
//! match runner.run_tests(&[]) {
//!     Ok(result) => {
//!         println!("Exit code: {:?}", result.exit_code);
//!         println!("Stdout: {}", result.stdout);
//!         println!("Stderr: {}", result.stderr);
//!         if result.success() {
//!             println!("Tests passed!");
//!         }
//!     },
//!     Err(e) => println!("Error running tests: {}", e),
//! }
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

// ──────────────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────────────

/// Default timeout for cargo test commands (5 minutes).
const DEFAULT_TEST_TIMEOUT_SECS: u64 = 300;

// ──────────────────────────────────────────────────────────────────────────────
// Captured Output
// ──────────────────────────────────────────────────────────────────────────────

/// Captured output from test execution.
///
/// Holds the stdout and stderr streams captured from the cargo test process.
#[derive(Debug, Clone)]
pub struct CapturedOutput {
    /// Captured stdout from test execution.
    pub stdout: String,
    /// Captured stderr from test execution.
    pub stderr: String,
}

impl CapturedOutput {
    /// Create new captured output from raw bytes.
    pub fn new(stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self {
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
        }
    }

    /// Create empty captured output.
    pub fn empty() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    /// Returns true if both stdout and stderr are empty.
    pub fn is_empty(&self) -> bool {
        self.stdout.is_empty() && self.stderr.is_empty()
    }

    /// Returns the combined length of stdout and stderr in bytes.
    pub fn total_len(&self) -> usize {
        self.stdout.len() + self.stderr.len()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Test Result
// ──────────────────────────────────────────────────────────────────────────────

/// Result of running cargo test.
#[derive(Debug, Clone)]
pub struct TestResult {
    /// The test execution status.
    pub status: TestStatus,
    /// Captured stdout from test execution.
    pub stdout: String,
    /// Captured stderr from test execution.
    pub stderr: String,
    /// Exit code from cargo test (None if killed by signal).
    pub exit_code: Option<i32>,
}

/// The status of test execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestStatus {
    /// All tests passed successfully.
    Success,
    /// Some tests failed.
    Failed,
    /// Compilation failed.
    CompilationFailed,
    /// Test execution timed out.
    TimedOut,
}

impl TestResult {
    /// Create a new test result from process output.
    fn from_output(output: Output) -> Self {
        let exit_code = output.status.code();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let status = Self::classify_status(&output);

        Self {
            status,
            stdout,
            stderr,
            exit_code,
        }
    }

    /// Create a timeout test result.
    fn timeout() -> Self {
        Self {
            status: TestStatus::TimedOut,
            stdout: String::new(),
            stderr: String::from("command timed out"),
            exit_code: None,
        }
    }

    /// Classify the test status based on process output.
    fn classify_status(output: &Output) -> TestStatus {
        let exit_code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Check for compilation errors
        if stderr.contains("error[E") || stderr.contains("error: aborting") {
            return TestStatus::CompilationFailed;
        }

        // Check exit code
        match exit_code {
            Some(0) => TestStatus::Success,
            Some(101) => {
                // Exit code 101 indicates test failures
                if stderr.contains("test result:") {
                    TestStatus::Failed
                } else {
                    TestStatus::CompilationFailed
                }
            }
            Some(code) => {
                tracing::warn!(exit_code = code, "unexpected exit code from cargo test");
                TestStatus::Failed
            }
            None => {
                tracing::warn!("no exit code available from cargo test");
                TestStatus::Failed
            }
        }
    }

    /// Returns true if the test result indicates success.
    pub fn is_success(&self) -> bool {
        matches!(self.status, TestStatus::Success)
    }

    /// Returns true if the test result indicates failure.
    pub fn is_failure(&self) -> bool {
        !self.is_success()
    }

    /// Returns true if the failure was due to compilation errors.
    pub fn is_compilation_failure(&self) -> bool {
        matches!(self.status, TestStatus::CompilationFailed)
    }

    /// Returns true if the test timed out.
    pub fn is_timed_out(&self) -> bool {
        matches!(self.status, TestStatus::TimedOut)
    }

    /// Returns a human-readable summary of the result.
    pub fn summary(&self) -> String {
        match &self.status {
            TestStatus::TimedOut => "Test execution timed out".to_string(),
            TestStatus::CompilationFailed => "Compilation failed".to_string(),
            TestStatus::Success => "All tests passed".to_string(),
            TestStatus::Failed => {
                format!("Tests failed with exit code {:?}", self.exit_code)
            }
        }
    }

    /// Get the captured stdout.
    pub fn captured_stdout(&self) -> &str {
        &self.stdout
    }

    /// Get the captured stderr.
    pub fn captured_stderr(&self) -> &str {
        &self.stderr
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Test Runner
// ──────────────────────────────────────────────────────────────────────────────

/// Basic test runner for executing cargo test commands.
#[derive(Debug, Clone)]
pub struct TestRunner {
    /// Workspace directory where tests will be run.
    workspace: PathBuf,
    /// Timeout in seconds for test execution.
    timeout_secs: u64,
    /// Additional arguments to pass to cargo test.
    extra_args: Vec<String>,
}

impl TestRunner {
    /// Create a new test runner for the given workspace.
    ///
    /// ## Arguments
    ///
    /// * `workspace` - Path to the workspace directory
    pub fn new(workspace: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
            timeout_secs: DEFAULT_TEST_TIMEOUT_SECS,
            extra_args: Vec::new(),
        }
    }

    /// Set the timeout for test execution.
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// Add extra arguments to pass to cargo test.
    pub fn with_extra_arg(mut self, arg: impl Into<String>) -> Self {
        self.extra_args.push(arg.into());
        self
    }

    /// Run cargo test with the given additional arguments.
    ///
    /// ## Arguments
    ///
    /// * `args` - Additional arguments to pass to cargo test
    ///
    /// ## Returns
    ///
    /// Returns `TestResult` containing:
    /// - `status`: The test execution status (Success, Failed, CompilationFailed, TimedOut)
    /// - `stdout`: Captured stdout from test execution
    /// - `stderr`: Captured stderr from test execution
    /// - `exit_code`: Process exit code (None if killed by signal)
    ///
    /// ## Errors
    ///
    /// Returns an error if:
    /// - The cargo binary cannot be found
    /// - The process fails to spawn
    pub fn run_tests(&self, args: &[&str]) -> Result<TestResult> {
        let start = Instant::now();

        // Build the command
        let mut cmd = Command::new("cargo");
        cmd.arg("test");
        cmd.current_dir(&self.workspace);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Add extra arguments
        for arg in &self.extra_args {
            cmd.arg(arg);
        }

        // Add additional arguments
        for arg in args {
            cmd.arg(arg);
        }

        // Spawn the process
        tracing::debug!(
            workspace = %self.workspace.display(),
            args = ?args,
            "spawning cargo test process"
        );

        let result = self.execute_with_timeout(cmd)?;

        // Build the test result with captured output
        let test_result = match result {
            Some(output) => TestResult::from_output(output),
            None => TestResult::timeout(),
        };

        let duration = start.elapsed();

        tracing::info!(
            status = ?test_result.status,
            duration_secs = duration.as_secs(),
            stdout_len = test_result.stdout.len(),
            stderr_len = test_result.stderr.len(),
            "cargo test completed"
        );

        Ok(test_result)
    }

    /// Execute a command with timeout protection.
    ///
    /// Returns Ok(Some(output)) if the command completes within timeout,
    /// Ok(None) if the command times out, or Err if the process fails to spawn.
    fn execute_with_timeout(&self, mut cmd: Command) -> Result<Option<Output>> {
        let timeout = Duration::from_secs(self.timeout_secs);

        // Spawn the process
        let mut child = cmd
            .spawn()
            .context("failed to spawn cargo test process")?;

        // Wait for completion with timeout
        let start = Instant::now();
        loop {
            // Check if timeout exceeded
            if start.elapsed() > timeout {
                tracing::error!(
                    timeout_secs = self.timeout_secs,
                    "cargo test timed out"
                );

                // Kill the child process
                let _ = child.kill();
                let _ = child.wait();

                return Ok(None);
            }

            // Try to wait with a small timeout
            match child.try_wait() {
                Ok(Some(_status)) => {
                    // Process has exited
                    return Ok(Some(child.wait_with_output()?));
                }
                Ok(None) => {
                    // Still running, sleep a bit and retry
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("error waiting for cargo test: {}", e))
                        .context("failed to wait for cargo test process");
                }
            }
        }
    }

    /// Get the workspace directory.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Get the timeout in seconds.
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    /// Get the extra arguments.
    pub fn extra_args(&self) -> &[String] {
        &self.extra_args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_captured_output_new() {
        let stdout = b"test stdout output";
        let stderr = b"test stderr output";
        let output = CapturedOutput::new(stdout.to_vec(), stderr.to_vec());

        assert_eq!(output.stdout, "test stdout output");
        assert_eq!(output.stderr, "test stderr output");
        assert!(!output.is_empty());
        assert_eq!(output.total_len(), 36); // 18 + 18
    }

    #[test]
    fn test_captured_output_empty() {
        let output = CapturedOutput::empty();
        assert!(output.is_empty());
        assert_eq!(output.total_len(), 0);
    }

    #[test]
    fn test_result_status_success() {
        let result = TestResult {
            status: TestStatus::Success,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
        };
        assert!(result.is_success());
        assert!(!result.is_failure());
        assert!(!result.is_compilation_failure());
        assert!(!result.is_timed_out());
    }

    #[test]
    fn test_result_status_failed() {
        let result = TestResult {
            status: TestStatus::Failed,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(1),
        };
        assert!(!result.is_success());
        assert!(result.is_failure());
    }

    #[test]
    fn test_result_status_compilation_failed() {
        let result = TestResult {
            status: TestStatus::CompilationFailed,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(101),
        };
        assert!(!result.is_success());
        assert!(result.is_failure());
        assert!(result.is_compilation_failure());
    }

    #[test]
    fn test_result_status_timed_out() {
        let result = TestResult {
            status: TestStatus::TimedOut,
            stdout: String::new(),
            stderr: String::from("command timed out"),
            exit_code: None,
        };
        assert!(!result.is_success());
        assert!(result.is_failure());
        assert!(result.is_timed_out());
    }

    #[test]
    fn test_result_captured_stdout() {
        let result = TestResult {
            status: TestStatus::Success,
            stdout: String::from("test output"),
            stderr: String::new(),
            exit_code: Some(0),
        };
        assert_eq!(result.captured_stdout(), "test output");
        assert_eq!(result.captured_stderr(), "");
    }

    #[test]
    fn test_result_captured_stderr() {
        let result = TestResult {
            status: TestStatus::Failed,
            stdout: String::new(),
            stderr: String::from("error message"),
            exit_code: Some(1),
        };
        assert_eq!(result.captured_stdout(), "");
        assert_eq!(result.captured_stderr(), "error message");
    }

    #[test]
    fn test_result_summary() {
        let success = TestResult {
            status: TestStatus::Success,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
        };
        assert!(success.summary().contains("passed"));

        let failed = TestResult {
            status: TestStatus::Failed,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(1),
        };
        assert!(failed.summary().contains("failed"));

        let timeout = TestResult {
            status: TestStatus::TimedOut,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        };
        assert!(timeout.summary().contains("timed out"));
    }

    #[test]
    fn test_runner_new() {
        let runner = TestRunner::new(Path::new("/tmp"));
        assert_eq!(runner.workspace(), Path::new("/tmp"));
        assert_eq!(runner.timeout_secs(), DEFAULT_TEST_TIMEOUT_SECS);
        assert!(runner.extra_args().is_empty());
    }

    #[test]
    fn test_runner_with_timeout() {
        let runner = TestRunner::new(Path::new("/tmp")).with_timeout(60);
        assert_eq!(runner.timeout_secs(), 60);
    }

    #[test]
    fn test_runner_with_extra_arg() {
        let runner = TestRunner::new(Path::new("/tmp"))
            .with_extra_arg("--release")
            .with_extra_arg("--lib");
        assert_eq!(runner.extra_args().len(), 2);
        assert_eq!(runner.extra_args()[0], "--release");
        assert_eq!(runner.extra_args()[1], "--lib");
    }
}
