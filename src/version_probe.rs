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

use anyhow::{Context, Result};
use thiserror::Error;

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
#[derive(Debug, Clone)]
pub struct VersionProbe {
    /// Timeout for binary execution (default: 5 seconds).
    timeout: Duration,
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
        }
    }

    /// Create a new version probe with custom timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
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
    /// - `Ok(Some(backend))` - Backend name successfully extracted
    /// - `Ok(None)` - Binary exists but backend name could not be parsed
    /// - `Err(e)` - Binary not found, execution failed, or other error
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use needle::version_probe::VersionProbe;
    /// let probe = VersionProbe::new();
    /// match probe.detect_backend("bf") {
    ///     Ok(Some(backend)) => println!("Backend: {}", backend),
    ///     Ok(None) => println!("Could not parse backend name"),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// ```
    pub fn detect_backend(&self, binary_name: &str) -> Result<Option<String>> {
        let output = self.run_version_binary(binary_name)?;

        // Parse the output to extract backend name
        let backend = self.parse_version_output(&output);

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
    /// Returns an error if:
    /// - The binary is not found in PATH
    /// - The binary fails to execute
    /// - The binary returns a non-zero exit code
    /// - Output cannot be decoded as UTF-8
    fn run_version_binary(&self, binary_name: &str) -> Result<String> {
        // Use the which crate to check if binary exists first
        // This provides a clearer error than relying on Command::status
        let path = which::which(binary_name)
            .with_context(|| format!("binary '{}' not found in PATH", binary_name))?;

        tracing::debug!(
            binary = binary_name,
            path = %path.display(),
            "version_probe: executing --version"
        );

        // Execute with timeout
        let output = tokio::runtime::Runtime::new()
            .context("failed to create async runtime for version probe")?
            .block_on(async {
                tokio::time::timeout(self.timeout, async {
                    tokio::process::Command::new(binary_name)
                        .arg("--version")
                        .output()
                        .await
                })
                .await
            })
            .context(format!(
                "version probe for '{}' timed out after {:?}",
                binary_name, self.timeout
            ))?
            .context(format!("failed to execute '{}' --version", binary_name))?;

        // Check exit code
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "binary '{}' --version exited with {}: {}",
                binary_name,
                output.status,
                stderr.trim()
            );
        }

        // Parse stdout as UTF-8
        let stdout = String::from_utf8(output.stdout).with_context(|| {
            format!(
                "binary '{}' --version produced non-UTF-8 output",
                binary_name
            )
        })?;

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

        // Detect actual backend from --version output
        let actual = self
            .detect_backend(binary_name)
            .map_err(|_| VerifyError::NoBackendDetected {
                binary: binary_name.to_string(),
            })?
            .ok_or_else(|| VerifyError::NoBackendDetected {
                binary: binary_name.to_string(),
            })?;

        // Compare case-sensitively
        if expected != actual {
            return Err(VerifyError::BackendMismatch {
                binary: binary_name.to_string(),
                expected: expected.to_string(),
                actual: actual.to_string(),
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
        let probe = VersionProbe::new();

        // This test uses a binary that doesn't exist, so it should return NoBackendDetected
        let result = probe.verify_backend("nonexistent-binary-xyz123");
        assert!(matches!(result, Err(VerifyError::NoBackendDetected { .. })));
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
}
