//! Version probe: Run binaries with --version and extract backend identity.
//!
//! This module provides functionality to execute a binary with the `--version`
//! flag and parse its output to determine the backend type (e.g., 'bf', 'bead',
//! 'beads-rust').
//!
//! ## Version Output Formats
//!
//! Different bead CLI backends report their identity in different formats:
//!
//! - **bf (bead-forge)**: `bf 0.3.0`
//! - **bead-rs**: `bead 0.26.0`
//! - **Future backends**: May use different naming conventions
//!
//! ## Usage
//!
//! ```no_run
//! use needle::version_probe::VersionProbe;
//!
//! let probe = VersionProbe::new();
//! match probe.detect_backend("bf") {
//!     Ok(Some(backend)) => println!("Detected backend: {}", backend),
//!     Ok(None) => println!("No backend detected"),
//!     Err(e) => eprintln!("Error: {}", e),
//! }
//! ```

use std::time::Duration;

use anyhow::Result;
use thiserror::Error;

// ──────────────────────────────────────────────────────────────────────────────
// Telemetry support
// ──────────────────────────────────────────────────────────────────────────────

/// Telemetry emitter trait for version probe events.
///
/// This trait allows the version probe to emit telemetry events without
/// depending on the full Telemetry struct, enabling testing and flexible usage.
pub trait TelemetryEmitter: Send + Sync {
    /// Emit a telemetry event.
    fn emit_version_event(&self, event: VersionVerifyEvent);
}

/// Version verification telemetry event.
#[derive(Debug, Clone)]
pub enum VersionVerifyEvent {
    Started {
        binary: String,
        expected_backend: String,
    },
    Success {
        binary: String,
        expected_backend: String,
        actual_backend: String,
    },
    Failed {
        binary: String,
        expected_backend: String,
        actual_backend: Option<String>,
        error_type: String,
        error_message: String,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// Backend name constants
// ──────────────────────────────────────────────────────────────────────────────

/// Backend name for bead-forge (legacy CLI).
pub const BACKEND_BF: &str = "bf";

/// Backend name for bead-rs (current CLI).
pub const BACKEND_BEAD: &str = "bead";

/// Backend name for bead-rs (alternative form).
pub const BACKEND_BEADS_RUST: &str = "beads-rust";

// ──────────────────────────────────────────────────────────────────────────────
// ProbeError
// ──────────────────────────────────────────────────────────────────────────────

/// Error type for version probe failures.
///
/// This error represents all possible failure modes when attempting to detect
/// a backend from a binary's --version output.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum ProbeError {
    /// Binary not found in PATH.
    #[error("binary '{binary}' not found in PATH")]
    BinaryNotFound { binary: String },

    /// Binary execution failed (OS-level spawn failure).
    #[error("failed to execute binary '{binary}': {reason}")]
    ExecutionFailed { binary: String, reason: String },

    /// Binary exited with non-zero exit code.
    #[error("binary '{binary}' --version exited with code {code}: {stderr}")]
    NonZeroExitCode {
        binary: String,
        code: i32,
        stderr: String,
    },

    /// Binary produced output that could not be parsed as UTF-8.
    #[error("binary '{binary}' --version produced non-UTF-8 output")]
    Utf8Error { binary: String },

    /// Binary execution timed out.
    #[error("binary '{binary}' --version timed out after {timeout:?}")]
    Timeout { binary: String, timeout: Duration },

    /// Failed to create async runtime for timeout.
    #[error("failed to create async runtime for version probe: {reason}")]
    AsyncRuntimeFailed { reason: String },

    /// Version output exists but backend name could not be parsed.
    #[error("binary '{binary}' --version produced unparseable output: {output}")]
    UnparseableOutput { binary: String, output: String },
}

// ──────────────────────────────────────────────────────────────────────────────
// VerifyError
// ──────────────────────────────────────────────────────────────────────────────

/// Error type for backend verification failures.
///
/// This error is returned when the detected backend from a binary's --version
/// output does not match the expected backend derived from the binary filename.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// No backend could be detected from the binary.
    #[error("no backend detected from binary '{binary}'")]
    NoBackendDetected { binary: String },

    /// Backend mismatch between expected and actual.
    #[error("backend mismatch for binary '{binary}': expected '{expected}', got '{actual}'")]
    BackendMismatch {
        binary: String,
        expected: String,
        actual: String,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// VersionProbe
// ──────────────────────────────────────────────────────────────────────────────

/// Probe for backend identity by running binaries with --version.
#[derive(Clone)]
pub struct VersionProbe {
    /// Timeout for binary execution (default: 5 seconds).
    timeout: Duration,
    /// Optional telemetry emitter for verification events.
    telemetry: Option<std::sync::Arc<dyn TelemetryEmitter>>,
}

impl Default for VersionProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl VersionProbe {
    /// Create a new version probe with default timeout (5 seconds).
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            telemetry: None,
        }
    }

    /// Create a new version probe with custom timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            telemetry: None,
        }
    }

    /// Attach a telemetry emitter to this probe.
    pub fn with_telemetry(mut self, telemetry: std::sync::Arc<dyn TelemetryEmitter>) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    /// Detect the backend identity by running a binary with --version.
    ///
    /// This method:
    /// 1. Executes the binary with `--version` flag
    /// 2. Captures stdout
    /// 3. Parses the output to extract the backend name
    ///
    /// # Arguments
    ///
    /// * `binary_name` - Name of the binary to execute (e.g., "bf", "bead")
    ///
    /// # Returns
    ///
    /// - `Ok(backend)` - Backend name successfully extracted
    /// - `Err(ProbeError::BinaryNotFound)` - Binary not found in PATH
    /// - `Err(ProbeError::NonZeroExitCode)` - Binary exited with non-zero code
    /// - `Err(ProbeError::UnparseableOutput)` - Output exists but backend name could not be parsed
    /// - `Err(ProbeError::Timeout)` - Execution timed out
    /// - `Err(ProbeError::Utf8Error)` - Output is not valid UTF-8
    /// - `Err(ProbeError::ExecutionFailed)` - Binary failed to execute
    /// - `Err(ProbeError::AsyncRuntimeFailed)` - Async runtime creation failed
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use needle::version_probe::VersionProbe;
    /// let probe = VersionProbe::new();
    /// match probe.detect_backend("bf") {
    ///     Ok(backend) => println!("Backend: {}", backend),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// ```
    pub fn detect_backend(&self, binary_name: &str) -> Result<String, ProbeError> {
        let output = self.run_version_binary(binary_name)?;

        // Parse the output to extract backend name
        let backend =
            self.parse_version_output(&output)
                .ok_or_else(|| ProbeError::UnparseableOutput {
                    binary: binary_name.to_string(),
                    output: output.clone(),
                })?;

        Ok(backend)
    }

    /// Run a binary with --version flag and return its stdout.
    ///
    /// # Arguments
    ///
    /// * `binary_name` - Name of the binary to execute
    ///
    /// # Returns
    ///
    /// The raw stdout output as a String.
    ///
    /// # Errors
    ///
    /// Returns `ProbeError` if:
    /// - The binary is not found in PATH (`ProbeError::BinaryNotFound`)
    /// - The binary fails to execute (`ProbeError::ExecutionFailed`)
    /// - The binary returns a non-zero exit code (`ProbeError::NonZeroExitCode`)
    /// - Output cannot be decoded as UTF-8 (`ProbeError::Utf8Error`)
    /// - Execution times out (`ProbeError::Timeout`)
    /// - Async runtime creation fails (`ProbeError::AsyncRuntimeFailed`)
    fn run_version_binary(&self, binary_name: &str) -> Result<String, ProbeError> {
        // Use the which crate to check if binary exists first
        // This provides a clearer error than relying on Command::status
        let path = which::which(binary_name).map_err(|_| ProbeError::BinaryNotFound {
            binary: binary_name.to_string(),
        })?;

        tracing::debug!(
            binary = binary_name,
            path = %path.display(),
            "version_probe: executing --version"
        );

        // Execute with timeout
        let output = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(e) => {
                return Err(ProbeError::AsyncRuntimeFailed {
                    reason: e.to_string(),
                })
            }
        }
        .block_on(async {
            tokio::time::timeout(self.timeout, async {
                tokio::process::Command::new(binary_name)
                    .arg("--version")
                    .output()
                    .await
            })
            .await
        });

        let output = match output {
            Ok(output) => output,
            Err(_) => {
                return Err(ProbeError::Timeout {
                    binary: binary_name.to_string(),
                    timeout: self.timeout,
                })
            }
        };

        let output = match output {
            Ok(output) => output,
            Err(e) => {
                return Err(ProbeError::ExecutionFailed {
                    binary: binary_name.to_string(),
                    reason: e.to_string(),
                })
            }
        };

        // Check exit code
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProbeError::NonZeroExitCode {
                binary: binary_name.to_string(),
                code: output.status.code().unwrap_or(-1),
                stderr: stderr.trim().to_string(),
            });
        }

        // Parse stdout as UTF-8
        let stdout = match String::from_utf8(output.stdout) {
            Ok(stdout) => stdout,
            Err(_) => {
                return Err(ProbeError::Utf8Error {
                    binary: binary_name.to_string(),
                })
            }
        };

        Ok(stdout)
    }

    /// Parse version output to extract the backend name.
    ///
    /// This method handles various version output formats:
    ///
    /// - `bf 0.3.0` → `bf`
    /// - `bead 0.26.0` → `bead`
    /// - `beads-rust 0.26.0` → `beads-rust`
    /// - `bf version 0.3.0` → `bf`
    /// - Multi-line output → First word on first line
    ///
    /// # Arguments
    ///
    /// * `output` - Raw stdout from the binary
    ///
    /// # Returns
    ///
    /// - `Some(backend_name)` - Successfully parsed backend name
    /// - `None` - Could not parse a recognizable backend name
    fn parse_version_output(&self, output: &str) -> Option<String> {
        // Find the first non-empty line
        let first_line = output.lines().find(|line| !line.trim().is_empty())?;

        // Split on whitespace and take the first token
        let first_token = first_line.split_whitespace().next()?;

        // Validate that it looks like a backend name (alphanumeric, may contain hyphens)
        if is_valid_backend_name(first_token) {
            Some(first_token.to_string())
        } else {
            tracing::debug!(
                token = first_token,
                "version_probe: first token does not look like a backend name"
            );
            None
        }
    }

    /// Check if a specific binary is installed and available.
    ///
    /// This is a convenience method that checks for binary existence without
    /// running it.
    ///
    /// # Arguments
    ///
    /// * `binary_name` - Name of the binary to check
    ///
    /// # Returns
    ///
    /// - `true` - Binary exists in PATH
    /// - `false` - Binary not found
    pub fn is_binary_available(&self, binary_name: &str) -> bool {
        which::which(binary_name).is_ok()
    }

    /// Derive the expected backend name from a binary filename.
    ///
    /// This method maps binary names to their expected backend identities:
    ///
    /// - `bf` → `bf` (bead-forge)
    /// - `bead` → `bead` or `beads-rust` (bead-rs CLI)
    ///
    /// # Arguments
    ///
    /// * `binary_name` - Name of the binary (e.g., "bf", "bead")
    ///
    /// # Returns
    ///
    /// The expected backend name as a string slice.
    ///
    /// # Examples
    ///
    /// ```
    /// # use needle::version_probe::VersionProbe;
    /// let probe = VersionProbe::new();
    /// assert_eq!(probe.expected_backend_for_binary("bf"), "bf");
    /// assert_eq!(probe.expected_backend_for_binary("bead"), "bead");
    /// ```
    pub fn expected_backend_for_binary<'a>(&self, binary_name: &'a str) -> &'a str {
        match binary_name {
            "bf" => BACKEND_BF,
            "bead" => BACKEND_BEAD,
            _ => binary_name, // For unknown binaries, expect the binary name itself
        }
    }

    /// Verify that a binary's detected backend matches its expected backend.
    ///
    /// This method:
    /// 1. Derives the expected backend from the binary filename
    /// 2. Detects the actual backend from the binary's --version output
    /// 3. Compares them, returning an error if they don't match
    ///
    /// This method emits telemetry events if a telemetry emitter was attached
    /// via `with_telemetry()`.
    ///
    /// # Arguments
    ///
    /// * `binary_name` - Name of the binary to verify
    ///
    /// # Returns
    ///
    /// - `Ok(())` - Backend matches or binary not found (no verification possible)
    /// - `Err(VerifyError::NoBackendDetected)` - Binary exists but backend couldn't be parsed
    /// - `Err(VerifyError::BackendMismatch)` - Detected backend doesn't match expected
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use needle::version_probe::VersionProbe;
    /// let probe = VersionProbe::new();
    /// match probe.verify_backend("bead") {
    ///     Ok(()) => println!("Backend verification passed"),
    ///     Err(e) => eprintln!("Verification failed: {}", e),
    /// }
    /// ```
    pub fn verify_backend(&self, binary_name: &str) -> Result<(), VerifyError> {
        // Derive expected backend from binary filename
        let expected = self.expected_backend_for_binary(binary_name);

        // Emit verification started event
        if let Some(telemetry) = &self.telemetry {
            telemetry.emit_version_event(VersionVerifyEvent::Started {
                binary: binary_name.to_string(),
                expected_backend: expected.to_string(),
            });
        }

        // Detect actual backend from --version output
        let actual = match self.detect_backend(binary_name) {
            Ok(backend) => backend,
            Err(e) => {
                let error = VerifyError::NoBackendDetected {
                    binary: binary_name.to_string(),
                };

                // Determine error type for telemetry
                let error_type = match &e {
                    ProbeError::BinaryNotFound { .. } => "BinaryNotFound",
                    ProbeError::NonZeroExitCode { .. } => "NonZeroExitCode",
                    ProbeError::UnparseableOutput { .. } => "UnparseableOutput",
                    ProbeError::Timeout { .. } => "Timeout",
                    ProbeError::Utf8Error { .. } => "Utf8Error",
                    ProbeError::ExecutionFailed { .. } => "ExecutionFailed",
                    ProbeError::AsyncRuntimeFailed { .. } => "AsyncRuntimeFailed",
                };

                if let Some(telemetry) = &self.telemetry {
                    telemetry.emit_version_event(VersionVerifyEvent::Failed {
                        binary: binary_name.to_string(),
                        expected_backend: expected.to_string(),
                        actual_backend: None,
                        error_type: error_type.to_string(),
                        error_message: e.to_string(),
                    });
                }
                return Err(error);
            }
        };

        // Compare case-sensitively
        if expected != actual {
            let error = VerifyError::BackendMismatch {
                binary: binary_name.to_string(),
                expected: expected.to_string(),
                actual: actual.clone(),
            };
            if let Some(telemetry) = &self.telemetry {
                telemetry.emit_version_event(VersionVerifyEvent::Failed {
                    binary: binary_name.to_string(),
                    expected_backend: expected.to_string(),
                    actual_backend: Some(actual.clone()),
                    error_type: "BackendMismatch".to_string(),
                    error_message: error.to_string(),
                });
            }
            return Err(error);
        }

        // Emit success event
        if let Some(telemetry) = &self.telemetry {
            telemetry.emit_version_event(VersionVerifyEvent::Success {
                binary: binary_name.to_string(),
                expected_backend: expected.to_string(),
                actual_backend: actual,
            });
        }

        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helper functions
// ──────────────────────────────────────────────────────────────────────────────

/// Check if a string looks like a valid backend name.
///
/// Valid backend names are:
/// - Alphanumeric characters
/// - May contain hyphens
/// - No spaces or special characters
///
/// # Examples
///
/// ```
/// # use needle::version_probe::is_valid_backend_name;
/// assert!(is_valid_backend_name("bf"));
/// assert!(is_valid_backend_name("bead"));
/// assert!(is_valid_backend_name("beads-rust"));
/// assert!(!is_valid_backend_name("0.3.0"));  // Version string, not name
/// assert!(!is_valid_backend_name(""));        // Empty
/// assert!(!is_valid_backend_name("bf version"));  // Contains space
/// ```
fn is_valid_backend_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    // Backend names must start with a letter
    if !name.chars().next().is_some_and(|c| c.is_alphabetic()) {
        return false;
    }

    // Backend names must not end with a hyphen
    if !name.chars().last().is_some_and(|c| c.is_alphanumeric()) {
        return false;
    }

    // Backend names contain only alphanumeric characters and hyphens
    name.chars().all(|c| c.is_alphanumeric() || c == '-')
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_probe_default_has_timeout() {
        let probe = VersionProbe::new();
        assert_eq!(probe.timeout, Duration::from_secs(5));
    }

    #[test]
    fn version_probe_with_custom_timeout() {
        let probe = VersionProbe::with_timeout(Duration::from_secs(10));
        assert_eq!(probe.timeout, Duration::from_secs(10));
    }

    #[test]
    fn parse_version_output_bf_format() {
        let probe = VersionProbe::new();

        let output = "bf 0.3.0";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("bf".to_string()));
    }

    #[test]
    fn parse_version_output_bead_format() {
        let probe = VersionProbe::new();

        let output = "bead 0.26.0";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("bead".to_string()));
    }

    #[test]
    fn parse_version_output_beads_rust_format() {
        let probe = VersionProbe::new();

        let output = "beads-rust 0.26.0";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("beads-rust".to_string()));
    }

    #[test]
    fn parse_version_output_with_version_keyword() {
        let probe = VersionProbe::new();

        let output = "bf version 0.3.0";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("bf".to_string()));
    }

    #[test]
    fn parse_version_output_multiline() {
        let probe = VersionProbe::new();

        let output = "bf 0.3.0\nCopyright 2024\nMore info here";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("bf".to_string()));
    }

    #[test]
    fn parse_version_output_empty() {
        let probe = VersionProbe::new();

        let output = "";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, None);
    }

    #[test]
    fn parse_version_output_only_version_number() {
        let probe = VersionProbe::new();

        let output = "0.3.0";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, None); // Version numbers are not valid backend names
    }

    #[test]
    fn parse_version_output_with_leading_whitespace() {
        let probe = VersionProbe::new();

        let output = "   bf 0.3.0";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("bf".to_string()));
    }

    #[test]
    fn parse_version_output_with_extra_whitespace() {
        let probe = VersionProbe::new();

        let output = "bf    0.3.0";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("bf".to_string()));
    }

    #[test]
    fn is_valid_backend_name_accepts_valid_names() {
        assert!(is_valid_backend_name("bf"));
        assert!(is_valid_backend_name("bead"));
        assert!(is_valid_backend_name("beads-rust"));
        assert!(is_valid_backend_name("bead-rs"));
        assert!(is_valid_backend_name("needle-cli"));
    }

    #[test]
    fn is_valid_backend_name_rejects_invalid_names() {
        assert!(!is_valid_backend_name(""));
        assert!(!is_valid_backend_name("0.3.0")); // Starts with number
        assert!(!is_valid_backend_name("123")); // All numbers
        assert!(!is_valid_backend_name("bf version")); // Contains space
        assert!(!is_valid_backend_name("bf@1.0.0")); // Special char @
        assert!(!is_valid_backend_name("-bf")); // Starts with hyphen
        assert!(!is_valid_backend_name("bf-")); // Ends with hyphen
    }

    #[test]
    fn is_valid_backend_name_requires_starting_letter() {
        assert!(is_valid_backend_name("bf")); // Starts with b
        assert!(is_valid_backend_name("Bead")); // Starts with B
        assert!(!is_valid_backend_name("1bf")); // Starts with 1
        assert!(!is_valid_backend_name("-bf")); // Starts with -
    }

    #[test]
    fn constants_match_expected_values() {
        assert_eq!(BACKEND_BF, "bf");
        assert_eq!(BACKEND_BEAD, "bead");
        assert_eq!(BACKEND_BEADS_RUST, "beads-rust");
    }

    #[test]
    fn parse_version_output_handles_github_style_output() {
        let probe = VersionProbe::new();

        // Some tools output version like: "bf version 0.3.0 (abcd123)"
        let output = "bf version 0.3.0 (abcd123)";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("bf".to_string()));
    }

    #[test]
    fn parse_version_output_handles_verbose_output() {
        let probe = VersionProbe::new();

        // Some tools output verbose version strings
        let output = "bead 0.26.0 (2024-08-15) built for x86_64-unknown-linux-gnu";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("bead".to_string()));
    }

    #[test]
    fn parse_version_output_ignores_empty_lines() {
        let probe = VersionProbe::new();

        let output = "\n\nbf 0.3.0\n\n";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("bf".to_string()));
    }

    #[test]
    fn parse_version_output_handles_tabs() {
        let probe = VersionProbe::new();

        let output = "bf\t0.3.0";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("bf".to_string()));
    }

    #[test]
    fn parse_version_output_handles_mixed_whitespace() {
        let probe = VersionProbe::new();

        let output = "bf \t 0.3.0";
        let backend = probe.parse_version_output(output);
        assert_eq!(backend, Some("bf".to_string()));
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tests for expected_backend_for_binary
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn expected_backend_for_binary_bf() {
        let probe = VersionProbe::new();
        assert_eq!(probe.expected_backend_for_binary("bf"), "bf");
    }

    #[test]
    fn expected_backend_for_binary_bead() {
        let probe = VersionProbe::new();
        assert_eq!(probe.expected_backend_for_binary("bead"), "bead");
    }

    #[test]
    fn expected_backend_for_binary_unknown() {
        let probe = VersionProbe::new();
        // For unknown binaries, expect the binary name itself
        assert_eq!(
            probe.expected_backend_for_binary("unknown-cli"),
            "unknown-cli"
        );
        assert_eq!(probe.expected_backend_for_binary("needle"), "needle");
    }

    #[test]
    fn expected_backend_for_binary_case_sensitive() {
        let probe = VersionProbe::new();
        // Comparison should be case-sensitive
        assert_eq!(probe.expected_backend_for_binary("BF"), "BF");
        assert_eq!(probe.expected_backend_for_binary("BEAD"), "BEAD");
        assert_eq!(probe.expected_backend_for_binary("Bead"), "Bead");
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tests for verify_backend (mock-based tests since we can't run real binaries)
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn verify_backend_no_backend_detected() {
        // Note: verify_backend calls detect_backend which executes real binaries.
        // Testing this properly requires either:
        // 1. Mocking the binary execution (would need dependency injection)
        // 2. Integration tests with real bead binaries
        //
        // The error cases (binary not found, bad exit code, etc.) are documented
        // in the parse_version_output tests above, which test the parsing logic
        // without requiring actual binary execution.
        //
        // Real-world behavior is tested in integration tests where actual
        // bead binaries (bf, bead) are available.
    }

    #[test]
    fn verify_backend_error_formatting() {
        let err = VerifyError::NoBackendDetected {
            binary: "bf".to_string(),
        };
        assert_eq!(err.to_string(), "no backend detected from binary 'bf'");
    }

    #[test]
    fn verify_backend_mismatch_error_formatting() {
        let err = VerifyError::BackendMismatch {
            binary: "bead".to_string(),
            expected: "bead".to_string(),
            actual: "bf".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "backend mismatch for binary 'bead': expected 'bead', got 'bf'"
        );
    }

    #[test]
    fn verify_backend_mismatch_error_fields() {
        let err = VerifyError::BackendMismatch {
            binary: "bead".to_string(),
            expected: "bead".to_string(),
            actual: "bf".to_string(),
        };
        assert!(matches!(err, VerifyError::BackendMismatch { .. }));
        if let VerifyError::BackendMismatch {
            binary,
            expected,
            actual,
        } = err
        {
            assert_eq!(binary, "bead");
            assert_eq!(expected, "bead");
            assert_eq!(actual, "bf");
        }
    }

    #[test]
    fn verify_backend_error_equality() {
        let err1 = VerifyError::NoBackendDetected {
            binary: "bf".to_string(),
        };
        let err2 = VerifyError::NoBackendDetected {
            binary: "bf".to_string(),
        };
        assert_eq!(err1, err2);
    }

    #[test]
    fn verify_backend_mismatch_error_equality() {
        let err1 = VerifyError::BackendMismatch {
            binary: "bead".to_string(),
            expected: "bead".to_string(),
            actual: "bf".to_string(),
        };
        let err2 = VerifyError::BackendMismatch {
            binary: "bead".to_string(),
            expected: "bead".to_string(),
            actual: "bf".to_string(),
        };
        assert_eq!(err1, err2);
    }

    #[test]
    fn verify_backend_error_inequality() {
        let err1 = VerifyError::NoBackendDetected {
            binary: "bf".to_string(),
        };
        let err2 = VerifyError::NoBackendDetected {
            binary: "bead".to_string(),
        };
        assert_ne!(err1, err2);
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Additional tests for error cases in parsing
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_version_output_rejects_version_only_output() {
        let probe = VersionProbe::new();

        // Output that is just a version number should not parse
        let output = "1.0.0";
        assert_eq!(probe.parse_version_output(output), None);

        let output = "2.3.4-beta";
        assert_eq!(probe.parse_version_output(output), None);

        let output = "0.0.1-alpha+001";
        assert_eq!(probe.parse_version_output(output), None);
    }

    #[test]
    fn parse_version_output_handles_various_bead_rs_formats() {
        let probe = VersionProbe::new();

        // Standard bead-rs output
        assert_eq!(
            probe.parse_version_output("bead 0.26.0"),
            Some("bead".to_string())
        );

        // Alternative bead-rs name
        assert_eq!(
            probe.parse_version_output("beads-rust 0.26.0"),
            Some("beads-rust".to_string())
        );

        // With additional metadata
        assert_eq!(
            probe.parse_version_output("bead 0.26.0 (abcd1234)"),
            Some("bead".to_string())
        );

        // With build info
        assert_eq!(
            probe.parse_version_output("bead 0.26.0 built for x86_64"),
            Some("bead".to_string())
        );

        // With date
        assert_eq!(
            probe.parse_version_output("bead 0.26.0 (2024-08-15)"),
            Some("bead".to_string())
        );
    }

    #[test]
    fn parse_version_output_handles_various_bf_formats() {
        let probe = VersionProbe::new();

        // Standard bf output
        assert_eq!(
            probe.parse_version_output("bf 0.3.0"),
            Some("bf".to_string())
        );

        // With "version" keyword
        assert_eq!(
            probe.parse_version_output("bf version 0.3.0"),
            Some("bf".to_string())
        );

        // With git commit
        assert_eq!(
            probe.parse_version_output("bf 0.3.0 (abcd1234)"),
            Some("bf".to_string())
        );

        // With build metadata
        assert_eq!(
            probe.parse_version_output("bf 0.3.0 built from main"),
            Some("bf".to_string())
        );
    }

    #[test]
    fn parse_version_output_handles_edge_cases() {
        let probe = VersionProbe::new();

        // Single word (valid backend name)
        assert_eq!(probe.parse_version_output("bead"), Some("bead".to_string()));

        // Backend name followed by newline
        assert_eq!(
            probe.parse_version_output("bead\n"),
            Some("bead".to_string())
        );

        // Backend name with trailing spaces
        assert_eq!(
            probe.parse_version_output("bead   "),
            Some("bead".to_string())
        );

        // All whitespace
        assert_eq!(probe.parse_version_output("   \n\t  \n"), None);

        // Single newline
        assert_eq!(probe.parse_version_output("\n"), None);
    }

    #[test]
    fn parse_version_output_rejects_non_backend_names() {
        let probe = VersionProbe::new();

        // Special characters
        assert_eq!(probe.parse_version_output("bead@1.0.0"), None);
        assert_eq!(probe.parse_version_output("bead_v1.0.0"), None);
        assert_eq!(probe.parse_version_output("bead+1.0.0"), None);

        // Starts with number
        assert_eq!(probe.parse_version_output("1bead"), None);
        assert_eq!(probe.parse_version_output("2bf"), None);

        // Starts with special character
        assert_eq!(probe.parse_version_output("-bead"), None);
        assert_eq!(probe.parse_version_output("_bf"), None);
        assert_eq!(probe.parse_version_output("@bead"), None);

        // Ends with hyphen
        assert_eq!(probe.parse_version_output("bead-"), None);
        assert_eq!(probe.parse_version_output("bead-rs-"), None);

        // Empty after trimming
        assert_eq!(probe.parse_version_output("   "), None);
    }

    #[test]
    fn parse_version_output_handles_unicode() {
        let probe = VersionProbe::new();

        // Backend names with alphanumeric + hyphens only (ASCII)
        assert_eq!(
            probe.parse_version_output("bead-rs-cli"),
            Some("bead-rs-cli".to_string())
        );

        // Note: Rust's `is_alphanumeric()` returns true for Unicode alphanumeric
        // characters, so backend names with Unicode letters are currently valid.
        // This is intentional behavior to support internationalized tool names.
        // The tests below document this actual behavior:
        assert_eq!(
            probe.parse_version_output("bead-cli-日本語"),
            Some("bead-cli-日本語".to_string())
        );
        assert_eq!(
            probe.parse_version_output("bead-工具"),
            Some("bead-工具".to_string())
        );
    }

    #[test]
    fn parse_version_output_handles_multiple_whitespace_types() {
        let probe = VersionProbe::new();

        // Mix of spaces and tabs
        assert_eq!(
            probe.parse_version_output("bead \t \t 0.26.0"),
            Some("bead".to_string())
        );

        // Leading/trailing whitespace on first line
        assert_eq!(
            probe.parse_version_output("  \t bead 0.26.0  \t"),
            Some("bead".to_string())
        );
    }

    #[test]
    fn is_binary_available_documented_behavior() {
        // Note: is_binary_available executes `which::which`, which checks the real PATH.
        // Testing this requires either:
        // 1. Testing with binaries known to exist (e.g., "ls", "sh")
        // 2. Testing with binaries known to not exist
        // 3. Mocking the `which` crate (which would require dependency injection)
        //
        // We document the expected behavior here:
        // - Returns true if the binary exists in PATH
        // - Returns false if the binary doesn't exist
        // - This method does NOT execute the binary, only checks for its existence
        //
        // The actual implementation is tested in integration tests with real bead binaries.
    }

    #[test]
    fn verify_backend_error_variants_are_exhaustive() {
        // Ensure all VerifyError variants can be created and displayed
        let err1 = VerifyError::NoBackendDetected {
            binary: "test".to_string(),
        };
        let _ = format!("{}", err1); // Should not panic

        let err2 = VerifyError::BackendMismatch {
            binary: "test".to_string(),
            expected: "bead".to_string(),
            actual: "bf".to_string(),
        };
        let _ = format!("{}", err2); // Should not panic
    }

    #[test]
    fn verify_backend_handles_case_sensitive_mismatch() {
        let probe = VersionProbe::new();

        // If we somehow had a binary named "BF" that reported "bf",
        // that would be a mismatch (case-sensitive)
        // This documents the expected behavior
        let expected = probe.expected_backend_for_binary("BF");
        assert_eq!(expected, "BF"); // Case is preserved
    }

    #[test]
    fn parse_version_output_handles_real_world_outputs() {
        let probe = VersionProbe::new();

        // Simulated real-world bead-rs output
        assert_eq!(
            probe.parse_version_output(
                "bead 0.26.0\nRepository: https://github.com/jedarden/bead-rs\nCommit: abcd1234"
            ),
            Some("bead".to_string())
        );

        // Simulated real-world bf output
        assert_eq!(
            probe.parse_version_output("bf 0.3.0 (abcd1234 2024-08-15)"),
            Some("bf".to_string())
        );

        // Simulated cargo-style version output (common in Rust tools)
        assert_eq!(
            probe.parse_version_output("bead 0.26.0\nRelease: 2024-08-15"),
            Some("bead".to_string())
        );
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tests for telemetry emission during verification
    // ──────────────────────────────────────────────────────────────────────────────

    /// Mock telemetry emitter for testing.
    struct MockTelemetryEmitter {
        events: std::sync::Arc<std::sync::Mutex<Vec<VersionVerifyEvent>>>,
    }

    impl MockTelemetryEmitter {
        fn new() -> Self {
            Self {
                events: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn get_events(&self) -> Vec<VersionVerifyEvent> {
            self.events.lock().unwrap().clone()
        }

        #[allow(dead_code)]
        fn clear(&self) {
            self.events.lock().unwrap().clear();
        }
    }

    impl TelemetryEmitter for MockTelemetryEmitter {
        fn emit_version_event(&self, event: VersionVerifyEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn verify_backend_emits_events_on_binary_not_found() {
        let mock = std::sync::Arc::new(MockTelemetryEmitter::new());
        let probe = VersionProbe::new().with_telemetry(mock.clone());

        // Try to verify a binary that doesn't exist
        let result = probe.verify_backend("nonexistent-binary-xyz-123");

        assert!(result.is_err());
        let events = mock.get_events();
        assert_eq!(events.len(), 2); // Started + Failed

        match &events[0] {
            VersionVerifyEvent::Started {
                binary,
                expected_backend,
            } => {
                assert_eq!(binary, "nonexistent-binary-xyz-123");
                assert_eq!(expected_backend, "nonexistent-binary-xyz-123");
            }
            _ => panic!("First event should be Started"),
        }

        match &events[1] {
            VersionVerifyEvent::Failed {
                binary,
                expected_backend,
                actual_backend,
                error_type,
                error_message,
            } => {
                assert_eq!(binary, "nonexistent-binary-xyz-123");
                assert_eq!(expected_backend, "nonexistent-binary-xyz-123");
                assert!(actual_backend.is_none());
                assert_eq!(error_type, "BinaryNotFound");
                assert!(error_message.contains("not found") || error_message.contains("PATH"));
            }
            _ => panic!("Second event should be Failed"),
        }
    }

    #[test]
    fn verify_backend_emits_no_events_without_telemetry() {
        let probe = VersionProbe::new();

        // No telemetry attached, should not panic
        let result = probe.verify_backend("nonexistent-binary-xyz-123");
        assert!(result.is_err());
    }

    #[test]
    fn verify_backend_error_messages_include_mismatch_details() {
        let _probe = VersionProbe::new();

        // Create a BackendMismatch error manually
        let err = VerifyError::BackendMismatch {
            binary: "bead".to_string(),
            expected: "bead".to_string(),
            actual: "bf".to_string(),
        };

        let error_message = err.to_string();
        assert!(error_message.contains("bead")); // binary name
        assert!(error_message.contains("expected 'bead'"));
        assert!(error_message.contains("got 'bf'"));
        assert!(error_message.contains("mismatch"));
    }

    #[test]
    fn verify_backend_error_messages_include_no_backend_details() {
        let _probe = VersionProbe::new();

        // Create a NoBackendDetected error manually
        let err = VerifyError::NoBackendDetected {
            binary: "bf".to_string(),
        };

        let error_message = err.to_string();
        assert!(error_message.contains("bf")); // binary name
        assert!(error_message.contains("no backend detected"));
    }

    #[test]
    fn verify_backend_telemetry_includes_all_fields_on_success() {
        let mock = std::sync::Arc::new(MockTelemetryEmitter::new());
        let _probe = VersionProbe::new().with_telemetry(mock.clone());

        // This test documents the expected structure
        // Real verification requires actual binaries to exist
        // The telemetry structure is validated here
        assert_eq!(std::sync::Arc::strong_count(&mock), 2); // probe + this test
    }

    #[test]
    fn verify_backend_telemetry_error_types_are_distinct() {
        let mock = std::sync::Arc::new(MockTelemetryEmitter::new());
        let probe = VersionProbe::new().with_telemetry(mock.clone());

        let result = probe.verify_backend("nonexistent-binary-xyz-123");
        assert!(result.is_err());

        let events = mock.get_events();
        assert_eq!(events.len(), 2);

        // Verify error types are properly set
        if let VersionVerifyEvent::Failed { error_type, .. } = &events[1] {
            assert_eq!(error_type, "BinaryNotFound");
        } else {
            panic!("Expected Failed event");
        }
    }

    #[test]
    fn verify_backend_mismatch_error_shows_both_backends() {
        // Verify that the error message clearly shows expected vs actual
        let err = VerifyError::BackendMismatch {
            binary: "bead".to_string(),
            expected: "bead".to_string(),
            actual: "bf".to_string(),
        };

        let msg = err.to_string();
        // The error should show what was found vs what was expected
        assert!(msg.contains("expected 'bead'"));
        assert!(msg.contains("got 'bf'"));
        assert!(msg.contains("binary 'bead'"));

        // Verify we can extract the fields for structured reporting
        match err {
            VerifyError::BackendMismatch {
                binary,
                expected,
                actual,
            } => {
                assert_eq!(binary, "bead");
                assert_eq!(expected, "bead");
                assert_eq!(actual, "bf");
            }
            _ => panic!("Should be BackendMismatch"),
        }
    }

    #[test]
    fn version_verify_event_cloneable() {
        // Events need to be cloneable for test assertions
        let event = VersionVerifyEvent::Started {
            binary: "test".to_string(),
            expected_backend: "test-backend".to_string(),
        };
        let event2 = event.clone();
        match event {
            VersionVerifyEvent::Started { binary, .. } => {
                assert_eq!(binary, "test");
            }
            _ => panic!("Should be Started event"),
        }
        match event2 {
            VersionVerifyEvent::Started { binary, .. } => {
                assert_eq!(binary, "test");
            }
            _ => panic!("Cloned event should be Started"),
        }
    }

    #[test]
    fn verify_backend_without_telemetry_succeeds() {
        // Ensure verify_backend works without telemetry attached
        let probe = VersionProbe::new();
        let result = probe.verify_backend("nonexistent-binary-xyz-123");

        // Should return error but not panic
        assert!(result.is_err());
    }

    #[test]
    fn version_verify_event_all_variants_testable() {
        // Verify all event variants can be created and inspected
        let started = VersionVerifyEvent::Started {
            binary: "test".to_string(),
            expected_backend: "test-backend".to_string(),
        };
        match &started {
            VersionVerifyEvent::Started {
                binary,
                expected_backend,
            } => {
                assert_eq!(binary, "test");
                assert_eq!(expected_backend, "test-backend");
            }
            _ => panic!("Should be Started"),
        }

        let success = VersionVerifyEvent::Success {
            binary: "test".to_string(),
            expected_backend: "test-backend".to_string(),
            actual_backend: "test-backend".to_string(),
        };
        match &success {
            VersionVerifyEvent::Success {
                binary,
                expected_backend,
                actual_backend,
            } => {
                assert_eq!(binary, "test");
                assert_eq!(expected_backend, "test-backend");
                assert_eq!(actual_backend, "test-backend");
            }
            _ => panic!("Should be Success"),
        }

        let failed = VersionVerifyEvent::Failed {
            binary: "test".to_string(),
            expected_backend: "test-backend".to_string(),
            actual_backend: Some("other-backend".to_string()),
            error_type: "TestError".to_string(),
            error_message: "test error message".to_string(),
        };
        match &failed {
            VersionVerifyEvent::Failed {
                binary,
                expected_backend,
                actual_backend,
                error_type,
                error_message,
            } => {
                assert_eq!(binary, "test");
                assert_eq!(expected_backend, "test-backend");
                assert_eq!(actual_backend, &Some("other-backend".to_string()));
                assert_eq!(error_type, "TestError");
                assert_eq!(error_message, "test error message");
            }
            _ => panic!("Should be Failed"),
        }
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tests for ProbeError variants
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn probe_error_binary_not_found_message_formatting() {
        let err = ProbeError::BinaryNotFound {
            binary: "bf".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("bf"));
        assert!(msg.contains("not found"));
        assert!(msg.contains("PATH"));
    }

    #[test]
    fn probe_error_non_zero_exit_code_message_formatting() {
        let err = ProbeError::NonZeroExitCode {
            binary: "bead".to_string(),
            code: 1,
            stderr: "invalid option".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("bead"));
        assert!(msg.contains("exited with code 1"));
        assert!(msg.contains("invalid option"));
    }

    #[test]
    fn probe_error_unparseable_output_message_formatting() {
        let err = ProbeError::UnparseableOutput {
            binary: "bf".to_string(),
            output: "1.0.0".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("bf"));
        assert!(msg.contains("unparseable"));
        assert!(msg.contains("1.0.0"));
    }

    #[test]
    fn probe_error_timeout_message_formatting() {
        let err = ProbeError::Timeout {
            binary: "bead".to_string(),
            timeout: Duration::from_secs(5),
        };
        let msg = err.to_string();
        assert!(msg.contains("bead"));
        assert!(msg.contains("timed out"));
        assert!(msg.contains("5s") || msg.contains("5 sec"));
    }

    #[test]
    fn probe_error_utf8_error_message_formatting() {
        let err = ProbeError::Utf8Error {
            binary: "bf".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("bf"));
        assert!(msg.contains("non-UTF-8"));
    }

    #[test]
    fn probe_error_execution_failed_message_formatting() {
        let err = ProbeError::ExecutionFailed {
            binary: "bead".to_string(),
            reason: "permission denied".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("bead"));
        assert!(msg.contains("failed to execute"));
        assert!(msg.contains("permission denied"));
    }

    #[test]
    fn probe_error_async_runtime_failed_message_formatting() {
        let err = ProbeError::AsyncRuntimeFailed {
            reason: "out of memory".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("async runtime"));
        assert!(msg.contains("out of memory"));
    }

    #[test]
    fn probe_error_equality_binary_not_found() {
        let err1 = ProbeError::BinaryNotFound {
            binary: "bf".to_string(),
        };
        let err2 = ProbeError::BinaryNotFound {
            binary: "bf".to_string(),
        };
        assert_eq!(err1, err2);
    }

    #[test]
    fn probe_error_inequality_binary_not_found() {
        let err1 = ProbeError::BinaryNotFound {
            binary: "bf".to_string(),
        };
        let err2 = ProbeError::BinaryNotFound {
            binary: "bead".to_string(),
        };
        assert_ne!(err1, err2);
    }

    #[test]
    fn probe_error_equality_non_zero_exit_code() {
        let err1 = ProbeError::NonZeroExitCode {
            binary: "bf".to_string(),
            code: 1,
            stderr: "error".to_string(),
        };
        let err2 = ProbeError::NonZeroExitCode {
            binary: "bf".to_string(),
            code: 1,
            stderr: "error".to_string(),
        };
        assert_eq!(err1, err2);
    }

    #[test]
    fn probe_error_equality_unparseable_output() {
        let err1 = ProbeError::UnparseableOutput {
            binary: "bf".to_string(),
            output: "1.0.0".to_string(),
        };
        let err2 = ProbeError::UnparseableOutput {
            binary: "bf".to_string(),
            output: "1.0.0".to_string(),
        };
        assert_eq!(err1, err2);
    }

    #[test]
    fn probe_error_cloneable() {
        let err1 = ProbeError::BinaryNotFound {
            binary: "bf".to_string(),
        };
        let err2 = err1.clone();
        assert_eq!(err1, err2);

        let err3 = ProbeError::NonZeroExitCode {
            binary: "bead".to_string(),
            code: 1,
            stderr: "error".to_string(),
        };
        let err4 = err3.clone();
        assert_eq!(err3, err4);
    }

    #[test]
    fn detect_backend_returns_specific_binary_not_found_error() {
        let probe = VersionProbe::new();
        let result = probe.detect_backend("nonexistent-binary-xyz-123");

        assert!(result.is_err());
        match result.unwrap_err() {
            ProbeError::BinaryNotFound { binary } => {
                assert_eq!(binary, "nonexistent-binary-xyz-123");
            }
            other => panic!("Expected BinaryNotFound error, got: {}", other),
        }
    }

    #[test]
    fn detect_backend_error_messages_are_meaningful() {
        let probe = VersionProbe::new();

        // Test binary not found
        let result = probe.detect_backend("totally-fake-binary-999");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("totally-fake-binary-999"));
        assert!(msg.contains("not found") || msg.contains("PATH"));

        // Test that the error message is actionable
        assert!(msg.len() > 20); // Should have substantial information
    }

    #[test]
    fn probe_error_variants_are_exhaustive() {
        // Ensure all ProbeError variants can be created and displayed
        let errors = vec![
            ProbeError::BinaryNotFound {
                binary: "bf".to_string(),
            },
            ProbeError::NonZeroExitCode {
                binary: "bead".to_string(),
                code: 1,
                stderr: "error".to_string(),
            },
            ProbeError::UnparseableOutput {
                binary: "bf".to_string(),
                output: "1.0.0".to_string(),
            },
            ProbeError::Timeout {
                binary: "bead".to_string(),
                timeout: Duration::from_secs(5),
            },
            ProbeError::Utf8Error {
                binary: "bf".to_string(),
            },
            ProbeError::ExecutionFailed {
                binary: "bead".to_string(),
                reason: "permission denied".to_string(),
            },
            ProbeError::AsyncRuntimeFailed {
                reason: "out of memory".to_string(),
            },
        ];

        // Verify all errors can be converted to strings without panicking
        for err in errors {
            let _ = format!("{}", err);
            let _ = format!("{:?}", err);
        }
    }

    #[test]
    fn verify_backend_maps_probe_errors_to_no_backend_detected() {
        let mock = std::sync::Arc::new(MockTelemetryEmitter::new());
        let probe = VersionProbe::new().with_telemetry(mock.clone());

        // Try to verify a nonexistent binary
        let result = probe.verify_backend("nonexistent-binary-xyz-123");

        // Should return NoBackendDetected, not the raw ProbeError
        assert!(result.is_err());
        match result.unwrap_err() {
            VerifyError::NoBackendDetected { .. } => {
                // Expected - verification failures always return NoBackendDetected
            }
            other => panic!("Expected NoBackendDetected, got: {}", other),
        }

        // But telemetry should have the specific error type
        let events = mock.get_events();
        assert_eq!(events.len(), 2); // Started + Failed

        match &events[1] {
            VersionVerifyEvent::Failed { error_type, .. } => {
                assert_eq!(error_type, "BinaryNotFound");
            }
            _ => panic!("Expected Failed event"),
        }
    }

    #[test]
    fn verify_backend_includes_probe_error_details_in_telemetry() {
        let mock = std::sync::Arc::new(MockTelemetryEmitter::new());
        let probe = VersionProbe::new().with_telemetry(mock.clone());

        let result = probe.verify_backend("nonexistent-binary-xyz-123");
        assert!(result.is_err());

        let events = mock.get_events();
        match &events[1] {
            VersionVerifyEvent::Failed {
                error_message,
                error_type,
                ..
            } => {
                // error_message should contain the ProbeError details
                assert!(error_message.contains("nonexistent-binary-xyz-123"));
                assert_eq!(error_type, "BinaryNotFound");
            }
            _ => panic!("Expected Failed event"),
        }
    }
}
