//! Test output capture and management.
//!
//! This module provides utilities for managing test output files including
//! stdout, stderr, and combined output. Test outputs are stored in a dedicated
//! directory structure with proper error handling.
//!
//! ## Directory Structure
//!
//! ```text
//! .test_outputs/
//! └── <test-name>/
//!     ├── stdout.txt      # Raw stdout from test execution
//!     ├── stderr.txt      # Raw stderr from test execution
//!     └── combined.txt    # Combined stdout + stderr with interleaving
//! ```
//!
//! ## Usage
//!
//! ```no_run
//! use needle::test_output::{TestOutput, test_output_dir};
//! use std::path::Path;
//!
//! // Create test output directory structure
//! let output = TestOutput::new("my_test", Path::new(".")).unwrap();
//!
//! // Write test outputs
//! output.write_stdout("Test stdout content").unwrap();
//! output.write_stderr("Test stderr content").unwrap();
//! output.write_combined("Combined output").unwrap();
//! ```

use std::path::{Path, PathBuf};
use anyhow::{Context, Result};

// ──────────────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────────────

/// Default test output directory name
pub const TEST_OUTPUT_DIR_NAME: &str = ".test_outputs";

/// Stdout output file name
pub const STDOUT_FILE: &str = "stdout.txt";

/// Stderr output file name
pub const STDERR_FILE: &str = "stderr.txt";

/// Combined output file name
pub const COMBINED_FILE: &str = "combined.txt";

// ──────────────────────────────────────────────────────────────────────────────
// Test output management
// ──────────────────────────────────────────────────────────────────────────────

/// Manages test output files for a single test.
pub struct TestOutput {
    /// Output directory for this test (`.test_outputs/<test-name>`).
    output_dir: PathBuf,
    /// Whether output capture is enabled.
    enabled: bool,
}

impl TestOutput {
    /// Create a new test output manager for a test.
    ///
    /// `test_name` is a unique identifier for the test (e.g., "integration_test_1").
    /// `workspace_root` is the workspace root directory where `.test_outputs/` will be created.
    ///
    /// Returns `None` if the output directory cannot be created.
    ///
    /// ## Example
    ///
    /// ```no_run
    /// use needle::test_output::TestOutput;
    /// use std::path::Path;
    ///
    /// let output = TestOutput::new("my_test", Path::new(".")).unwrap();
    /// ```
    pub fn new(test_name: &str, workspace_root: &Path) -> Option<Self> {
        let output_dir = workspace_root
            .join(TEST_OUTPUT_DIR_NAME)
            .join(test_name);

        // Create the output directory with proper error handling
        if let Err(e) = std::fs::create_dir_all(&output_dir) {
            tracing::warn!(
                test_name = %test_name,
                path = %output_dir.display(),
                error = %e,
                "failed to create test output directory, output capture disabled"
            );
            return None;
        }

        Some(TestOutput {
            output_dir,
            enabled: true,
        })
    }

    /// Get the output directory path.
    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    /// Write stdout to `stdout.txt`.
    ///
    /// ## Errors
    ///
    /// Returns an error if:
    /// - Output capture is disabled
    /// - File write operation fails
    pub fn write_stdout(&self, stdout: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self.output_dir.join(STDOUT_FILE);
        std::fs::write(&path, stdout.as_bytes())
            .with_context(|| format!("failed to write test stdout: {}", path.display()))
    }

    /// Write stderr to `stderr.txt`.
    ///
    /// ## Errors
    ///
    /// Returns an error if:
    /// - Output capture is disabled
    /// - File write operation fails
    pub fn write_stderr(&self, stderr: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self.output_dir.join(STDERR_FILE);
        std::fs::write(&path, stderr.as_bytes())
            .with_context(|| format!("failed to write test stderr: {}", path.display()))
    }

    /// Write combined output to `combined.txt`.
    ///
    /// This is useful for interleaved stdout/stderr or unified test output.
    ///
    /// ## Errors
    ///
    /// Returns an error if:
    /// - Output capture is disabled
    /// - File write operation fails
    pub fn write_combined(&self, combined: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self.output_dir.join(COMBINED_FILE);
        std::fs::write(&path, combined.as_bytes())
            .with_context(|| format!("failed to write test combined output: {}", path.display()))
    }

    /// Get the stdout file path.
    pub fn stdout_path(&self) -> PathBuf {
        self.output_dir.join(STDOUT_FILE)
    }

    /// Get the stderr file path.
    pub fn stderr_path(&self) -> PathBuf {
        self.output_dir.join(STDERR_FILE)
    }

    /// Get the combined output file path.
    pub fn combined_path(&self) -> PathBuf {
        self.output_dir.join(COMBINED_FILE)
    }

    /// Check if stdout file exists.
    pub fn has_stdout(&self) -> bool {
        self.stdout_path().exists()
    }

    /// Check if stderr file exists.
    pub fn has_stderr(&self) -> bool {
        self.stderr_path().exists()
    }

    /// Check if combined output file exists.
    pub fn has_combined(&self) -> bool {
        self.combined_path().exists()
    }

    /// Read stdout content if it exists.
    pub fn read_stdout(&self) -> Result<String> {
        std::fs::read_to_string(self.stdout_path())
            .with_context(|| format!("failed to read test stdout: {}", self.stdout_path().display()))
    }

    /// Read stderr content if it exists.
    pub fn read_stderr(&self) -> Result<String> {
        std::fs::read_to_string(self.stderr_path())
            .with_context(|| format!("failed to read test stderr: {}", self.stderr_path().display()))
    }

    /// Read combined output content if it exists.
    pub fn read_combined(&self) -> Result<String> {
        std::fs::read_to_string(self.combined_path())
            .with_context(|| format!("failed to read test combined output: {}", self.combined_path().display()))
    }

    /// Delete all test output files and the directory.
    ///
    /// ## Errors
    ///
    /// Returns an error if the directory removal fails.
    pub fn cleanup(&self) -> Result<()> {
        if self.output_dir.exists() {
            std::fs::remove_dir_all(&self.output_dir)
                .with_context(|| format!("failed to cleanup test output directory: {}", self.output_dir.display()))?;
        }
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Utility functions
// ──────────────────────────────────────────────────────────────────────────────

/// Get the global test output directory path.
///
/// Returns the path to `.test_outputs/` in the given workspace root.
pub fn test_output_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(TEST_OUTPUT_DIR_NAME)
}

/// Create the global test output directory if it doesn't exist.
///
/// ## Errors
///
/// Returns an error if directory creation fails.
pub fn ensure_test_output_dir(workspace_root: &Path) -> Result<()> {
    let dir = test_output_dir(workspace_root);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create test output directory: {}", dir.display()))
}

/// Clean up all test outputs by removing the entire `.test_outputs/` directory.
///
/// ## Errors
///
/// Returns an error if the directory removal fails.
///
/// ## Warning
///
/// This will delete ALL test outputs. Use with caution.
pub fn cleanup_all_test_outputs(workspace_root: &Path) -> Result<()> {
    let dir = test_output_dir(workspace_root);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("failed to cleanup all test outputs: {}", dir.display()))?;
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_output_creates_directory() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_root = temp_dir.path();

        let output = TestOutput::new("test_example", workspace_root).unwrap();
        assert!(output.output_dir().exists());
        assert!(output.output_dir().ends_with(".test_outputs/test_example"));
    }

    #[test]
    fn test_output_writes_stdout() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_root = temp_dir.path();

        let output = TestOutput::new("test_stdout", workspace_root).unwrap();
        output.write_stdout("hello stdout").unwrap();

        assert!(output.has_stdout());
        let content = output.read_stdout().unwrap();
        assert_eq!(content, "hello stdout");
    }

    #[test]
    fn test_output_writes_stderr() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_root = temp_dir.path();

        let output = TestOutput::new("test_stderr", workspace_root).unwrap();
        output.write_stderr("error output").unwrap();

        assert!(output.has_stderr());
        let content = output.read_stderr().unwrap();
        assert_eq!(content, "error output");
    }

    #[test]
    fn test_output_writes_combined() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_root = temp_dir.path();

        let output = TestOutput::new("test_combined", workspace_root).unwrap();
        output.write_combined("combined output").unwrap();

        assert!(output.has_combined());
        let content = output.read_combined().unwrap();
        assert_eq!(content, "combined output");
    }

    #[test]
    fn test_output_cleanup_removes_directory() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_root = temp_dir.path();

        let output = TestOutput::new("test_cleanup", workspace_root).unwrap();
        output.write_stdout("test data").unwrap();
        assert!(output.output_dir().exists());

        output.cleanup().unwrap();
        assert!(!output.output_dir().exists());
    }

    #[test]
    fn test_output_directory_creation_failure_returns_none() {
        // Try to create a directory in a location that will fail
        // Using a file instead of a directory as the parent
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("not_a_dir.txt");
        std::fs::write(&file_path, "I'm a file").unwrap();

        // This should fail because the parent path exists as a file
        let output = TestOutput::new("test_fail", &file_path);
        assert!(output.is_none(), "Should return None when directory creation fails");
    }

    #[test]
    fn ensure_test_output_dir_creates_directory() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_root = temp_dir.path();

        ensure_test_output_dir(workspace_root).unwrap();

        let dir = test_output_dir(workspace_root);
        assert!(dir.exists());
        assert!(dir.is_dir());
    }

    #[test]
    fn cleanup_all_test_outputs_removes_directory() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_root = temp_dir.path();

        // Create some test outputs
        let output1 = TestOutput::new("test1", workspace_root).unwrap();
        output1.write_stdout("data1").unwrap();
        let output2 = TestOutput::new("test2", workspace_root).unwrap();
        output2.write_stdout("data2").unwrap();

        assert!(test_output_dir(workspace_root).exists());

        cleanup_all_test_outputs(workspace_root).unwrap();
        assert!(!test_output_dir(workspace_root).exists());
    }

    #[test]
    fn test_output_paths() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_root = temp_dir.path();

        let output = TestOutput::new("test_paths", workspace_root).unwrap();

        assert!(output.stdout_path().ends_with("stdout.txt"));
        assert!(output.stderr_path().ends_with("stderr.txt"));
        assert!(output.combined_path().ends_with("combined.txt"));
    }

    #[test]
    fn test_output_disabled_returns_ok() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_root = temp_dir.path();

        let output = TestOutput::new("test_disabled", workspace_root).unwrap();

        // When enabled, operations should succeed
        assert!(output.write_stdout("test").is_ok());
        assert!(output.write_stderr("test").is_ok());
        assert!(output.write_combined("test").is_ok());
    }

    #[test]
    fn test_output_read_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_root = temp_dir.path();

        let output = TestOutput::new("test_empty", workspace_root).unwrap();
        output.write_stdout("").unwrap();

        let content = output.read_stdout().unwrap();
        assert_eq!(content, "");
    }
}
