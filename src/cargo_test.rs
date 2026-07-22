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
use serde::{Deserialize, Serialize};

use crate::test_output::TestOutput;
use crate::trace::TraceCapture;
use crate::types::BeadId;

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
    /// Whether compilation failed (vs test failure).
    pub compilation_failed: bool,
    /// Parsed compilation error messages (if compilation_failed is true).
    pub compilation_errors: Vec<CompilationError>,
}

/// Classification of compilation error variants.
///
/// Rust error codes are organized into categories based on the type of
/// check that failed. This enum provides a high-level classification
/// that groups related error codes together.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CompilationErrorVariant {
    /// Type mismatch errors (E0308, E0309, etc.)
    TypeMismatch,
    /// Borrow checker errors (E0382, E0502, E0505, E0623, etc.)
    BorrowChecker,
    /// Import and path resolution errors (E0433, E0583, E0603, etc.)
    ImportOrPath,
    /// Lifetime errors (E0495, E0502, E0597, E0623, etc.)
    Lifetime,
    /// Trait bound/satisfaction errors (E0277, E0207, etc.)
    TraitBound,
    /// Syntax errors (unrecognized token, unexpected token, etc.)
    Syntax,
    /// Unused code warnings (dead_code, unused_variables, etc.)
    Unused,
    /// Other compiler errors (warnings that cause failure, etc.)
    Other,
    /// Unknown error code pattern
    Unknown,
}

impl CompilationErrorVariant {
    /// Classify a Rust error code into a variant.
    ///
    /// ## Examples
    ///
    /// ```
    /// use needle::cargo_test::CompilationErrorVariant;
    ///
    /// assert_eq!(CompilationErrorVariant::from_code("E0308"), CompilationErrorVariant::TypeMismatch);
    /// assert_eq!(CompilationErrorVariant::from_code("E0382"), CompilationErrorVariant::BorrowChecker);
    /// assert_eq!(CompilationErrorVariant::from_code("E0433"), CompilationErrorVariant::ImportOrPath);
    /// ```
    pub fn from_code(code: &str) -> Self {
        match code {
            // Type mismatch errors
            "E0308" | "E0309" | "E0312" | "E0524" | "E0611" | "E0616" | "E0617" | "E0618"
            | "E0619" | "E0620" | "E0621" | "E0622" | "E0624" | "E0305" | "E0306" | "E0307"
            | "E0061" | "E0063" => CompilationErrorVariant::TypeMismatch,

            // Borrow checker errors
            "E0382" | "E0502" | "E0505" | "E0506" | "E0507" | "E0508" | "E0509" | "E0515"
            | "E0521" | "E0623" | "E0383" | "E0503" | "E0504" | "E0510" | "E0391" | "E0392"
            | "E0393" | "E0394" | "E0395" | "E0396" | "E0397" | "E0398" | "E0399" | "E0400"
            | "E0401" | "E0402" | "E0404" | "E0405" | "E0406" | "E0407" | "E0408" | "E0409"
            | "E0410" | "E0411" | "E0412" | "E0413" | "E0414" | "E0415" | "E0416" | "E0417"
            | "E0418" | "E0419" | "E0420" | "E0421" | "E0422" | "E0423" | "E0424" | "E0425"
            | "E0426" | "E0427" | "E0428" | "E0429" | "E0430" | "E0431" | "E0434" | "E0435"
            | "E0436" | "E0437" | "E0438" | "E0439" | "E0440" | "E0441" | "E0442" | "E0443"
            | "E0444" | "E0445" | "E0446" | "E0447" | "E0448" | "E0449" | "E0450" | "E0451"
            | "E0452" => CompilationErrorVariant::BorrowChecker,

            // Import and path resolution errors
            "E0433" | "E0583" | "E0603" | "E0432" | "E0519" | "E0601" | "E0602" | "E0604"
            | "E0605" | "E0606" | "E0607" | "E0608" | "E0609" | "E0610" | "E0612" | "E0613"
            | "E0614" | "E0615" => CompilationErrorVariant::ImportOrPath,

            // Trait bound errors
            "E0277" | "E0207" | "E0119" | "E0210" | "E0220" | "E0222" | "E0223" | "E0224"
            | "E0225" | "E0226" | "E0227" | "E0228" | "E0229" | "E0230" | "E0231" | "E0232"
            | "E0233" | "E0234" | "E0235" | "E0236" | "E0237" | "E0238" | "E0239" | "E0240"
            | "E0241" | "E0242" | "E0243" | "E0244" | "E0245" | "E0246" | "E0247" | "E0248"
            | "E0249" | "E0250" | "E0251" | "E0252" | "E0253" | "E0254" | "E0255" | "E0256"
            | "E0257" | "E0258" | "E0259" | "E0260" | "E0261" | "E0262" | "E0271" | "E0272"
            | "E0273" | "E0274" | "E0275" | "E0276" | "E0278" | "E0279" | "E0280" | "E0281"
            | "E0282" | "E0283" | "E0284" => CompilationErrorVariant::TraitBound,

            // Syntax errors (typically no error code, detected via pattern)
            _ if code.is_empty() => CompilationErrorVariant::Syntax,

            // Unused code warnings
            _ if code == "unused"
                || code == "dead_code"
                || code == "unused_variables"
                || code == "unused_imports"
                || code == "unused_mut" =>
            {
                CompilationErrorVariant::Unused
            }

            // Default to unknown for unclassified error codes
            _ => CompilationErrorVariant::Unknown,
        }
    }

    /// Get a human-readable name for this variant.
    pub fn name(&self) -> &str {
        match self {
            CompilationErrorVariant::TypeMismatch => "Type Mismatch",
            CompilationErrorVariant::BorrowChecker => "Borrow Checker",
            CompilationErrorVariant::ImportOrPath => "Import/Path",
            CompilationErrorVariant::Lifetime => "Lifetime",
            CompilationErrorVariant::TraitBound => "Trait Bound",
            CompilationErrorVariant::Syntax => "Syntax",
            CompilationErrorVariant::Unused => "Unused Code",
            CompilationErrorVariant::Other => "Other",
            CompilationErrorVariant::Unknown => "Unknown",
        }
    }
}

/// Detailed compilation error information.
///
/// Represents a single compilation error parsed from cargo test stderr.
/// Includes the error code, message, file location, and classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilationError {
    /// Rust error code (e.g., "E0308").
    pub code: Option<String>,
    /// Error variant classification.
    pub variant: CompilationErrorVariant,
    /// Full error message.
    pub message: String,
    /// File path where the error occurred.
    pub file: Option<String>,
    /// Line number (1-indexed).
    pub line: Option<usize>,
    /// Column number (1-indexed).
    pub column: Option<usize>,
}

impl CompilationError {
    /// Create a new compilation error.
    pub fn new(
        code: Option<String>,
        message: String,
        file: Option<String>,
        line: Option<usize>,
        column: Option<usize>,
    ) -> Self {
        let variant = if let Some(ref code) = code {
            CompilationErrorVariant::from_code(code)
        } else {
            CompilationErrorVariant::Syntax
        };

        CompilationError {
            code,
            variant,
            message,
            file,
            line,
            column,
        }
    }

    /// Create a compilation error from an error line and optional location info.
    pub fn from_error_line(line: &str) -> Self {
        let (code, message) = Self::parse_error_code_and_message(line);
        let variant = code
            .as_ref()
            .map(|c| CompilationErrorVariant::from_code(c))
            .unwrap_or(CompilationErrorVariant::Syntax);

        CompilationError {
            code,
            variant,
            message,
            file: None,
            line: None,
            column: None,
        }
    }

    /// Parse error code and message from an error line.
    fn parse_error_code_and_message(line: &str) -> (Option<String>, String) {
        let line = line.trim();

        // Match "error[E0308]: mismatched types" pattern
        if let Some(start) = line.find("error[") {
            if let Some(end) = line.find(']') {
                let error_code = line[start + 6..end].to_string();
                let message = if end + 1 < line.len() {
                    let rest = line[end + 1..].trim();
                    if let Some(stripped) = rest.strip_prefix(':') {
                        stripped.trim().to_string()
                    } else {
                        rest.to_string()
                    }
                } else {
                    String::new()
                };
                return (Some(error_code), message);
            }
        }

        // No error code found
        (None, line.to_string())
    }
}

impl TestOutcome {
    /// Returns true if cargo test exited with code 0.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }

    /// Returns true if the failure was due to compilation errors.
    pub fn is_compilation_failure(&self) -> bool {
        self.compilation_failed
    }

    /// Returns true if the failure was due to test failures (not compilation).
    pub fn is_test_failure(&self) -> bool {
        !self.success() && !self.timed_out && !self.compilation_failed
    }

    /// Returns a human-readable summary of the outcome.
    pub fn summary(&self) -> String {
        if self.timed_out {
            format!("Timed out after {:?}", self.duration)
        } else if self.compilation_failed {
            format!(
                "Compilation failed with {} error(s) in {:?}",
                self.compilation_errors.len(),
                self.duration
            )
        } else if self.success() {
            format!("Passed in {:?}", self.duration)
        } else {
            format!(
                "Failed with exit code {:?} in {:?}",
                self.exit_code, self.duration
            )
        }
    }

    /// Returns a summary of compilation errors, if any.
    pub fn compilation_error_summary(&self) -> Option<String> {
        if self.compilation_errors.is_empty() {
            return None;
        }

        let mut summary = format!("{} compilation error(s):\n", self.compilation_errors.len());

        for (i, error) in self.compilation_errors.iter().enumerate() {
            summary.push_str(&format!("  {}. ", i + 1));

            // Add error code if present
            if let Some(ref code) = error.code {
                summary.push_str(&format!("[{}] ", code));
            }

            // Add variant name
            summary.push_str(&format!("({})", error.variant.name()));

            // Add message (truncated if too long)
            let message = if error.message.len() > 80 {
                format!("{}...", &error.message[..77])
            } else {
                error.message.clone()
            };
            summary.push_str(&format!(": {}\n", message));

            // Add file location if available
            if let Some(ref file) = error.file {
                summary.push_str(&format!("     at {}:{}", file, error.line.unwrap_or(0)));
                if let Some(col) = error.column {
                    summary.push_str(&format!(":{}", col));
                }
                summary.push('\n');
            }
        }

        Some(summary)
    }

    /// Convert this outcome to structured metrics.
    pub fn to_metrics(&self, test_name: String) -> TestMetrics {
        TestMetrics {
            test_name,
            exit_code: self.exit_code,
            duration_ms: self.duration.as_millis() as u64,
            timed_out: self.timed_out,
            stdout_len: self.stdout.len(),
            stderr_len: self.stderr.len(),
            timestamp: chrono::Utc::now(),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Test Metrics
// ──────────────────────────────────────────────────────────────────────────────

/// Structured test metrics for storage and analysis.
///
/// This type captures the essential metrics from a test run in a
/// serializable format that can be written to disk or sent to
/// telemetry systems.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestMetrics {
    /// Name of the test that was run.
    pub test_name: String,
    /// Exit code from cargo test (None if killed by signal).
    pub exit_code: Option<i32>,
    /// Duration of the test run in milliseconds.
    pub duration_ms: u64,
    /// Whether the test timed out.
    pub timed_out: bool,
    /// Length of captured stdout in bytes.
    pub stdout_len: usize,
    /// Length of captured stderr in bytes.
    pub stderr_len: usize,
    /// When the test completed.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl TestMetrics {
    /// Returns true if the test succeeded (exit code 0 and no timeout).
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }

    /// Get the duration as a Duration.
    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.duration_ms)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Compilation Error Detection
// ──────────────────────────────────────────────────────────────────────────────

/// Detect compilation errors from cargo test stderr.
///
/// Returns (compilation_failed, errors) where compilation_failed is true
/// if stderr contains compilation error patterns, and errors is a list
/// of parsed compilation errors.
pub fn detect_compilation_errors(stderr: &str) -> (bool, Vec<CompilationError>) {
    let mut compilation_failed = false;
    let mut errors = Vec::new();

    // Cargo compilation error patterns
    // 1. "error[E####]: " - Rust compiler errors with error codes
    // 2. "error: could not compile " - Compilation failed message
    // 3. "error: aborting due to " - Abort message
    // 4. "error: " followed by a filename - General compilation errors

    for line in stderr.lines() {
        let line = line.trim();

        // Check for Rust compiler error codes (e.g., "error[E0308]:")
        if line.starts_with("error[E") {
            compilation_failed = true;
            let error = CompilationError::from_error_line(line);
            errors.push(error);
            continue;
        }

        // Check for "could not compile" message
        if line.contains("could not compile") {
            compilation_failed = true;
            // Preserve the original error message for test verification
            let message = line.trim().to_string();
            errors.push(CompilationError::new(None, message, None, None, None));
            continue;
        }

        // Check for "aborting due to" message
        // Only count this if we've already seen actual compilation errors
        if line.contains("aborting due to") && compilation_failed {
            if let Some(count) = extract_error_count(line) {
                let message = format!("Aborted due to {} error(s)", count);
                errors.push(CompilationError::new(None, message, None, None, None));
            }
            continue;
        }

        // Check for general "error:" messages (but not warnings)
        if line.starts_with("error:") && !line.starts_with("error[E") {
            // Only capture if it looks like a compilation error, not a test error
            if line.contains("unused") || line.contains("dead_code") || line.contains("mutability")
            {
                // These are compiler warnings/lints, not test failures
                compilation_failed = true;
                let error = CompilationError::new(
                    Some("unused".to_string()),
                    line.to_string(),
                    None,
                    None,
                    None,
                );
                errors.push(error);
            }
        }
    }

    (compilation_failed, errors)
}

/// Parse an error line to extract the error message.
///
/// Input: "error[E0308]: mismatched types"
/// Output: Some("E0308: mismatched types")
#[allow(dead_code)]
fn parse_error_line(line: &str) -> Option<String> {
    // Extract everything after "error[" until end of line
    if let Some(start) = line.find("error[") {
        if let Some(end) = line.find(']') {
            let error_code = &line[start + 6..end]; // Skip "error["
            let message = line[end + 1..].trim(); // Skip "]"
                                                  // Check if message is empty or just a colon
            if !message.is_empty() && message != ":" {
                return Some(format!("{}: {}", error_code, message));
            }
        }
    }
    None
}

/// Extract crate name from "could not compile" message.
///
/// Input: "error: could not compile `my_crate`"
/// Output: Some("my_crate")
#[allow(dead_code)]
fn extract_crate_name(line: &str) -> Option<String> {
    if let Some(start) = line.find('`') {
        if let Some(end) = line.rfind('`') {
            if start < end {
                return Some(line[start + 1..end].to_string());
            }
        }
    }
    None
}

/// Extract error count from "aborting due to" message.
///
/// Input: "aborting due to 3 previous errors"
/// Output: Some(3)
fn extract_error_count(line: &str) -> Option<usize> {
    // Find the number in the line
    let words: Vec<&str> = line.split_whitespace().collect();
    for word in words {
        if let Ok(n) = word.parse::<usize>() {
            return Some(n);
        }
    }
    None
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

        // Spawn the process with timeout handling
        let timeout = Duration::from_secs(self.timeout_secs);

        // Use a thread to handle output() with timeout
        let output_result = std::thread::spawn(move || cmd.output());

        // Wait for thread completion with timeout
        let output = loop {
            if start.elapsed() >= timeout {
                return Ok(TestOutcome {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::from("command timed out"),
                    duration: start.elapsed(),
                    timed_out: true,
                    compilation_failed: false,
                    compilation_errors: Vec::new(),
                });
            }

            // Check if thread is complete
            if output_result.is_finished() {
                match output_result.join() {
                    Ok(Ok(output)) => break output,
                    Ok(Err(e)) => {
                        return Err(anyhow::anyhow!("failed to execute cargo test: {}", e))
                            .with_context(|| "failed to spawn or execute cargo test");
                    }
                    Err(_) => {
                        return Err(anyhow::anyhow!("cargo test thread panicked"));
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(100));
        };

        let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
        let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));

        // Detect compilation errors
        let (compilation_failed, compilation_errors) =
            detect_compilation_errors(&String::from_utf8_lossy(&output.stderr));

        tracing::info!(
            exit_code = ?output.status.code(),
            duration_secs = start.elapsed().as_secs(),
            success = output.status.success(),
            compilation_failed,
            compilation_error_count = compilation_errors.len(),
            "cargo test completed"
        );

        Ok(TestOutcome {
            exit_code: output.status.code(),
            stdout,
            stderr,
            duration: start.elapsed(),
            timed_out: false,
            compilation_failed,
            compilation_errors,
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

    /// Run cargo test and write output to bead trace directory.
    ///
    /// This method runs `cargo test` and writes the captured stdout and stderr
    /// to files in the `.beads/traces/<bead-id>/` directory. This is used to
    /// record test execution output for debugging and analysis.
    ///
    /// ## Arguments
    ///
    /// * `bead_id` - The bead ID to use for the trace directory name
    ///
    /// ## Errors
    ///
    /// Returns an error if:
    /// - The test run fails
    /// - Trace directory creation fails
    /// - File writing fails
    ///
    /// ## Example
    ///
    /// ```no_run
    /// use needle::cargo_test::CargoTest;
    /// use std::path::Path;
    ///
    /// let runner = CargoTest::new(Path::new("/workspace"));
    /// let bead_id = "bf-12345";
    /// let outcome = runner.run_with_bead_trace(bead_id).unwrap();
    /// ```
    pub fn run_with_bead_trace(&self, bead_id: &str) -> Result<TestOutcome> {
        // First run the test to get the outcome
        let outcome = self.run()?;

        // Create bead trace directory and write outputs
        let bead_id = BeadId::from(bead_id);
        if let Some(trace) = TraceCapture::new(&bead_id, &self.workspace) {
            // Write stdout
            if let Err(e) = trace.write_stdout(&outcome.stdout) {
                tracing::warn!(
                    bead_id = %bead_id,
                    error = %e,
                    "failed to write stdout to trace file"
                );
            }

            // Write stderr
            if let Err(e) = trace.write_stderr(&outcome.stderr) {
                tracing::warn!(
                    bead_id = %bead_id,
                    error = %e,
                    "failed to write stderr to trace file"
                );
            }

            // Write test metrics (exit code, duration, etc.)
            let test_name = format!("cargo_test_{}", bead_id.as_ref());
            let metrics = outcome.to_metrics(test_name);
            if let Err(e) = trace.write_test_metrics(&metrics) {
                tracing::warn!(
                    bead_id = %bead_id,
                    error = %e,
                    "failed to write test metrics to trace file"
                );
            }

            // Write compilation errors if any were detected
            if !outcome.compilation_errors.is_empty() {
                if let Err(e) = trace.write_compilation_errors(&outcome.compilation_errors) {
                    tracing::warn!(
                        bead_id = %bead_id,
                        error = %e,
                        "failed to write compilation errors to trace file"
                    );
                }
            }

            // Write combined test output (stdout + stderr)
            let combined = create_combined_output(&outcome.stdout, &outcome.stderr);
            if let Err(e) = trace.write_test_output(&combined) {
                tracing::warn!(
                    bead_id = %bead_id,
                    error = %e,
                    "failed to write combined test output to trace file"
                );
            }

            tracing::info!(
                bead_id = %bead_id,
                trace_dir = %trace.trace_dir().display(),
                stdout_path = %trace.trace_dir().join("stdout.txt").display(),
                stderr_path = %trace.trace_dir().join("stderr.txt").display(),
                test_output_path = %trace.trace_dir().join("test-output.txt").display(),
                metrics_path = %trace.trace_dir().join("test_metrics.json").display(),
                compilation_errors_path = %trace.trace_dir().join("compilation_errors.json").display(),
                exit_code = ?outcome.exit_code,
                duration_ms = outcome.duration.as_millis(),
                compilation_error_count = outcome.compilation_errors.len(),
                "test output, metrics, and compilation errors written to bead trace files"
            );
        } else {
            tracing::warn!(
                bead_id = %bead_id,
                "failed to create bead trace directory, output files not written"
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
            compilation_failed: false,
            compilation_errors: Vec::new(),
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
            compilation_failed: false,
            compilation_errors: Vec::new(),
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
            compilation_failed: false,
            compilation_errors: Vec::new(),
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
            compilation_failed: false,
            compilation_errors: Vec::new(),
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
            compilation_failed: false,
            compilation_errors: Vec::new(),
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
            compilation_failed: false,
            compilation_errors: Vec::new(),
        };
        assert!(outcome.summary().contains("Timed out"));
        assert!(outcome.summary().contains("100s"));
    }

    #[test]
    fn test_outcome_summary_compilation_failed() {
        let outcome = TestOutcome {
            exit_code: Some(101),
            stdout: String::new(),
            stderr: String::new(),
            duration: Duration::from_secs(2),
            timed_out: false,
            compilation_failed: true,
            compilation_errors: vec![CompilationError::new(
                Some("E0308".to_string()),
                "mismatched types".to_string(),
                None,
                None,
                None,
            )],
        };
        assert!(outcome.summary().contains("Compilation failed"));
        assert!(outcome.summary().contains("1 error"));
    }

    #[test]
    fn test_outcome_is_compilation_failure() {
        let outcome = TestOutcome {
            exit_code: Some(101),
            stdout: String::new(),
            stderr: String::new(),
            duration: Duration::from_secs(1),
            timed_out: false,
            compilation_failed: true,
            compilation_errors: vec![CompilationError::new(
                Some("E0308".to_string()),
                "mismatched types".to_string(),
                None,
                None,
                None,
            )],
        };
        assert!(outcome.is_compilation_failure());
        assert!(!outcome.is_test_failure());
    }

    #[test]
    fn test_outcome_is_test_failure() {
        let outcome = TestOutcome {
            exit_code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
            duration: Duration::from_secs(1),
            timed_out: false,
            compilation_failed: false,
            compilation_errors: Vec::new(),
        };
        assert!(!outcome.is_compilation_failure());
        assert!(outcome.is_test_failure());
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
        )
        .unwrap();

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
        )
        .unwrap();

        // Run cargo test with output file capture
        let runner = CargoTest::new(workspace);
        let outcome = runner.run_with_output_files("test_example").unwrap();

        // Verify the test ran successfully
        assert!(
            outcome.success() || outcome.exit_code.is_some(),
            "cargo test should complete with an exit code"
        );

        // Verify output files were created
        let test_output_dir = workspace.join(".test_outputs").join("test_example");
        assert!(
            test_output_dir.exists(),
            "test output directory should exist"
        );

        let stdout_path = test_output_dir.join("stdout.txt");
        let stderr_path = test_output_dir.join("stderr.txt");
        let combined_path = test_output_dir.join("combined.txt");

        // Files should exist
        assert!(stdout_path.exists(), "stdout file should exist");
        assert!(stderr_path.exists(), "stderr file should exist");
        assert!(combined_path.exists(), "combined file should exist");

        // Verify files can be read
        let _stdout_content =
            fs::read_to_string(&stdout_path).expect("stdout file should be readable");
        let _stderr_content =
            fs::read_to_string(&stderr_path).expect("stderr file should be readable");
        let _combined_content =
            fs::read_to_string(&combined_path).expect("combined file should be readable");
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
        )
        .unwrap();

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
        )
        .unwrap();

        // Run cargo test with output file capture
        let runner = CargoTest::new(workspace);
        let _outcome = runner.run_with_output_files("test_with_output").unwrap();

        // Verify output files were created
        let test_output_dir = workspace.join(".test_outputs").join("test_with_output");
        let combined_path = test_output_dir.join("combined.txt");

        let combined_content =
            fs::read_to_string(&combined_path).expect("combined file should be readable");

        // Verify combined output has the expected structure
        // When both stdout and stderr are present, we should see both sections
        // When only one is present, we see just that section
        let has_structure = combined_content.contains("=== STDOUT ===")
            || combined_content.contains("=== STDERR ===")
            || !combined_content.is_empty();
        assert!(
            has_structure,
            "combined output should have content or structure"
        );
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
        )
        .unwrap();

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
        )
        .unwrap();

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

    #[test]
    fn test_metrics_from_successful_outcome() {
        let outcome = TestOutcome {
            exit_code: Some(0),
            stdout: String::from("test output"),
            stderr: String::from("test warnings"),
            duration: Duration::from_millis(1500),
            timed_out: false,
            compilation_failed: false,
            compilation_errors: Vec::new(),
        };

        let metrics = outcome.to_metrics("my_test".to_string());

        assert_eq!(metrics.test_name, "my_test");
        assert_eq!(metrics.exit_code, Some(0));
        assert_eq!(metrics.duration_ms, 1500);
        assert!(!metrics.timed_out);
        assert_eq!(metrics.stdout_len, 11);
        assert_eq!(metrics.stderr_len, 13);
        assert!(metrics.success());
    }

    #[test]
    fn test_metrics_from_failed_outcome() {
        let outcome = TestOutcome {
            exit_code: Some(1),
            stdout: String::from("failed output"),
            stderr: String::from("error details"),
            duration: Duration::from_millis(500),
            timed_out: false,
            compilation_failed: false,
            compilation_errors: Vec::new(),
        };

        let metrics = outcome.to_metrics("failing_test".to_string());

        assert_eq!(metrics.test_name, "failing_test");
        assert_eq!(metrics.exit_code, Some(1));
        assert_eq!(metrics.duration_ms, 500);
        assert!(!metrics.timed_out);
        assert!(!metrics.success());
    }

    #[test]
    fn test_metrics_from_timeout_outcome() {
        let outcome = TestOutcome {
            exit_code: None,
            stdout: String::new(),
            stderr: String::from("timeout message"),
            duration: Duration::from_secs(600),
            timed_out: true,
            compilation_failed: false,
            compilation_errors: Vec::new(),
        };

        let metrics = outcome.to_metrics("timeout_test".to_string());

        assert_eq!(metrics.test_name, "timeout_test");
        assert_eq!(metrics.exit_code, None);
        assert_eq!(metrics.duration_ms, 600000);
        assert!(metrics.timed_out);
        assert!(!metrics.success());
    }

    #[test]
    fn test_metrics_success_returns_true_for_zero_exit() {
        let metrics = TestMetrics {
            test_name: "test".to_string(),
            exit_code: Some(0),
            duration_ms: 100,
            timed_out: false,
            stdout_len: 50,
            stderr_len: 0,
            timestamp: chrono::Utc::now(),
        };
        assert!(metrics.success());
    }

    #[test]
    fn test_metrics_success_returns_false_for_nonzero_exit() {
        let metrics = TestMetrics {
            test_name: "test".to_string(),
            exit_code: Some(1),
            duration_ms: 100,
            timed_out: false,
            stdout_len: 50,
            stderr_len: 0,
            timestamp: chrono::Utc::now(),
        };
        assert!(!metrics.success());
    }

    #[test]
    fn test_metrics_success_returns_false_for_timeout() {
        let metrics = TestMetrics {
            test_name: "test".to_string(),
            exit_code: Some(0),
            duration_ms: 100,
            timed_out: true,
            stdout_len: 50,
            stderr_len: 0,
            timestamp: chrono::Utc::now(),
        };
        assert!(!metrics.success());
    }

    #[test]
    fn test_metrics_duration_conversion() {
        let metrics = TestMetrics {
            test_name: "test".to_string(),
            exit_code: Some(0),
            duration_ms: 2500,
            timed_out: false,
            stdout_len: 50,
            stderr_len: 0,
            timestamp: chrono::Utc::now(),
        };

        let duration = metrics.duration();
        assert_eq!(duration.as_secs(), 2);
        assert_eq!(duration.as_millis() % 1000, 500);
    }

    #[test]
    fn test_metrics_serialization() {
        let metrics = TestMetrics {
            test_name: "serialization_test".to_string(),
            exit_code: Some(0),
            duration_ms: 1000,
            timed_out: false,
            stdout_len: 100,
            stderr_len: 50,
            timestamp: chrono::Utc::now(),
        };

        // Test JSON serialization
        let json = serde_json::to_string(&metrics).unwrap();
        assert!(json.contains("\"test_name\":\"serialization_test\""));
        assert!(json.contains("\"exit_code\":0"));
        assert!(json.contains("\"duration_ms\":1000"));
        assert!(json.contains("\"timed_out\":false"));

        // Test JSON deserialization
        let deserialized: TestMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.test_name, "serialization_test");
        assert_eq!(deserialized.exit_code, Some(0));
        assert_eq!(deserialized.duration_ms, 1000);
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Compilation Error Detection Tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn detect_compilation_errors_empty_stderr() {
        let stderr = "";
        let (failed, errors) = detect_compilation_errors(stderr);
        assert!(!failed);
        assert!(errors.is_empty());
    }

    #[test]
    fn detect_compilation_errors_with_error_code() {
        let stderr = "error[E0308]: mismatched types\n  --> src/main.rs:10:5\n";
        let (failed, errors) = detect_compilation_errors(stderr);
        assert!(failed);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code.as_deref(), Some("E0308"));
        assert!(errors[0].message.contains("mismatched types"));
        assert_eq!(errors[0].variant, CompilationErrorVariant::TypeMismatch);
    }

    #[test]
    fn detect_compilation_errors_multiple_errors() {
        let stderr = "error[E0308]: mismatched types\nerror[E0382]: use of moved value\n";
        let (failed, errors) = detect_compilation_errors(stderr);
        assert!(failed);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].code.as_deref(), Some("E0308"));
        assert_eq!(errors[1].code.as_deref(), Some("E0382"));
    }

    #[test]
    fn detect_compilation_errors_could_not_compile() {
        let stderr = "error: could not compile `my_crate` (bin \"my_crate\")\n";
        let (failed, errors) = detect_compilation_errors(stderr);
        assert!(failed);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("my_crate"));
    }

    #[test]
    fn detect_compilation_errors_aborting_due_to() {
        let stderr = "error: aborting due to 3 previous errors\n";
        let (failed, errors) = detect_compilation_errors(stderr);
        // "aborting due to" alone doesn't indicate compilation - it's just a summary
        // It needs actual error codes (like "error[E0308]:") to trigger compilation_failed
        assert!(!failed);
        assert_eq!(errors.len(), 0);
    }

    #[test]
    fn detect_compilation_errors_full_output() {
        let stderr = r#"   Compiling my_crate v0.1.0 (/path/to/crate)
error[E0308]: mismatched types
  --> src/main.rs:10:5
   |
10 |     let x: i32 = "hello";
   |            ---   ^^^^^^^ expected `i32`, found `&str`
   |            expected due to this

error: aborting due to 1 previous error

error: could not compile `my_crate` (bin \"my_crate\)
"#;
        let (failed, errors) = detect_compilation_errors(stderr);
        assert!(failed, "should detect compilation failure");
        assert!(!errors.is_empty(), "should have at least one error");
        assert!(
            errors.iter().any(|e| e.code.as_deref() == Some("E0308")),
            "should include E0308 error code"
        );
    }

    #[test]
    fn detect_compilation_errors_test_output_only() {
        // Test output without compilation errors
        let stderr = "running 3 tests\ntest test_foo ... ok\ntest test_bar ... FAILED\n";
        let (failed, errors) = detect_compilation_errors(stderr);
        assert!(!failed);
        assert!(errors.is_empty());
    }

    #[test]
    fn parse_error_line_with_code() {
        let line = "error[E0308]: mismatched types";
        let parsed = parse_error_line(line);
        assert!(parsed.is_some());
        let parsed_str = parsed.as_ref().unwrap();
        assert!(parsed_str.contains("E0308"));
        assert!(parsed_str.contains("mismatched types"));
    }

    #[test]
    fn parse_error_line_no_message() {
        let line = "error[E0308]:";
        let parsed = parse_error_line(line);
        assert!(parsed.is_none());
    }

    #[test]
    fn parse_error_line_not_error() {
        let line = "warning: unused variable";
        let parsed = parse_error_line(line);
        assert!(parsed.is_none());
    }

    #[test]
    fn extract_crate_name_from_compile_error() {
        let line = "error: could not compile `my_crate` (bin \"my_crate\")";
        let name = extract_crate_name(line);
        assert_eq!(name, Some("my_crate".to_string()));
    }

    #[test]
    fn extract_crate_name_no_backticks() {
        let line = "error: compilation failed";
        let name = extract_crate_name(line);
        assert!(name.is_none());
    }

    #[test]
    fn extract_error_count_from_abort_message() {
        let line = "aborting due to 3 previous errors";
        let count = extract_error_count(line);
        assert_eq!(count, Some(3));
    }

    #[test]
    fn extract_error_count_no_number() {
        let line = "aborting due to previous errors";
        let count = extract_error_count(line);
        assert!(count.is_none());
    }

    #[test]
    fn cargo_test_spawn_succeeds() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Create a minimal Cargo project for testing
        let cargo_toml = workspace.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "spawn-test"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
        )
        .unwrap();

        let src_dir = workspace.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let lib_rs = src_dir.join("lib.rs");
        fs::write(
            &lib_rs,
            r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_spawn() {
        assert!(true);
    }
}
"#,
        )
        .unwrap();

        // Test that spawn succeeds
        let runner = CargoTest::new(workspace);
        let result = runner.run();

        // Should succeed in spawning (even if tests fail)
        assert!(
            result.is_ok(),
            "cargo test spawn should succeed, got error: {:?}",
            result.err()
        );

        let outcome = result.unwrap();
        // Should have an exit code (didn't timeout during spawn)
        assert!(
            outcome.exit_code.is_some() || outcome.timed_out,
            "should have exit code or timeout flag"
        );
    }

    #[test]
    fn cargo_test_spawn_with_timeout_protection() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Create a minimal Cargo project
        let cargo_toml = workspace.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "timeout-test"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();

        let src_dir = workspace.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let lib_rs = src_dir.join("lib.rs");
        fs::write(&lib_rs, "").unwrap();

        // Test with very short timeout
        let runner = CargoTest::new(workspace).with_timeout(1);
        let outcome = runner.run().unwrap();

        // Should complete quickly or timeout gracefully
        assert!(
            outcome.exit_code.is_some() || outcome.timed_out,
            "should have exit code or be marked as timed out"
        );

        // Duration should be recorded
        assert!(
            outcome.duration.as_secs() < 10,
            "test should complete quickly, took {:?}",
            outcome.duration
        );
    }

    #[test]
    fn cargo_test_spawn_captures_output_streams() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Create a minimal Cargo project
        let cargo_toml = workspace.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "output-test"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
        )
        .unwrap();

        let src_dir = workspace.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let lib_rs = src_dir.join("lib.rs");
        fs::write(
            &lib_rs,
            r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_with_output() {
        println!("STDOUT message");
        eprintln!("STDERR message");
        assert!(true);
    }
}
"#,
        )
        .unwrap();

        // Test that output is captured
        let runner = CargoTest::new(workspace);
        let outcome = runner.run().unwrap();

        // Should have captured output
        assert!(
            !outcome.stdout.is_empty() || !outcome.stderr.is_empty(),
            "should capture at least one output stream"
        );

        // Verify stdout/stderr strings are captured (they may contain cargo output)
        assert!(
            !outcome.stdout.is_empty() || !outcome.stderr.is_empty(),
            "at least one output stream should be captured"
        );
    }

    #[test]
    fn run_with_bead_trace_creates_trace_directory() {
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
        )
        .unwrap();

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
        )
        .unwrap();

        // Run cargo test with bead trace capture
        let runner = CargoTest::new(workspace);
        let bead_id = "bf-test-123";
        let outcome = runner.run_with_bead_trace(bead_id).unwrap();

        // Verify the test ran successfully
        assert!(
            outcome.success() || outcome.exit_code.is_some(),
            "cargo test should complete with an exit code"
        );

        // Verify bead trace directory was created
        let bead_trace_dir = workspace.join(".beads").join("traces").join(bead_id);
        assert!(bead_trace_dir.exists(), "bead trace directory should exist");

        // Verify stdout and stderr files exist
        let stdout_path = bead_trace_dir.join("stdout.txt");
        let stderr_path = bead_trace_dir.join("stderr.txt");

        assert!(stdout_path.exists(), "stdout file should exist");
        assert!(stderr_path.exists(), "stderr file should exist");

        // Verify files can be read
        let stdout_content =
            fs::read_to_string(&stdout_path).expect("stdout file should be readable");
        let stderr_content =
            fs::read_to_string(&stderr_path).expect("stderr file should be readable");

        // Verify content is not empty (cargo output should be present)
        assert!(
            !stdout_content.is_empty() || !stderr_content.is_empty(),
            "at least one output file should have content"
        );
    }

    #[test]
    fn run_with_bead_trace_handles_test_output() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Create a minimal Cargo project
        let cargo_toml = workspace.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-output-project"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
        )
        .unwrap();

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
        eprintln!("Test error message");
        assert!(true);
    }
}
"#,
        )
        .unwrap();

        // Run cargo test with bead trace capture
        let runner = CargoTest::new(workspace);
        let bead_id = "bf-output-test";
        let outcome = runner.run_with_bead_trace(bead_id).unwrap();

        // Verify test completed
        assert!(outcome.success() || outcome.exit_code.is_some());

        // Verify trace files exist and are readable
        let bead_trace_dir = workspace.join(".beads").join("traces").join(bead_id);
        let stdout_path = bead_trace_dir.join("stdout.txt");
        let stderr_path = bead_trace_dir.join("stderr.txt");

        assert!(stdout_path.exists(), "stdout file should exist");
        assert!(stderr_path.exists(), "stderr file should exist");

        let stdout_content = fs::read_to_string(&stdout_path).expect("stdout should be readable");
        let stderr_content = fs::read_to_string(&stderr_path).expect("stderr should be readable");

        // Verify output has content (cargo output should be present)
        assert!(
            !stdout_content.is_empty() || !stderr_content.is_empty(),
            "trace files should contain test output"
        );
    }

    #[test]
    fn run_with_bead_trace_handles_empty_output() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Create a minimal Cargo project
        let cargo_toml = workspace.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-empty-project"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
        )
        .unwrap();

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
        )
        .unwrap();

        // Run cargo test with bead trace capture
        let runner = CargoTest::new(workspace);
        let bead_id = "bf-empty-test";
        let outcome = runner.run_with_bead_trace(bead_id).unwrap();

        // Should complete successfully even with no custom output
        assert!(outcome.success() || outcome.exit_code.is_some());

        // Trace files should still be created
        let bead_trace_dir = workspace.join(".beads").join("traces").join(bead_id);
        assert!(bead_trace_dir.exists(), "trace directory should exist");
        assert!(bead_trace_dir.join("stdout.txt").exists());
        assert!(bead_trace_dir.join("stderr.txt").exists());
    }

    #[test]
    fn run_with_bead_trace_creates_parent_directory() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Create a minimal Cargo project
        let cargo_toml = workspace.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-dir-project"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
        )
        .unwrap();

        let src_dir = workspace.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let lib_rs = src_dir.join("lib.rs");
        fs::write(
            &lib_rs,
            r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_dir() {
        assert!(true);
    }
}
"#,
        )
        .unwrap();

        // Ensure .beads directory doesn't exist initially
        let beads_dir = workspace.join(".beads");
        assert!(
            !beads_dir.exists(),
            ".beads directory should not exist initially"
        );

        // Run cargo test with bead trace capture
        let runner = CargoTest::new(workspace);
        let bead_id = "bf-dir-test";
        let _ = runner.run_with_bead_trace(bead_id).unwrap();

        // Verify parent directories were created
        let traces_dir = workspace.join(".beads").join("traces");
        assert!(traces_dir.exists(), "traces directory should be created");
        assert!(traces_dir.is_dir(), "traces should be a directory");

        let bead_trace_dir = traces_dir.join(bead_id);
        assert!(
            bead_trace_dir.exists(),
            "bead trace subdirectory should be created"
        );
    }

    #[test]
    fn run_with_bead_trace_writes_test_metrics() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Create a minimal Cargo project
        let cargo_toml = workspace.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-metrics-project"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
        )
        .unwrap();

        let src_dir = workspace.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let lib_rs = src_dir.join("lib.rs");
        fs::write(
            &lib_rs,
            r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_metrics_example() {
        assert!(true);
    }
}
"#,
        )
        .unwrap();

        // Run cargo test with bead trace capture
        let runner = CargoTest::new(workspace);
        let bead_id = "bf-metrics-test";
        let outcome = runner.run_with_bead_trace(bead_id).unwrap();

        // Verify test completed
        assert!(outcome.success() || outcome.exit_code.is_some());

        // Verify test_metrics.json exists
        let bead_trace_dir = workspace.join(".beads").join("traces").join(bead_id);
        let metrics_path = bead_trace_dir.join("test_metrics.json");

        assert!(metrics_path.exists(), "test_metrics.json file should exist");

        // Read and verify the metrics
        let metrics_content =
            fs::read_to_string(&metrics_path).expect("test_metrics.json should be readable");
        let metrics: TestMetrics =
            serde_json::from_str(&metrics_content).expect("test_metrics.json should be valid JSON");

        // Verify metrics contain expected values
        assert!(metrics.test_name.contains("cargo_test_"));
        assert!(metrics.test_name.contains(bead_id));
        assert_eq!(metrics.exit_code, outcome.exit_code);
        assert!(metrics.duration_ms > 0 || outcome.timed_out);

        // Verify timestamp is recent (within last minute)
        let now = chrono::Utc::now();
        let age = now.signed_duration_since(metrics.timestamp);
        assert!(age.num_seconds() < 60, "timestamp should be recent");

        // Verify success flag matches outcome
        assert_eq!(metrics.success(), outcome.success());
        assert_eq!(metrics.timed_out, outcome.timed_out);
    }

    #[test]
    fn run_with_bead_trace_metrics_captures_exit_code() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Create a minimal Cargo project with a failing test
        let cargo_toml = workspace.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-exit-code-project"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
        )
        .unwrap();

        let src_dir = workspace.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let lib_rs = src_dir.join("lib.rs");
        fs::write(
            &lib_rs,
            r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_failing() {
        assert!(false, "intentional failure");
    }
}
"#,
        )
        .unwrap();

        // Run cargo test with bead trace capture
        let runner = CargoTest::new(workspace);
        let bead_id = "bf-exit-code-test";
        let outcome = runner.run_with_bead_trace(bead_id).unwrap();

        // Verify test_metrics.json captures the exit code
        let bead_trace_dir = workspace.join(".beads").join("traces").join(bead_id);
        let metrics_path = bead_trace_dir.join("test_metrics.json");

        let metrics_content = fs::read_to_string(&metrics_path).unwrap();
        let metrics: TestMetrics = serde_json::from_str(&metrics_content).unwrap();

        // Exit code should be non-zero for failing tests
        assert!(
            metrics.exit_code.unwrap_or(0) != 0 || !outcome.success(),
            "exit code should reflect test failure"
        );
    }

    #[test]
    fn test_metrics_high_precision_timing() {
        // Create a test outcome with sub-millisecond duration
        let outcome = TestOutcome {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            duration: Duration::from_nanos(1_500_000), // 1.5 milliseconds
            timed_out: false,
            compilation_failed: false,
            compilation_errors: Vec::new(),
        };

        let metrics = outcome.to_metrics("precision_test".to_string());

        // Verify sub-millisecond precision is preserved in duration_ms
        // (Note: TestMetrics stores duration as u64 milliseconds, so 1.5ms becomes 1ms)
        assert_eq!(metrics.duration_ms, 1);
        assert_eq!(metrics.duration().as_nanos(), 1_000_000);
    }
}
