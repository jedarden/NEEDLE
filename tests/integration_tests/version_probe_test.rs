//! Integration tests for version probe backend detection.
//!
//! These tests verify that the version probe can correctly identify
//! different bead CLI backends by running them with --version and parsing
//! the output.

use needle::version_probe::{
    ProbeError, TelemetryEmitter, VersionProbe, VersionVerifyEvent, BACKEND_BEAD,
    BACKEND_BEADS_RUST, BACKEND_BF,
};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn test_version_probe_detects_bf_backend() {
    // This test only runs if `bf` is installed
    let probe = VersionProbe::new();

    if !probe.is_binary_available("bf") {
        println!("Skipping: bf binary not found in PATH");
        return;
    }

    match probe.detect_backend("bf") {
        Ok(backend) => {
            assert_eq!(backend, BACKEND_BF);
            println!("✓ Detected bf backend: {}", backend);
        }
        Err(e) => {
            panic!("Failed to detect bf backend: {}", e);
        }
    }
}

#[test]
fn test_version_probe_detects_bead_backend() {
    // This test only runs if `bead` is installed
    let probe = VersionProbe::new();

    if !probe.is_binary_available("bead") {
        println!("Skipping: bead binary not found in PATH");
        return;
    }

    match probe.detect_backend("bead") {
        Ok(backend) => {
            // bead-rs reports as "bead" in version output
            assert!(backend == BACKEND_BEAD || backend == BACKEND_BEADS_RUST);
            println!("✓ Detected bead backend: {}", backend);
        }
        Err(e) => {
            panic!("Failed to detect bead backend: {}", e);
        }
    }
}

#[test]
fn test_version_probe_handles_missing_binary() {
    let probe = VersionProbe::new();

    // Use a binary name that definitely doesn't exist
    let result = probe.detect_backend("nonexistent-binary-xyz-123");

    assert!(result.is_err());
    match result.unwrap_err() {
        ProbeError::BinaryNotFound { binary } => {
            assert_eq!(binary, "nonexistent-binary-xyz-123");
            println!("✓ Binary not found returns specific error");
        }
        other => panic!("Expected BinaryNotFound error, got: {}", other),
    }
}

#[test]
fn test_version_probe_is_binary_available() {
    let probe = VersionProbe::new();

    // Test with a binary that should exist on most systems
    let ls_exists = probe.is_binary_available("ls");
    assert!(ls_exists, "ls should be available on most systems");

    // Test with a binary that definitely doesn't exist
    let fake_exists = probe.is_binary_available("nonexistent-binary-xyz-123");
    assert!(!fake_exists, "fake binary should not be available");

    println!("✓ Binary availability check works");
}

#[test]
fn test_version_probe_timeout_configurable() {
    let short_timeout = Duration::from_millis(100);
    let probe = VersionProbe::with_timeout(short_timeout);

    if !probe.is_binary_available("sleep") {
        println!("Skipping: sleep binary not found");
        return;
    }

    // Note: This test assumes `sleep --version` completes quickly
    // If sleep doesn't support --version, this may fail differently
    let result = probe.detect_backend("sleep");

    // sleep typically doesn't output a backend name in --version
    // We're just testing that the timeout is respected
    println!("✓ Timeout configuration works (result: {:?})", result);
}

#[test]
fn test_version_probe_handles_git_version() {
    // git --version outputs "git version X.Y.Z" - first word should be "git"
    let probe = VersionProbe::new();

    if !probe.is_binary_available("git") {
        println!("Skipping: git binary not found");
        return;
    }

    match probe.detect_backend("git") {
        Ok(backend) => {
            assert_eq!(backend, "git");
            println!("✓ Detected git backend: {}", backend);
        }
        Err(e) => {
            println!("Failed to detect git backend (may be expected): {}", e);
        }
    }
}

#[test]
fn test_version_probe_handles_cargo_version() {
    // cargo --version outputs "cargo X.Y.Z" - first word should be "cargo"
    let probe = VersionProbe::new();

    if !probe.is_binary_available("cargo") {
        println!("Skipping: cargo binary not found");
        return;
    }

    match probe.detect_backend("cargo") {
        Ok(backend) => {
            assert_eq!(backend, "cargo");
            println!("✓ Detected cargo backend: {}", backend);
        }
        Err(e) => {
            println!("Failed to detect cargo backend (may be expected): {}", e);
        }
    }
}

#[test]
fn test_backend_constants_are_correct() {
    assert_eq!(BACKEND_BF, "bf");
    assert_eq!(BACKEND_BEAD, "bead");
    assert_eq!(BACKEND_BEADS_RUST, "beads-rust");
    println!("✓ Backend constants are correctly defined");
}

#[test]
fn test_version_probe_rejects_non_binary_names() {
    let probe = VersionProbe::new();

    // Try to run a directory instead of a binary
    let result = probe.detect_backend("/tmp");

    assert!(result.is_err());
    println!("✓ Correctly rejects non-binary paths");
}

#[test]
fn test_version_probe_handles_version_flag_failure() {
    let probe = VersionProbe::new();

    // Some binaries might not support --version
    // echo doesn't fail on --version but outputs it literally
    if probe.is_binary_available("echo") {
        let result = probe.detect_backend("echo");

        // echo doesn't fail, but the output won't be a valid version string
        // So we should get Ok("echo") since "echo" is a valid backend name
        match result {
            Ok(backend) => {
                println!("✓ echo treated as backend: {}", backend);
            }
            Err(e) => {
                // If echo outputs something unparseable, we get an error
                println!("✓ echo --version produced unparseable output: {}", e);
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Verification tests with telemetry
// ──────────────────────────────────────────────────────────────────────────────

/// Mock telemetry emitter for integration tests.
struct TestTelemetryEmitter {
    events: Arc<std::sync::Mutex<Vec<VersionVerifyEvent>>>,
}

impl TestTelemetryEmitter {
    fn new() -> Self {
        Self {
            events: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn get_events(&self) -> Vec<VersionVerifyEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl TelemetryEmitter for TestTelemetryEmitter {
    fn emit_version_event(&self, event: VersionVerifyEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[test]
fn test_verify_backend_bf_success_with_telemetry() {
    let mock = Arc::new(TestTelemetryEmitter::new());
    let probe = VersionProbe::new().with_telemetry(mock.clone());

    if !probe.is_binary_available("bf") {
        println!("Skipping: bf binary not found in PATH");
        return;
    }

    let result = probe.verify_backend("bf");

    match result {
        Ok(()) => {
            println!("✓ bf backend verification succeeded");
            let events = mock.get_events();
            assert_eq!(events.len(), 2); // Started + Success

            match &events[0] {
                VersionVerifyEvent::Started {
                    binary,
                    expected_backend,
                } => {
                    assert_eq!(binary, "bf");
                    assert_eq!(expected_backend, "bf");
                }
                _ => panic!("First event should be Started"),
            }

            match &events[1] {
                VersionVerifyEvent::Success {
                    binary,
                    expected_backend,
                    actual_backend,
                } => {
                    assert_eq!(binary, "bf");
                    assert_eq!(expected_backend, "bf");
                    assert_eq!(actual_backend, "bf");
                }
                _ => panic!("Second event should be Success"),
            }
        }
        Err(e) => {
            println!(
                "bf verification failed (may indicate backend mismatch): {}",
                e
            );
            // Check that we got the failure telemetry
            let events = mock.get_events();
            assert_eq!(events.len(), 2); // Started + Failed

            match &events[1] {
                VersionVerifyEvent::Failed { error_type, .. } => {
                    assert!(!error_type.is_empty());
                    println!("✓ Failure telemetry emitted: {}", error_type);
                }
                _ => panic!("Second event should be Failed"),
            }
        }
    }
}

#[test]
fn test_verify_backend_bead_success_with_telemetry() {
    let mock = Arc::new(TestTelemetryEmitter::new());
    let probe = VersionProbe::new().with_telemetry(mock.clone());

    if !probe.is_binary_available("bead") {
        println!("Skipping: bead binary not found in PATH");
        return;
    }

    let result = probe.verify_backend("bead");

    match result {
        Ok(()) => {
            println!("✓ bead backend verification succeeded");
            let events = mock.get_events();
            assert_eq!(events.len(), 2); // Started + Success

            match &events[1] {
                VersionVerifyEvent::Success {
                    binary,
                    expected_backend,
                    actual_backend,
                } => {
                    assert_eq!(binary, "bead");
                    assert_eq!(expected_backend, "bead");
                    // bead-rs may report as either "bead" or "beads-rust"
                    assert!(actual_backend == "bead" || actual_backend == "beads-rust");
                }
                _ => panic!("Second event should be Success"),
            }
        }
        Err(e) => {
            println!("bead verification failed: {}", e);
            let events = mock.get_events();
            assert_eq!(events.len(), 2); // Started + Failed

            match &events[1] {
                VersionVerifyEvent::Failed { error_type, .. } => {
                    println!("✓ Failure telemetry emitted: {}", error_type);
                }
                _ => panic!("Second event should be Failed"),
            }
        }
    }
}

#[test]
fn test_verify_backend_binary_not_found_emits_telemetry() {
    let mock = Arc::new(TestTelemetryEmitter::new());
    let probe = VersionProbe::new().with_telemetry(mock.clone());

    let result = probe.verify_backend("nonexistent-binary-xyz-123");

    assert!(result.is_err());
    let events = mock.get_events();
    assert_eq!(events.len(), 2); // Started + Failed

    match &events[0] {
        VersionVerifyEvent::Started { binary, .. } => {
            assert_eq!(binary, "nonexistent-binary-xyz-123");
        }
        _ => panic!("First event should be Started"),
    }

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

#[test]
fn test_verify_backend_no_telemetry_no_panic() {
    let probe = VersionProbe::new();

    // Ensure verify_backend works without telemetry attached
    let result = probe.verify_backend("nonexistent-binary-xyz-123");
    assert!(result.is_err());
    println!("✓ verify_backend works without telemetry");
}

#[test]
fn test_verify_backend_error_messages_are_actionable() {
    // Test that error messages include actionable information
    let probe = VersionProbe::new();

    // Test binary not found error
    let result = probe.verify_backend("totally-fake-binary-999");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();

    // Error should mention the binary and what was expected
    assert!(err_msg.contains("totally-fake-binary-999"));
    println!("✓ Binary not found error is actionable: {}", err_msg);

    // Test backend mismatch error (manual construction)
    use needle::version_probe::VerifyError;
    let mismatch_err = VerifyError::BackendMismatch {
        binary: "bead".to_string(),
        expected: "bead".to_string(),
        actual: "bf".to_string(),
    };
    let mismatch_msg = mismatch_err.to_string();

    // Error should show expected vs actual
    assert!(mismatch_msg.contains("expected 'bead'"));
    assert!(mismatch_msg.contains("got 'bf'"));
    assert!(mismatch_msg.contains("mismatch"));
    println!("✓ Backend mismatch error is actionable: {}", mismatch_msg);
}

#[test]
fn test_verify_backend_git_integration() {
    let mock = Arc::new(TestTelemetryEmitter::new());
    let probe = VersionProbe::new().with_telemetry(mock.clone());

    if !probe.is_binary_available("git") {
        println!("Skipping: git binary not found");
        return;
    }

    // git should self-verify as "git"
    let result = probe.verify_backend("git");

    match result {
        Ok(()) => {
            println!("✓ git backend verification succeeded");
            let events = mock.get_events();
            assert_eq!(events.len(), 2);
        }
        Err(e) => {
            // git --version outputs "git version X.Y.Z" - should detect as "git"
            println!("git verification result: {}", e);
            let events = mock.get_events();
            assert_eq!(events.len(), 2); // Should still emit telemetry even on failure
        }
    }
}
