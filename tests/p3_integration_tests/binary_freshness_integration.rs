//! Integration tests for binary freshness detection and worker rotation.
//!
//! These tests verify that the supervisor correctly detects when the worker
//! binary changes and gracefully rotates workers onto the new version.

use std::fs;
use std::time::{Duration, Instant};

use needle::supervisor::{BinaryFreshnessChecker, FreshnessCheck};

#[test]
fn test_binary_freshness_detection_workflow() {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("test-worker");

    // Create initial binary
    fs::write(&binary_path, b"version-1").expect("failed to write initial binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let now = Instant::now();

    // First check should record initial hash
    let result = checker.poll_at(now).expect("first freshness check failed");
    assert!(result.is_some(), "first check should return a result");

    match result.unwrap() {
        FreshnessCheck::Unchanged { current_hash, .. } => {
            assert!(!current_hash.is_empty(), "hash should not be empty");
            assert_eq!(current_hash.len(), 64, "SHA256 hash should be 64 hex chars");
        }
        other => panic!("expected Unchanged on first check, got {:?}", other),
    }

    // Update binary to simulate new deployment
    fs::write(&binary_path, b"version-2").expect("failed to update binary");

    // Second check should detect change
    let result = checker
        .poll_at(now + Duration::from_secs(2))
        .expect("second freshness check failed");
    assert!(result.is_some(), "second check should return a result");

    match result.unwrap() {
        FreshnessCheck::NewBinary {
            old_hash, new_hash, ..
        } => {
            assert!(!old_hash.is_empty(), "old hash should not be empty");
            assert!(!new_hash.is_empty(), "new hash should not be empty");
            assert_ne!(old_hash, new_hash, "hashes should differ");
            assert_eq!(old_hash.len(), 64, "old hash should be 64 hex chars");
            assert_eq!(new_hash.len(), 64, "new hash should be 64 hex chars");
        }
        other => panic!("expected NewBinary on change, got {:?}", other),
    }
}

#[test]
fn test_binary_freshness_rate_limiting() {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("rate-limited-binary");

    fs::write(&binary_path, b"initial").expect("failed to write binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 10);
    let now = Instant::now();

    // First check at t=0 should execute
    let result = checker.poll_at(now).expect("first check failed");
    assert!(result.is_some(), "first check should execute");

    // Immediate checks should be skipped (rate-limited)
    for i in 1..5 {
        let check_time = now + Duration::from_secs(i);
        let result = checker
            .poll_at(check_time)
            .unwrap_or_else(|_| panic!("check {} failed", i));
        assert!(
            result.is_none(),
            "check at {}s should be skipped (rate limit)",
            i
        );
    }

    // Check at interval boundary should execute
    let result = checker
        .poll_at(now + Duration::from_secs(10))
        .expect("interval check failed");
    assert!(
        result.is_some(),
        "check at interval boundary should execute"
    );
}

#[test]
fn test_binary_freshness_missing_binary() {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("nonexistent-binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);

    let result = checker.poll().expect("missing binary check failed");
    assert!(result.is_some(), "check should return a result");

    match result.unwrap() {
        FreshnessCheck::BinaryMissing { binary_path: path } => {
            assert_eq!(path, binary_path, "missing path should match");
        }
        other => panic!("expected BinaryMissing, got {:?}", other),
    }
}

#[test]
fn test_binary_freshness_multiple_changes() {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("multi-change-binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let now = Instant::now();

    // Version sequence: v1 -> v2 -> v3 -> v4
    let versions = ["v1", "v2", "v3", "v4"];

    for (i, version) in versions.iter().enumerate() {
        // Write version
        fs::write(&binary_path, version.as_bytes()).expect("failed to write version");

        // Check should detect change (or record initial for first version)
        let check_time = now + Duration::from_secs(i as u64);
        let result = checker
            .poll_at(check_time)
            .unwrap_or_else(|_| panic!("check for version {} failed", i));

        assert!(result.is_some(), "check should return result");

        match result.unwrap() {
            FreshnessCheck::Unchanged { .. } if i == 0 => {
                // First version records initial hash - expected
            }
            FreshnessCheck::NewBinary { .. } if i > 0 => {
                // Subsequent versions should detect changes
            }
            other => panic!("unexpected result for version {}: {:?}", i, other),
        }
    }
}

#[test]
fn test_binary_freshness_persistence_across_polls() {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("persistent-binary");

    fs::write(&binary_path, b"stable-content").expect("failed to write binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let now = Instant::now();

    // Perform multiple polls with unchanged binary
    let mut first_hash = None;

    for i in 0..5 {
        let check_time = now + Duration::from_secs(i);
        let result = checker
            .poll_at(check_time)
            .unwrap_or_else(|_| panic!("poll {} failed", i));

        assert!(result.is_some(), "poll should return result");

        match result.unwrap() {
            FreshnessCheck::Unchanged { current_hash, .. } => {
                if let Some(ref first) = first_hash {
                    assert_eq!(
                        current_hash, *first,
                        "hash should remain constant across polls"
                    );
                } else {
                    first_hash = Some(current_hash.clone());
                }
            }
            other => panic!("expected Unchanged, got {:?}", other),
        }
    }

    // Verify checker's last_hash is consistent
    assert_eq!(
        checker.last_hash(),
        first_hash.as_deref(),
        "checker state should persist"
    );
}

#[test]
fn test_binary_freshness_large_binary() {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("large-binary");

    // Create a larger binary (1MB of data)
    let large_data = vec![0xAB_u8; 1024 * 1024];
    fs::write(&binary_path, large_data).expect("failed to write large binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);

    let result = checker.poll().expect("large binary check failed");
    assert!(result.is_some(), "check should handle large binaries");

    match result.unwrap() {
        FreshnessCheck::Unchanged { current_hash, .. } => {
            assert!(
                !current_hash.is_empty(),
                "should compute hash for large binary"
            );
            assert_eq!(current_hash.len(), 64, "hash should be correct length");
        }
        other => panic!("expected Unchanged, got {:?}", other),
    }
}
