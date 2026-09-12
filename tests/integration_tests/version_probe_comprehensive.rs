//! Comprehensive integration tests for version probe functionality.
//!
//! These tests use mock binaries to test all error paths and edge cases
//! without requiring real bead CLI installations.

use std::path::PathBuf;
use std::sync::Arc;

use needle::version_probe::{
    ProbeError, TelemetryEmitter, VerifyError, VersionProbe, VersionVerifyEvent, BACKEND_BEAD,
    BACKEND_BEADS_RUST,
};
use std::time::Duration;

// ──────────────────────────────────────────────────────────────────────────────
// Test helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Get the path to the fixtures directory
fn fixtures_dir() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures");
    path
}

/// Create a mock binary path
fn mock_binary(name: &str) -> String {
    let mut path = fixtures_dir();
    path.push(format!("version-{}-mock.sh", name));
    path.to_string_lossy().to_string()
}

/// Create a one-test executable without modifying the repository fixtures.
fn temporary_mock(body: &str) -> (tempfile::TempDir, String) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("version-mock");
    std::fs::write(&path, body).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
    }

    let binary = path.to_string_lossy().into_owned();
    (directory, binary)
}

/// Mock telemetry emitter for testing
struct MockTelemetry {
    events: Arc<std::sync::Mutex<Vec<VersionVerifyEvent>>>,
}

impl MockTelemetry {
    fn new() -> Self {
        Self {
            events: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn get_events(&self) -> Vec<VersionVerifyEvent> {
        self.events.lock().unwrap().clone()
    }

    fn clear(&self) {
        self.events.lock().unwrap().clear();
    }
}

impl TelemetryEmitter for MockTelemetry {
    fn emit_version_event(&self, event: VersionVerifyEvent) {
        self.events.lock().unwrap().push(event);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Success path tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_detect_backend_bead_success() {
    let probe = VersionProbe::new();

    // Test with bead mock
    match probe.detect_backend(&mock_binary("bead")) {
        Ok(backend) => {
            assert!(backend == BACKEND_BEAD || backend == BACKEND_BEADS_RUST);
            println!("✓ Successfully detected bead backend: {}", backend);
        }
        Err(e) => {
            panic!("Failed to detect bead backend: {}", e);
        }
    }
}

#[test]
fn test_detect_backend_beads_rust_success() {
    let probe = VersionProbe::new();

    // Test with beads-rust mock
    match probe.detect_backend(&mock_binary("beads-rust")) {
        Ok(backend) => {
            assert_eq!(backend, BACKEND_BEADS_RUST);
            println!("✓ Successfully detected beads-rust backend");
        }
        Err(e) => {
            panic!("Failed to detect beads-rust backend: {}", e);
        }
    }
}

#[test]
fn test_verify_backend_path_reports_identity_mismatch() {
    let telemetry = Arc::new(MockTelemetry::new());
    let probe = VersionProbe::new().with_telemetry(telemetry.clone());

    let binary = mock_binary("bead");
    match probe.verify_backend(&binary).unwrap_err() {
        VerifyError::BackendMismatch {
            binary: reported_binary,
            expected,
            actual,
        } => {
            assert_eq!(reported_binary, binary);
            assert_eq!(expected, binary);
            assert_eq!(actual, BACKEND_BEAD);
        }
        error => panic!("expected path identity mismatch, got: {error}"),
    }

    let events = telemetry.get_events();
    assert_eq!(events.len(), 2);
    match &events[1] {
        VersionVerifyEvent::Failed {
            expected_backend,
            actual_backend,
            error_type,
            ..
        } => {
            assert_eq!(expected_backend, &binary);
            assert_eq!(actual_backend.as_deref(), Some(BACKEND_BEAD));
            assert_eq!(error_type, "BackendMismatch");
        }
        _ => panic!("Second event should be Failed"),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Error path tests: Binary not found
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_detect_backend_binary_not_found() {
    let probe = VersionProbe::new();

    let result = probe.detect_backend("nonexistent-binary-xyz-123");

    assert!(result.is_err());
    match result.unwrap_err() {
        ProbeError::BinaryNotFound { binary } => {
            assert_eq!(binary, "nonexistent-binary-xyz-123");
            println!("✓ Binary not found returns specific error");
        }
        other => panic!("Expected BinaryNotFound, got: {}", other),
    }
}

#[test]
fn test_verify_backend_binary_not_found() {
    let telemetry = Arc::new(MockTelemetry::new());
    let probe = VersionProbe::new().with_telemetry(telemetry.clone());

    let result = probe.verify_backend("nonexistent-binary-xyz-123");

    assert!(result.is_err());
    let events = telemetry.get_events();
    assert_eq!(events.len(), 2); // Started + Failed

    match &events[1] {
        VersionVerifyEvent::Failed {
            binary,
            error_type,
            actual_backend,
            ..
        } => {
            assert_eq!(binary, "nonexistent-binary-xyz-123");
            assert_eq!(error_type, "BinaryNotFound");
            assert!(actual_backend.is_none());
            println!("✓ Binary not found emits correct telemetry");
        }
        _ => panic!("Second event should be Failed"),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Error path tests: Empty output
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_detect_backend_empty_output() {
    let probe = VersionProbe::new();

    let result = probe.detect_backend(&mock_binary("empty"));

    assert!(result.is_err());
    match result.unwrap_err() {
        ProbeError::UnparseableOutput { binary, output } => {
            assert_eq!(binary, mock_binary("empty"));
            assert!(output.is_empty() || output.trim().is_empty());
            println!("✓ Empty output returns UnparseableOutput error");
        }
        other => panic!("Expected UnparseableOutput, got: {}", other),
    }
}

#[test]
fn test_detect_backend_whitespace_only_output() {
    let probe = VersionProbe::new();
    let (_directory, binary_path) = temporary_mock("#!/bin/sh\nprintf '   \\n'\n");
    let result = probe.detect_backend(&binary_path);

    assert!(result.is_err());
    match result.unwrap_err() {
        ProbeError::UnparseableOutput { .. } => {
            println!("✓ Whitespace-only output returns UnparseableOutput");
        }
        other => panic!("Expected UnparseableOutput, got: {}", other),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Error path tests: Malformed output
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_detect_backend_malformed_output() {
    let probe = VersionProbe::new();

    let result = probe.detect_backend(&mock_binary("malformed"));

    assert!(result.is_err());
    match result.unwrap_err() {
        ProbeError::UnparseableOutput { binary, output } => {
            assert_eq!(binary, mock_binary("malformed"));
            assert_eq!(output, "1.0.0\n"); // Raw output is retained for diagnostics.
            println!("✓ Malformed output (version-only) returns UnparseableOutput");
        }
        other => panic!("Expected UnparseableOutput, got: {}", other),
    }
}

#[test]
fn test_detect_backend_numeric_output() {
    let probe = VersionProbe::new();
    let (_directory, binary_path) = temporary_mock("#!/bin/sh\nprintf '12345\\n'\n");
    let result = probe.detect_backend(&binary_path);

    assert!(result.is_err());
    match result.unwrap_err() {
        ProbeError::UnparseableOutput { .. } => {
            println!("✓ Numeric-only output returns UnparseableOutput");
        }
        other => panic!("Expected UnparseableOutput, got: {}", other),
    }
}

#[test]
fn test_detect_backend_special_characters_output() {
    let probe = VersionProbe::new();
    let (_directory, binary_path) = temporary_mock("#!/bin/sh\nprintf 'bead@1.0.0\\n'\n");
    let result = probe.detect_backend(&binary_path);

    assert!(result.is_err());
    match result.unwrap_err() {
        ProbeError::UnparseableOutput { .. } => {
            println!("✓ Special character output returns UnparseableOutput");
        }
        other => panic!("Expected UnparseableOutput, got: {}", other),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Error path tests: Non-zero exit code
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_detect_backend_non_zero_exit() {
    let probe = VersionProbe::new();

    let result = probe.detect_backend(&mock_binary("bad-exit"));

    assert!(result.is_err());
    match result.unwrap_err() {
        ProbeError::NonZeroExitCode {
            binary,
            code,
            stderr,
        } => {
            assert_eq!(binary, mock_binary("bad-exit"));
            assert_eq!(code, 1);
            assert!(stderr.contains("Error") || stderr.contains("something went wrong"));
            println!("✓ Non-zero exit code returns NonZeroExitCode error");
        }
        other => panic!("Expected NonZeroExitCode, got: {}", other),
    }
}

#[test]
fn test_verify_backend_non_zero_exit() {
    let telemetry = Arc::new(MockTelemetry::new());
    let probe = VersionProbe::new().with_telemetry(telemetry.clone());

    let binary = mock_binary("bad-exit");
    let result = probe.verify_backend(&binary);

    assert!(result.is_err());
    let events = telemetry.get_events();
    assert_eq!(events.len(), 2);

    match &events[1] {
        VersionVerifyEvent::Failed {
            error_type,
            actual_backend,
            ..
        } => {
            assert_eq!(error_type, "NonZeroExitCode");
            assert!(actual_backend.is_none());
            println!("✓ Non-zero exit emits correct telemetry");
        }
        _ => panic!("Second event should be Failed"),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Error path tests: Timeout
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_detect_backend_timeout() {
    let probe = VersionProbe::with_timeout(Duration::from_millis(100));

    let result = probe.detect_backend(&mock_binary("timeout"));

    assert!(result.is_err());
    match result.unwrap_err() {
        ProbeError::Timeout { binary, timeout } => {
            assert_eq!(binary, mock_binary("timeout"));
            assert_eq!(timeout, Duration::from_millis(100));
            println!("✓ Timeout returns Timeout error");
        }
        other => panic!("Expected Timeout, got: {}", other),
    }
}

#[test]
fn test_verify_backend_timeout() {
    let telemetry = Arc::new(MockTelemetry::new());
    let probe =
        VersionProbe::with_timeout(Duration::from_millis(100)).with_telemetry(telemetry.clone());

    let binary = mock_binary("timeout");
    let result = probe.verify_backend(&binary);

    assert!(result.is_err());
    let events = telemetry.get_events();
    assert_eq!(events.len(), 2);

    match &events[1] {
        VersionVerifyEvent::Failed {
            error_type,
            actual_backend,
            ..
        } => {
            assert_eq!(error_type, "Timeout");
            assert!(actual_backend.is_none());
            println!("✓ Timeout emits correct telemetry");
        }
        _ => panic!("Second event should be Failed"),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Edge case tests: Various version output formats
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_detect_backend_multiline_output() {
    let probe = VersionProbe::new();
    let (_directory, binary_path) =
        temporary_mock("#!/bin/sh\nprintf 'bead 0.26.0\\nCopyright 2026\\nMore info here\\n'\n");
    let result = probe.detect_backend(&binary_path);

    match result {
        Ok(backend) => {
            assert_eq!(backend, BACKEND_BEAD);
            println!("✓ Multiline output parses correctly");
        }
        Err(e) => {
            panic!("Failed to parse multiline output: {}", e);
        }
    }
}

#[test]
fn test_detect_backend_with_version_keyword() {
    let probe = VersionProbe::new();
    let (_directory, binary_path) = temporary_mock("#!/bin/sh\nprintf 'bead version 0.26.0\\n'\n");
    let result = probe.detect_backend(&binary_path);

    match result {
        Ok(backend) => {
            assert_eq!(backend, "bead");
            println!("✓ Output with 'version' keyword parses correctly");
        }
        Err(e) => {
            panic!("Failed to parse output with version keyword: {}", e);
        }
    }
}

#[test]
fn test_detect_backend_with_extra_whitespace() {
    let probe = VersionProbe::new();
    let (_directory, binary_path) = temporary_mock("#!/bin/sh\nprintf '  bead   0.26.0  \\n'\n");
    let result = probe.detect_backend(&binary_path);

    match result {
        Ok(backend) => {
            assert_eq!(backend, BACKEND_BEAD);
            println!("✓ Output with extra whitespace parses correctly");
        }
        Err(e) => {
            panic!("Failed to parse output with extra whitespace: {}", e);
        }
    }
}

#[test]
fn test_detect_backend_with_leading_empty_lines() {
    let probe = VersionProbe::new();
    let (_directory, binary_path) = temporary_mock("#!/bin/sh\nprintf '\\n\\nbead 0.26.0\\n'\n");
    let result = probe.detect_backend(&binary_path);

    match result {
        Ok(backend) => {
            assert_eq!(backend, BACKEND_BEAD);
            println!("✓ Output with leading empty lines parses correctly");
        }
        Err(e) => {
            panic!("Failed to parse output with leading empty lines: {}", e);
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Telemetry tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_telemetry_no_panic_without_telemetry() {
    let probe = VersionProbe::new();

    // Should not panic without telemetry
    let result = probe.verify_backend("nonexistent-binary");
    assert!(result.is_err());

    println!("✓ No panic when telemetry not attached");
}

#[test]
fn test_telemetry_includes_error_details() {
    let telemetry = Arc::new(MockTelemetry::new());
    let probe = VersionProbe::new().with_telemetry(telemetry.clone());

    let result = probe.verify_backend("nonexistent-binary");

    assert!(result.is_err());

    let events = telemetry.get_events();
    match &events[1] {
        VersionVerifyEvent::Failed {
            binary,
            error_type,
            error_message,
            ..
        } => {
            assert_eq!(binary, "nonexistent-binary");
            assert_eq!(error_type, "BinaryNotFound");
            assert!(error_message.contains("not found") || error_message.contains("PATH"));
            println!("✓ Telemetry includes detailed error information");
        }
        _ => panic!("Second event should be Failed"),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Is binary available tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_is_binary_available_mock_exists() {
    let probe = VersionProbe::new();

    // Mock binaries should be available
    assert!(
        probe.is_binary_available(&mock_binary("bead")),
        "bead mock should be available"
    );

    println!("✓ Mock binaries detected as available");
}

#[test]
fn test_is_binary_available_not_found() {
    let probe = VersionProbe::new();

    assert!(
        !probe.is_binary_available("nonexistent-binary-xyz-123"),
        "Fake binary should not be available"
    );

    println!("✓ Nonexistent binary correctly reported as unavailable");
}

// ──────────────────────────────────────────────────────────────────────────────
// Expected backend tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_expected_backend_for_binary() {
    let probe = VersionProbe::new();

    assert_eq!(probe.expected_backend_for_binary("bead"), BACKEND_BEAD);
    assert_eq!(probe.expected_backend_for_binary("unknown"), "unknown");

    println!("✓ Expected backend mapping works correctly");
}

// ──────────────────────────────────────────────────────────────────────────────
// Timeout configuration tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_timeout_default() {
    let probe = VersionProbe::new();
    assert_eq!(probe.timeout(), Duration::from_secs(5));
    println!("✓ Default timeout is 5 seconds");
}

#[test]
fn test_timeout_custom() {
    let custom = Duration::from_secs(10);
    let probe = VersionProbe::with_timeout(custom);
    assert_eq!(probe.timeout(), custom);
    println!("✓ Custom timeout configured correctly");
}

// ──────────────────────────────────────────────────────────────────────────────
// Backend constants tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_backend_constants() {
    assert_eq!(BACKEND_BEAD, "bead");
    assert_eq!(BACKEND_BEADS_RUST, "beads-rust");
    println!("✓ Backend constants are correct");
}

// ──────────────────────────────────────────────────────────────────────────────
// Error message quality tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_error_messages_are_actionable() {
    let probe = VersionProbe::new();

    // Binary not found
    let err = probe.detect_backend("totally-fake-binary-999").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("totally-fake-binary-999"));
    assert!(msg.contains("not found") || msg.contains("PATH"));
    println!("Binary not found error: {}", msg);

    // Unparseable output
    let err = probe.detect_backend(&mock_binary("malformed")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("unparseable") || msg.contains("parse"));
    println!("Unparseable output error: {}", msg);

    // Non-zero exit
    let err = probe.detect_backend(&mock_binary("bad-exit")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("exited") || msg.contains("code"));
    println!("Non-zero exit error: {}", msg);

    println!("✓ All error messages are actionable");
}

// ──────────────────────────────────────────────────────────────────────────────
// Real-world scenario tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_real_world_scenario_bead_detection() {
    let probe = VersionProbe::new();

    // Simulate real bead detection
    let binary = mock_binary("bead");
    match probe.detect_backend(&binary) {
        Ok(backend) => {
            assert!(backend == BACKEND_BEAD || backend == BACKEND_BEADS_RUST);
            println!("✓ Real-world bead scenario: detection");
        }
        Err(e) => panic!("Detection failed: {}", e),
    }
}

#[test]
fn test_real_world_scenario_error_recovery() {
    let telemetry = Arc::new(MockTelemetry::new());
    let probe = VersionProbe::new().with_telemetry(telemetry.clone());

    // Try to detect a bad binary, recover, and try a good one
    let bad_binary = mock_binary("bad-exit");
    let good_binary = mock_binary("bead");

    // First attempt fails
    assert!(probe.verify_backend(&bad_binary).is_err());

    // Clear telemetry for next attempt
    telemetry.clear();

    // A later detection on the same probe still succeeds.
    assert_eq!(probe.detect_backend(&good_binary).unwrap(), BACKEND_BEAD);

    println!("✓ Real-world scenario: error recovery works");
}

// ──────────────────────────────────────────────────────────────────────────────
// Cleanup test
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_all_mocks_are_executable() {
    // These are the checked-in fixtures still exercised by this bead-rs
    // suite. Edge-case scripts are isolated in temporary directories.
    for name in [
        "bead",
        "beads-rust",
        "empty",
        "malformed",
        "bad-exit",
        "timeout",
    ] {
        let path = PathBuf::from(mock_binary(name));
        let metadata = std::fs::metadata(&path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode();
            assert!(
                mode & 0o111 != 0,
                "Mock binary {:?} is not executable",
                path
            );
        }
    }

    println!("✓ All mock binaries are executable");
}
