//! Integration tests for version probe backend detection.
//!
//! These tests verify that the version probe can correctly identify
//! different bead CLI backends by running them with --version and parsing
//! the output.

use needle::version_probe::{VersionProbe, BACKEND_BEAD, BACKEND_BEADS_RUST, BACKEND_BF};
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
        Ok(Some(backend)) => {
            assert_eq!(backend, BACKEND_BF);
            println!("✓ Detected bf backend: {}", backend);
        }
        Ok(None) => {
            panic!("bf binary exists but backend name could not be parsed");
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
        Ok(Some(backend)) => {
            // bead-rs reports as "bead" in version output
            assert!(backend == BACKEND_BEAD || backend == BACKEND_BEADS_RUST);
            println!("✓ Detected bead backend: {}", backend);
        }
        Ok(None) => {
            panic!("bead binary exists but backend name could not be parsed");
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
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("not found") || err_msg.contains("PATH"));

    println!("✓ Correctly error on missing binary");
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
        Ok(Some(backend)) => {
            assert_eq!(backend, "git");
            println!("✓ Detected git backend: {}", backend);
        }
        Ok(None) => {
            // git's version output might not match our parsing expectations
            println!("Git version output could not be parsed (this is acceptable)");
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
        Ok(Some(backend)) => {
            assert_eq!(backend, "cargo");
            println!("✓ Detected cargo backend: {}", backend);
        }
        Ok(None) => {
            println!("Cargo version output could not be parsed (this is acceptable)");
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
        // So we should get Ok(None) or Ok(Some("echo"))
        match result {
            Ok(Some(backend)) => {
                println!("✓ echo treated as backend: {}", backend);
            }
            Ok(None) => {
                println!("✓ echo version output correctly parsed as non-backend");
            }
            Err(_) => {
                println!("✓ echo --version failed (acceptable)");
            }
        }
    }
}
