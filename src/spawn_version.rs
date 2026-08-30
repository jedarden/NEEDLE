//! Low-level binary spawning infrastructure for version probing.
//!
//! This module provides foundational functionality to spawn a binary process
//! with the `--version` flag and capture its stdout output. This is intentionally
//! minimal - it only handles spawning and output capture, with no parsing logic.
//!
//! ## Purpose
//!
//! This module serves as the building block for version detection. Child tasks
//! will build on this infrastructure to:
//! - Parse version output to extract backend names
//! - Implement timeout and retry logic
//! - Add validation and error handling
//!
//! ## Usage
//!
//! ```no_run
//! use needle::spawn_version::spawn_version_output;
//! use std::path::Path;
//!
//! # fn main() -> anyhow::Result<()> {
//! let binary_path = Path::new("/usr/bin/git");
//! match spawn_version_output(binary_path) {
//!     Ok(output) => println!("Raw version output: {}", output),
//!     Err(e) => eprintln!("Spawn failed: {}", e),
//! }
//! # Ok(())
//! # }
//! ```

use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};
use tracing::debug;

/// Parse backend name from version output.
///
/// Extracts the backend identity from various version output formats.
/// This handles different bead CLI backends (bf, bead-rs, etc.) and returns
/// a clean backend name without version numbers.
///
/// # Arguments
///
/// * `version_output` - Raw stdout output from a `--version` command
///
/// # Returns
///
/// Backend name as a String, or a sensible default for unknown formats
///
/// # Supported Formats
///
/// - `bf 0.x.y` → `"bf"`
/// - `bead 0.x.y` (bead-rs) → `"bead-rs"`
/// - `bead-rs 0.x.y` → `"bead-rs"`
/// - Custom format: `backend-name 0.x.y` → `"backend-name"`
/// - Unknown/malformed → `"unknown"`
///
/// # Examples
///
/// ```
/// # use needle::spawn_version::parse_backend_name;
/// assert_eq!(parse_backend_name("bf 0.1.0"), "bf");
/// assert_eq!(parse_backend_name("bead 2.0.5"), "bead-rs");
/// assert_eq!(parse_backend_name("bead-rs 2.0.5"), "bead-rs");
/// assert_eq!(parse_backend_name("unknown format"), "unknown");
/// ```
///
/// # Notes
///
/// - This is parsing only - no validation or error handling yet
/// - Version numbers are stripped - only the backend name is returned
/// - Handles whitespace and newlines gracefully
/// - "bead" output is mapped to "bead-rs" for consistency
pub fn parse_backend_name(version_output: &str) -> String {
    use regex::Regex;

    let output = version_output.trim();

    // Try to extract the first word (backend name) from version output
    // Common formats: "bf 0.1.0", "bead 2.0.5", "bead-rs 2.0.5"
    let first_word = output.split_whitespace().next().unwrap_or("unknown");

    match first_word {
        "bead" => "bead-rs".to_string(), // bead CLI is bead-rs backend
        "bf" => "bf".to_string(),
        "bead-rs" => "bead-rs".to_string(),
        backend => {
            // For unknown backends, validate it looks like a name (alphanumeric + hyphens/underscores)
            // Must start with a letter, followed by letters, digits, hyphens, or underscores only
            let name_regex = Regex::new(r"^[a-zA-Z][a-zA-Z0-9_-]*$").unwrap();
            if name_regex.is_match(backend) && !backend.contains('@') && !backend.contains('.') {
                backend.to_string()
            } else {
                "unknown".to_string()
            }
        }
    }
}

/// Spawn a binary with `--version` flag and capture its stdout.
///
/// This is a low-level spawning function that executes a binary with the
/// `--version` argument and returns its raw stdout output as a String.
///
/// # Arguments
///
/// * `binary_path` - Path to the binary to execute
///
/// # Returns
///
/// * `Ok(String)` - Raw stdout output from the binary
/// * `Err(e)` - Spawn failure, execution error, or non-zero exit code
///
/// # Errors
///
/// This function returns an error if:
/// - The binary does not exist at the specified path
/// - The binary fails to spawn (OS-level execution failure)
/// - The binary exits with a non-zero exit code
/// - Output cannot be decoded as UTF-8
///
/// # Examples
///
/// ```no_run
/// # use needle::spawn_version::spawn_version_output;
/// # use std::path::Path;
/// # fn main() -> anyhow::Result<()> {
/// let binary = Path::new("/usr/local/bin/bead");
/// let output = spawn_version_output(binary)?;
/// println!("Raw output: {}", output);
/// # Ok(())
/// # }
/// ```
///
/// # Notes
///
/// - No parsing is performed on the output - this is raw stdout capture only
/// - No timeout is applied - use a timeout wrapper for long-running binaries
/// - Stderr is captured but only included in error messages on failure
/// - Exit code checking is performed - non-zero exits return an error
pub fn spawn_version_output(binary_path: &Path) -> Result<String> {
    // Check if binary exists before attempting to spawn
    if !binary_path.exists() {
        anyhow::bail!("binary not found at path: {}", binary_path.display());
    }

    debug!(
        binary = %binary_path.display(),
        "spawn_version: executing binary with --version"
    );

    // Spawn the binary with --version flag
    let output = Command::new(binary_path)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| {
            format!(
                "failed to execute binary '{}' with --version",
                binary_path.display()
            )
        })?;

    // Check exit status - non-zero exits are errors
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "binary '{}' --version exited with code {}: {}",
            binary_path.display(),
            output.status.code().unwrap_or(-1),
            stderr.trim()
        );
    }

    // Decode stdout as UTF-8
    let stdout = String::from_utf8(output.stdout).with_context(|| {
        format!(
            "binary '{}' --version produced non-UTF-8 output",
            binary_path.display()
        )
    })?;

    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// Create a fake executable binary in a temp directory.
    fn create_fake_binary(tmp_dir: &Path, name: &str, content: &str) -> PathBuf {
        let binary_path = tmp_dir.join(name);
        fs::write(&binary_path, content).expect("failed to write fake binary");

        let mut perms = fs::metadata(&binary_path)
            .expect("failed to get metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&binary_path, perms).expect("failed to set permissions");

        binary_path
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // parse_backend_name tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_backend_name_handles_bf_format() {
        // Test basic bf version format
        assert_eq!(parse_backend_name("bf 0.1.0"), "bf");
        assert_eq!(parse_backend_name("bf 1.2.3"), "bf");
        assert_eq!(parse_backend_name("bf 2.0.5-beta"), "bf");
    }

    #[test]
    fn parse_backend_name_handles_bead_format() {
        // Test bead format (should map to bead-rs)
        assert_eq!(parse_backend_name("bead 0.1.0"), "bead-rs");
        assert_eq!(parse_backend_name("bead 2.0.5"), "bead-rs");
        assert_eq!(parse_backend_name("bead 1.0.0-alpha"), "bead-rs");
    }

    #[test]
    fn parse_backend_name_handles_bead_rs_format() {
        // Test explicit bead-rs format
        assert_eq!(parse_backend_name("bead-rs 2.0.5"), "bead-rs");
        assert_eq!(parse_backend_name("bead-rs 1.0.0"), "bead-rs");
        assert_eq!(parse_backend_name("bead-rs 0.1.0-beta"), "bead-rs");
    }

    #[test]
    fn parse_backend_name_handles_multiline_output() {
        // Test parsing from multiline version output
        let multiline = "bead 2.0.5\nBuild metadata: some info\nCopyright 2026";
        assert_eq!(parse_backend_name(multiline), "bead-rs");

        let bf_multiline = "bf 0.1.0\nAnother line\nYet another line";
        assert_eq!(parse_backend_name(bf_multiline), "bf");
    }

    #[test]
    fn parse_backend_name_handles_whitespace_variations() {
        // Test various whitespace handling
        assert_eq!(parse_backend_name("  bf 0.1.0  "), "bf");
        assert_eq!(parse_backend_name("\nbead 2.0.5\n"), "bead-rs");
        assert_eq!(parse_backend_name("bf\t0.1.0"), "bf");
    }

    #[test]
    fn parse_backend_name_returns_unknown_for_unknown_formats() {
        // Test unknown/malformed formats return "unknown"
        assert_eq!(parse_backend_name("unknown format"), "unknown");
        assert_eq!(parse_backend_name("12345"), "unknown");
        assert_eq!(parse_backend_name(""), "unknown");
        assert_eq!(parse_backend_name("   "), "unknown");
    }

    #[test]
    fn parse_backend_name_handles_custom_backend_names() {
        // Test custom backend names (alphanumeric + hyphens/underscores)
        assert_eq!(parse_backend_name("my-backend 1.0.0"), "my-backend");
        assert_eq!(parse_backend_name("custom_backend 2.0.0"), "custom_backend");
        assert_eq!(
            parse_backend_name("my-custom_backend 1.0"),
            "my-custom_backend"
        );
    }

    #[test]
    fn parse_backend_name_rejects_invalid_backend_names() {
        // Test invalid backend names return "unknown"
        assert_eq!(parse_backend_name("123invalid 1.0.0"), "unknown");
        assert_eq!(parse_backend_name("-starts-with-dash 1.0.0"), "unknown");
        assert_eq!(
            parse_backend_name("_starts-with-underscore 1.0.0"),
            "unknown"
        );
        assert_eq!(parse_backend_name("has@symbol 1.0.0"), "unknown");
        // "has space" is split by whitespace, so "has" is the first word and is valid
        assert_eq!(parse_backend_name("has space 1.0.0"), "has");
    }

    #[test]
    fn parse_backend_name_returns_string_type() {
        // Verify the function returns String type (not &str)
        let result: String = parse_backend_name("bf 1.0.0");
        assert_eq!(result, "bf");
        assert!(result == "bf");
    }

    #[test]
    fn parse_backend_name_strips_version_numbers() {
        // Verify version numbers are not included in output
        assert_eq!(parse_backend_name("bf 0.1.0"), "bf");
        assert_eq!(parse_backend_name("bead 2.0.5-beta"), "bead-rs");
        assert_eq!(parse_backend_name("bead-rs 1.2.3-rc1"), "bead-rs");
    }

    #[test]
    fn spawn_version_output_captures_stdout() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let fake_binary = create_fake_binary(
            tmp_dir.path(),
            "fake-binary",
            r#"#!/bin/sh
echo "fake-binary 1.0.0"
"#,
        );

        let output =
            spawn_version_output(&fake_binary).expect("should successfully capture stdout");

        assert_eq!(output.trim(), "fake-binary 1.0.0");
    }

    #[test]
    fn spawn_version_output_handles_nonexistent_binary() {
        let nonexistent = PathBuf::from("/nonexistent/path/to/binary");
        let result = spawn_version_output(&nonexistent);

        assert!(result.is_err(), "should fail for nonexistent binary");
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("binary not found") || error_msg.contains("not found"));
    }

    #[test]
    fn spawn_version_output_handles_nonzero_exit_code() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let failing_binary = create_fake_binary(
            tmp_dir.path(),
            "failing-binary",
            r#"#!/bin/sh
echo "Error: something went wrong" >&2
exit 1
"#,
        );

        let result = spawn_version_output(&failing_binary);

        assert!(
            result.is_err(),
            "should fail when binary exits with non-zero code"
        );
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("exited with code"));
    }

    #[test]
    fn spawn_version_output_handles_empty_output() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let empty_binary = create_fake_binary(
            tmp_dir.path(),
            "empty-binary",
            r#"#!/bin/sh
# Output nothing
"#,
        );

        let output =
            spawn_version_output(&empty_binary).expect("should successfully capture empty stdout");

        assert_eq!(output.trim(), "");
    }

    #[test]
    fn spawn_version_output_handles_multiline_output() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let multiline_binary = create_fake_binary(
            tmp_dir.path(),
            "multiline-binary",
            r#"#!/bin/sh
echo "my-tool 2.0.0"
echo "Build metadata: some info"
echo "Copyright 2026"
"#,
        );

        let output = spawn_version_output(&multiline_binary)
            .expect("should successfully capture multiline stdout");

        assert!(output.contains("my-tool 2.0.0"));
        assert!(output.contains("Build metadata"));
        assert!(output.contains("Copyright"));
    }

    #[test]
    fn spawn_version_output_preserves_raw_output() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let raw_binary = create_fake_binary(
            tmp_dir.path(),
            "raw-binary",
            r#"#!/bin/sh
echo "  tool-with-spacing   1.2.3  "
"#,
        );

        let output = spawn_version_output(&raw_binary)
            .expect("should successfully capture stdout with original spacing");

        // Output should be raw, not trimmed
        assert!(output.contains("  tool-with-spacing   1.2.3  "));
    }

    #[test]
    fn spawn_version_output_returns_string_type() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let fake_binary = create_fake_binary(
            tmp_dir.path(),
            "fake-binary",
            r#"#!/bin/sh
echo "test output"
"#,
        );

        let output: String = spawn_version_output(&fake_binary).expect("should return String type");

        assert_eq!(output.trim(), "test output");
    }

    #[test]
    fn spawn_version_output_basic_spawn_infrastructure() {
        // This test verifies the basic spawning infrastructure works
        let tmp_dir = tempfile::tempdir().unwrap();
        let basic_binary = create_fake_binary(
            tmp_dir.path(),
            "basic-binary",
            r#"#!/bin/sh
echo "basic 1.0"
"#,
        );

        let result = spawn_version_output(&basic_binary);

        // Just verify the spawn succeeds - detailed checks are in other tests
        assert!(result.is_ok(), "basic spawn should succeed");
    }
}
