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

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::process_guard::ProcessGuardSync;
use crate::util::capture_timestamp;

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
// Test Metrics
// ──────────────────────────────────────────────────────────────────────────────

/// Serializable test metrics for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestMetrics {
    /// The test execution status.
    pub status: String,
    /// Exit code from cargo test (None if killed by signal).
    pub exit_code: Option<i32>,
    /// Duration of the test run in milliseconds.
    pub duration_ms: u128,
    /// Timestamp when the test was completed (ISO 8601).
    pub timestamp: String,
}

impl TestMetrics {
    /// Create test metrics from a test result.
    pub fn from_result(result: &TestResult) -> Self {
        Self {
            status: format!("{:?}", result.status),
            exit_code: result.exit_code,
            duration_ms: result.duration.as_millis(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
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
    /// Duration of the test run.
    pub duration: Duration,
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
    /// Create a new test result from process output and duration.
    fn from_output(output: Output, duration: Duration) -> Self {
        let exit_code = output.status.code();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let status = Self::classify_status(&output);

        Self {
            status,
            stdout,
            stderr,
            exit_code,
            duration,
        }
    }

    /// Create a timeout test result with duration.
    fn timeout(duration: Duration) -> Self {
        Self {
            status: TestStatus::TimedOut,
            stdout: String::new(),
            stderr: String::from("command timed out"),
            exit_code: None,
            duration,
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

    /// Persist test output (stdout and stderr) to files.
    ///
    /// ## Arguments
    ///
    /// * `output_dir` - Directory where output files will be written
    /// * `base_name` - Base name for the output files (stdout will be `base_name.stdout`, stderr will be `base_name.stderr`)
    ///
    /// ## Returns
    ///
    /// Returns `Ok(())` if both files were written successfully, or an error if either write failed.
    ///
    /// ## Errors
    ///
    /// Returns an error if:
    /// - The output directory cannot be created
    /// - Either file cannot be written
    pub fn persist_output(&self, output_dir: &Path, base_name: &str) -> Result<()> {
        // Create output directory if it doesn't exist
        fs::create_dir_all(output_dir).with_context(|| {
            format!(
                "failed to create output directory: {}",
                output_dir.display()
            )
        })?;

        // Write stdout
        let stdout_path = output_dir.join(format!("{}.stdout", base_name));
        self.write_file(&stdout_path, &self.stdout)
            .with_context(|| format!("failed to write stdout to {}", stdout_path.display()))?;

        // Write stderr
        let stderr_path = output_dir.join(format!("{}.stderr", base_name));
        self.write_file(&stderr_path, &self.stderr)
            .with_context(|| format!("failed to write stderr to {}", stderr_path.display()))?;

        tracing::debug!(
            stdout_path = %stdout_path.display(),
            stderr_path = %stderr_path.display(),
            stdout_len = self.stdout.len(),
            stderr_len = self.stderr.len(),
            "persisted test output"
        );

        Ok(())
    }

    /// Persist test metrics to a JSON file.
    ///
    /// ## Arguments
    ///
    /// * `output_dir` - Directory where the metrics file will be written
    /// * `base_name` - Base name for the metrics file (will be `base_name.metrics.json`)
    ///
    /// ## Returns
    ///
    /// Returns `Ok(())` if the metrics file was written successfully, or an error if the write failed.
    ///
    /// ## Errors
    ///
    /// Returns an error if:
    /// - The output directory cannot be created
    /// - The file cannot be written
    /// - The metrics cannot be serialized to JSON
    pub fn persist_metrics(&self, output_dir: &Path, base_name: &str) -> Result<()> {
        // Create output directory if it doesn't exist
        fs::create_dir_all(output_dir).with_context(|| {
            format!(
                "failed to create output directory: {}",
                output_dir.display()
            )
        })?;

        // Create metrics from result
        let metrics = TestMetrics::from_result(self);

        // Serialize to JSON
        let json = serde_json::to_string_pretty(&metrics)
            .context("failed to serialize test metrics to JSON")?;

        // Write metrics file
        let metrics_path = output_dir.join(format!("{}.metrics.json", base_name));
        self.write_file(&metrics_path, &json)
            .with_context(|| format!("failed to write metrics to {}", metrics_path.display()))?;

        tracing::debug!(
            metrics_path = %metrics_path.display(),
            status = ?metrics.status,
            duration_ms = metrics.duration_ms,
            "persisted test metrics"
        );

        Ok(())
    }

    /// Persist both test output and metrics.
    ///
    /// This is a convenience method that calls both `persist_output` and `persist_metrics`.
    ///
    /// ## Arguments
    ///
    /// * `output_dir` - Directory where files will be written
    /// * `base_name` - Base name for the output files
    ///
    /// ## Returns
    ///
    /// Returns `Ok(())` if all files were written successfully, or an error if any write failed.
    pub fn persist_all(&self, output_dir: &Path, base_name: &str) -> Result<()> {
        self.persist_output(output_dir, base_name)?;
        self.persist_metrics(output_dir, base_name)?;
        Ok(())
    }

    /// Helper method to write content to a file.
    ///
    /// This method handles the actual file I/O and is used by both `persist_output` and `persist_metrics`.
    fn write_file(&self, path: &Path, content: &str) -> Result<()> {
        let mut file = File::create(path)
            .with_context(|| format!("failed to create file: {}", path.display()))?;

        file.write_all(content.as_bytes())
            .with_context(|| format!("failed to write to file: {}", path.display()))?;

        Ok(())
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

        // Capture the launch timestamp before constructing the command. The
        // helper supplies an epoch fallback if reading the system clock fails,
        // so a timestamp is retained even when command launch fails.
        let launch_timestamp = capture_timestamp();

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
            launch_timestamp = %launch_timestamp,
            "spawning cargo test process"
        );

        let result = self.execute_with_timeout(cmd)?;

        let duration = start.elapsed();

        // Build the test result with captured output
        let test_result = match result {
            Some(output) => TestResult::from_output(output, duration),
            None => TestResult::timeout(duration),
        };

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

        // Spawn the process with guard
        let child = cmd.spawn().context("failed to spawn cargo test process")?;
        let mut guard = ProcessGuardSync::new(child);

        // Wait for completion with timeout
        let start = Instant::now();
        loop {
            // Check if timeout exceeded
            if start.elapsed() > timeout {
                tracing::error!(timeout_secs = self.timeout_secs, "cargo test timed out");

                // Kill the child process - guard will wait on drop
                if let Some(child) = guard.get_mut() {
                    let _ = child.kill();
                }
                let _ = guard.wait();

                return Ok(None);
            }

            // Try to wait with a small timeout
            match guard.get_mut().map(|c| c.try_wait()) {
                Some(Ok(Some(_status))) => {
                    // Process has exited - extract child and get output
                    // We need to take the child out of the guard to call wait_with_output
                    let mut child_option = None;
                    std::mem::swap(&mut child_option, &mut guard.child);
                    if let Some(child) = child_option {
                        return Ok(Some(child.wait_with_output()?));
                    }
                    return Err(anyhow::anyhow!("child already consumed"))
                        .context("failed to wait for cargo test process");
                }
                Some(Ok(None)) => {
                    // Still running, sleep a bit and retry
                    std::thread::sleep(Duration::from_millis(100));
                }
                Some(Err(e)) => {
                    return Err(anyhow::anyhow!("error waiting for cargo test: {}", e))
                        .context("failed to wait for cargo test process");
                }
                None => {
                    return Err(anyhow::anyhow!("child already consumed"))
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
            duration: Duration::from_millis(100),
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
            duration: Duration::from_millis(200),
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
            duration: Duration::from_millis(150),
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
            duration: Duration::from_secs(300),
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
            duration: Duration::from_millis(100),
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
            duration: Duration::from_millis(200),
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
            duration: Duration::from_millis(100),
        };
        assert!(success.summary().contains("passed"));

        let failed = TestResult {
            status: TestStatus::Failed,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: Duration::from_millis(200),
        };
        assert!(failed.summary().contains("failed"));

        let timeout = TestResult {
            status: TestStatus::TimedOut,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            duration: Duration::from_secs(300),
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

    // ──────────────────────────────────────────────────────────────────────────────
    // File Persistence Tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_persist_output_creates_files() {
        // Create a temporary directory for output
        let temp_dir = std::env::temp_dir().join("needle_test_output");
        let _ = fs::remove_dir_all(&temp_dir); // Clean up any previous test runs

        let result = TestResult {
            status: TestStatus::Success,
            stdout: String::from("test stdout content"),
            stderr: String::from("test stderr content"),
            exit_code: Some(0),
            duration: Duration::from_millis(100),
        };

        // Persist output
        let result = result.persist_output(&temp_dir, "test_run");
        assert!(result.is_ok(), "persist_output should succeed");

        // Check that files were created
        let stdout_path = temp_dir.join("test_run.stdout");
        let stderr_path = temp_dir.join("test_run.stderr");

        assert!(stdout_path.exists(), "stdout file should exist");
        assert!(stderr_path.exists(), "stderr file should exist");

        // Verify file contents
        let stdout_content =
            fs::read_to_string(&stdout_path).expect("should be able to read stdout file");
        let stderr_content =
            fs::read_to_string(&stderr_path).expect("should be able to read stderr file");

        assert_eq!(stdout_content, "test stdout content");
        assert_eq!(stderr_content, "test stderr content");

        // Clean up
        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn test_persist_output_creates_directory() {
        let base_dir = std::env::temp_dir().join("needle_test_nested");
        let temp_dir = base_dir.join("output").join("dir");
        let _ = fs::remove_dir_all(&base_dir); // Clean up

        let result = TestResult {
            status: TestStatus::Failed,
            stdout: String::from("error output"),
            stderr: String::from("error details"),
            exit_code: Some(1),
            duration: Duration::from_millis(50),
        };

        // Persist output - should create nested directories
        let result = result.persist_output(&temp_dir, "nested_test");
        assert!(
            result.is_ok(),
            "persist_output should create nested directories"
        );

        // Check that files were created
        assert!(temp_dir.join("nested_test.stdout").exists());
        assert!(temp_dir.join("nested_test.stderr").exists());

        // Clean up
        fs::remove_dir_all(&base_dir).ok();
    }

    #[test]
    fn test_persist_metrics_creates_json() {
        let temp_dir = std::env::temp_dir().join("needle_test_metrics");
        let _ = fs::remove_dir_all(&temp_dir);

        let result = TestResult {
            status: TestStatus::Success,
            stdout: String::from("stdout"),
            stderr: String::from("stderr"),
            exit_code: Some(0),
            duration: Duration::from_millis(250),
        };

        // Persist metrics
        let result = result.persist_metrics(&temp_dir, "test_metrics");
        assert!(result.is_ok(), "persist_metrics should succeed");

        // Check that metrics file was created
        let metrics_path = temp_dir.join("test_metrics.metrics.json");
        assert!(metrics_path.exists(), "metrics file should exist");

        // Verify JSON can be parsed
        let json_content =
            fs::read_to_string(&metrics_path).expect("should be able to read metrics file");
        let metrics: TestMetrics =
            serde_json::from_str(&json_content).expect("should be able to parse metrics JSON");

        assert_eq!(metrics.status, "Success");
        assert_eq!(metrics.exit_code, Some(0));
        assert_eq!(metrics.duration_ms, 250);
        assert!(!metrics.timestamp.is_empty()); // Should have ISO timestamp

        // Clean up
        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn test_persist_all_writes_all_files() {
        let temp_dir = std::env::temp_dir().join("needle_test_persist_all");
        let _ = fs::remove_dir_all(&temp_dir);

        let result = TestResult {
            status: TestStatus::TimedOut,
            stdout: String::from("partial output"),
            stderr: String::from("timeout error"),
            exit_code: None,
            duration: Duration::from_secs(300),
        };

        // Persist all files
        let result = result.persist_all(&temp_dir, "complete_test");
        assert!(result.is_ok(), "persist_all should succeed");

        // Check that all files were created
        assert!(temp_dir.join("complete_test.stdout").exists());
        assert!(temp_dir.join("complete_test.stderr").exists());
        assert!(temp_dir.join("complete_test.metrics.json").exists());

        // Verify metrics content
        let metrics_path = temp_dir.join("complete_test.metrics.json");
        let json_content =
            fs::read_to_string(&metrics_path).expect("should be able to read metrics file");
        let metrics: TestMetrics =
            serde_json::from_str(&json_content).expect("should be able to parse metrics JSON");

        assert_eq!(metrics.status, "TimedOut");
        assert_eq!(metrics.exit_code, None);
        assert_eq!(metrics.duration_ms, 300000);

        // Clean up
        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn test_persist_output_empty_streams() {
        let temp_dir = std::env::temp_dir().join("needle_test_empty");
        let _ = fs::remove_dir_all(&temp_dir);

        let result = TestResult {
            status: TestStatus::Success,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: Duration::from_millis(10),
        };

        // Persist empty output
        let result = result.persist_output(&temp_dir, "empty_test");
        assert!(result.is_ok(), "persist_output should handle empty streams");

        // Check that files were created (even if empty)
        let stdout_path = temp_dir.join("empty_test.stdout");
        let stderr_path = temp_dir.join("empty_test.stderr");

        assert!(stdout_path.exists());
        assert!(stderr_path.exists());

        // Verify files are empty
        let stdout_content =
            fs::read_to_string(&stdout_path).expect("should be able to read stdout file");
        let stderr_content =
            fs::read_to_string(&stderr_path).expect("should be able to read stderr file");

        assert_eq!(stdout_content.len(), 0);
        assert_eq!(stderr_content.len(), 0);

        // Clean up
        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn test_persist_output_large_output() {
        let temp_dir = std::env::temp_dir().join("needle_test_large");
        let _ = fs::remove_dir_all(&temp_dir);

        let large_stdout = "x".repeat(100_000); // 100KB of data
        let large_stderr = "y".repeat(50_000); // 50KB of data

        let result = TestResult {
            status: TestStatus::Failed,
            stdout: large_stdout.clone(),
            stderr: large_stderr.clone(),
            exit_code: Some(101),
            duration: Duration::from_secs(1),
        };

        // Persist large output
        let result = result.persist_output(&temp_dir, "large_test");
        assert!(result.is_ok(), "persist_output should handle large output");

        // Verify file contents
        let stdout_path = temp_dir.join("large_test.stdout");
        let stderr_path = temp_dir.join("large_test.stderr");

        let stdout_content =
            fs::read_to_string(&stdout_path).expect("should be able to read stdout file");
        let stderr_content =
            fs::read_to_string(&stderr_path).expect("should be able to read stderr file");

        assert_eq!(stdout_content, large_stdout);
        assert_eq!(stderr_content, large_stderr);

        // Clean up
        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn test_test_metrics_from_result() {
        let result = TestResult {
            status: TestStatus::CompilationFailed,
            stdout: String::from("compilation error"),
            stderr: String::from("error[E0001]"),
            exit_code: Some(101),
            duration: Duration::from_millis(500),
        };

        let metrics = TestMetrics::from_result(&result);

        assert_eq!(metrics.status, "CompilationFailed");
        assert_eq!(metrics.exit_code, Some(101));
        assert_eq!(metrics.duration_ms, 500);
        assert!(!metrics.timestamp.is_empty());
    }

    #[test]
    fn test_test_metrics_serialization() {
        let metrics = TestMetrics {
            status: String::from("Success"),
            exit_code: Some(0),
            duration_ms: 1000,
            timestamp: String::from("2026-07-14T12:00:00Z"),
        };

        // Test serialization
        let json =
            serde_json::to_string_pretty(&metrics).expect("should be able to serialize metrics");

        // Test deserialization
        let deserialized: TestMetrics =
            serde_json::from_str(&json).expect("should be able to deserialize metrics");

        assert_eq!(deserialized.status, "Success");
        assert_eq!(deserialized.exit_code, Some(0));
        assert_eq!(deserialized.duration_ms, 1000);
        assert_eq!(deserialized.timestamp, "2026-07-14T12:00:00Z");
    }
}
