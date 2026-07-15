//! Basic test runner module for executing cargo test commands.
//!
//! This module provides a simple interface for running cargo test
//! with proper process spawning and result handling.
//!
//! ## Usage
//!
//! ```no_run
//! use needle::test_runner::{TestRunner, TestResult};
//! use std::path::Path;
//!
//! let runner = TestRunner::new(Path::new("/workspace"));
//! match runner.run_tests(&[]) {
//!     Ok(TestResult::Success) => println!("Tests passed!"),
//!     Ok(TestResult::Failed) => println!("Tests failed"),
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
// Test Result
// ──────────────────────────────────────────────────────────────────────────────

/// Result of running cargo test.
#[derive(Debug, Clone)]
pub enum TestResult {
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
    /// Returns true if the test result indicates success.
    pub fn is_success(&self) -> bool {
        matches!(self, TestResult::Success)
    }

    /// Returns true if the test result indicates failure.
    pub fn is_failure(&self) -> bool {
        !self.is_success()
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
    /// ## Errors
    ///
    /// Returns an error if:
    /// - The cargo binary cannot be found
    /// - The process fails to spawn
    /// - The test execution times out
    pub fn run_tests(&self, args: &[&str]) -> Result<TestResult> {
        let start = Instant::now();

        // Build the command
        let mut cmd = Command::new("cargo");
        cmd.arg("test");
        cmd.current_dir(&self.workspace);

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

        // Determine the test result
        let test_result = match result {
            Some(output) => self.classify_result(&output),
            None => TestResult::TimedOut,
        };

        let duration = start.elapsed();

        tracing::info!(
            result = ?test_result,
            duration_secs = duration.as_secs(),
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

    /// Classify the test result based on process output.
    fn classify_result(&self, output: &Output) -> TestResult {
        let exit_code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Check for compilation errors
        if stderr.contains("error[E") || stderr.contains("error: aborting") {
            return TestResult::CompilationFailed;
        }

        // Check exit code
        match exit_code {
            Some(0) => TestResult::Success,
            Some(101) => {
                // Exit code 101 indicates test failures
                if stderr.contains("test result:") {
                    TestResult::Failed
                } else {
                    TestResult::CompilationFailed
                }
            }
            Some(code) => {
                tracing::warn!(exit_code = code, "unexpected exit code from cargo test");
                TestResult::Failed
            }
            None => {
                tracing::warn!("no exit code available from cargo test");
                TestResult::Failed
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
    fn test_result_is_success() {
        assert!(TestResult::Success.is_success());
        assert!(!TestResult::Failed.is_success());
        assert!(!TestResult::CompilationFailed.is_success());
        assert!(!TestResult::TimedOut.is_success());
    }

    #[test]
    fn test_result_is_failure() {
        assert!(!TestResult::Success.is_failure());
        assert!(TestResult::Failed.is_failure());
        assert!(TestResult::CompilationFailed.is_failure());
        assert!(TestResult::TimedOut.is_failure());
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
