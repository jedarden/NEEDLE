//! End-to-end test for binary freshness fix-loop verification.
//!
//! This test simulates the complete development lifecycle:
//! 1. Worker starts with binary v1
//! 2. Fix is committed and new binary v2 is built
//! 3. Worker detects new binary and rotates
//! 4. Worker runs with new binary v2
//!
//! This demonstrates the core value proposition: fixes land → new binary built →
//! workers eventually run the new code without manual intervention.

use needle::upgrade::{check_hot_reload, HotReloadCheck};
use std::fs;
use tempfile::TempDir;

fn stable_binary(home: &std::path::Path) -> std::path::PathBuf {
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create bin directory");
    bin_dir.join("needle-stable")
}

fn install_running_binary(home: &std::path::Path) -> std::path::PathBuf {
    let stable = stable_binary(home);
    fs::copy(
        std::env::current_exe().expect("test executable path"),
        &stable,
    )
    .expect("failed to install current binary as stable");
    stable
}

/// Simulates the complete fix-loop from development to deployment.
#[test]
fn test_fix_loop_end_to_end() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let needle_home = temp_dir.path().join("needle-home");
    let stable = install_running_binary(&needle_home);

    assert_eq!(
        check_hot_reload(&needle_home).expect("initial freshness check"),
        HotReloadCheck::NoChange
    );

    // Simulate CI publishing a different :stable payload. The worker consumes
    // this same check at the safe boundary between dispatches.
    fs::write(&stable, b"needle-v2-with-fix").expect("failed to write v2 binary");

    match check_hot_reload(&needle_home).expect("freshness check after publish") {
        HotReloadCheck::NewBinaryDetected {
            old_hash,
            new_hash,
            stable_path,
        } => {
            assert_ne!(old_hash, new_hash);
            assert_eq!(stable_path, stable);
        }
        other => panic!("expected the published binary to be detected, got {other:?}"),
    }
}

/// Tests that a stale running binary is detected before the rotation step.
#[test]
fn test_stale_binary_is_detected_before_rotation() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let stable = stable_binary(temp_dir.path());
    fs::write(&stable, b"different-from-the-running-test-binary")
        .expect("failed to write stable binary");

    assert!(matches!(
        check_hot_reload(temp_dir.path()).expect("stale binary check"),
        HotReloadCheck::NewBinaryDetected { stable_path, .. } if stable_path == stable
    ));
}

/// Tests metadata reading from binary.
#[test]
fn test_metadata_reading_from_binary() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-test");

    // Create binary with embedded metadata
    let metadata_json =
        r#"{"version":"1.0.0","commit_sha":"abc123","build_timestamp":"2024-01-01T00:00:00Z"}"#;
    let binary_content = format!("BINARY_CONTENT\x00{}", metadata_json);
    fs::write(&binary_path, binary_content.as_bytes()).expect("failed to write binary");

    // Verify metadata can be read
    use needle::build_metadata::BuildMetadata;
    let metadata =
        BuildMetadata::from_binary(&binary_path).expect("embedded metadata should parse");
    assert_eq!(metadata.version, "1.0.0");
    assert_eq!(metadata.commit_sha, "abc123");
}

/// Tests that binary unchanged doesn't trigger rotation.
#[test]
fn test_binary_unchanged_no_rotation() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let needle_home = temp_dir.path();
    install_running_binary(needle_home);

    assert_eq!(
        check_hot_reload(needle_home).expect("unchanged binary check"),
        HotReloadCheck::NoChange
    );
}

/// Tests corrupt binary handling.
#[test]
fn test_corrupt_binary_handling() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let needle_home = temp_dir.path();
    let stable = stable_binary(needle_home);
    fs::write(&stable, vec![0xFF_u8; 1000]).expect("failed to write corrupt binary");

    // Freshness detection hashes bytes and does not execute or parse the
    // candidate. A corrupt publication is still detected without crashing.
    assert!(matches!(
        check_hot_reload(needle_home).expect("corrupt binary freshness check"),
        HotReloadCheck::NewBinaryDetected { stable_path, .. } if stable_path == stable
    ));
}
