//! Edge case tests for binary freshness detection.
//!
//! These tests cover unusual scenarios and error conditions that may
//! occur in production but are not part of the happy path.

use std::fs;
use std::time::{Duration, Instant};

use needle::build_metadata::BuildMetadata;
use needle::supervisor::{BinaryFreshnessChecker, FreshnessCheck};
use tempfile::TempDir;

/// Test edge case: Binary file is replaced with a directory (not a file).
#[test]
fn test_binary_replaced_with_directory() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-test");

    // Create initial binary
    fs::write(&binary_path, b"v1").expect("failed to write binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);

    // First check succeeds
    let result = checker.poll().expect("first check failed");
    assert!(result.is_some());

    // Replace binary with a directory
    fs::remove_file(&binary_path).expect("failed to remove binary");
    fs::create_dir(&binary_path).expect("failed to create directory");

    // Next check should handle this gracefully (return CheckFailed or BinaryMissing)
    let result = checker
        .poll_at(Instant::now() + Duration::from_secs(2))
        .expect("second check failed");
    assert!(result.is_some());

    match result.unwrap() {
        FreshnessCheck::CheckFailed { .. } | FreshnessCheck::BinaryMissing { .. } => {
            // Expected: checker handles directory gracefully
        }
        other => panic!(
            "expected CheckFailed or BinaryMissing for directory, got {:?}",
            other
        ),
    }
}

/// Test edge case: Binary file becomes unreadable (permission denied).
#[test]
#[cfg(unix)]
fn test_binary_becomes_unreadable() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-test");

    // Create initial binary
    fs::write(&binary_path, b"v1").expect("failed to write binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let now = Instant::now();

    // First check succeeds
    let result = checker.poll_at(now).expect("first check failed");
    assert!(result.is_some());

    // Make file unreadable (chmod 000)
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&binary_path)
        .expect("failed to get metadata")
        .permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&binary_path, perms).expect("failed to set permissions");

    // Next check should handle permission error gracefully
    let result = checker
        .poll_at(now + Duration::from_secs(2))
        .expect("second check failed");
    assert!(result.is_some());

    match result.unwrap() {
        FreshnessCheck::CheckFailed { error, .. } => {
            assert!(
                error.contains("permission denied") || error.contains("failed to read"),
                "error should mention permission or read failure, got: {}",
                error
            );
        }
        other => panic!("expected CheckFailed for unreadable file, got {:?}", other),
    }
}

/// Test edge case: Binary is replaced mid-read (race condition).
#[test]
fn test_binary_replaced_during_hash_computation() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-race");

    // Create initial binary
    fs::write(&binary_path, vec![b'A'; 10_000]).expect("failed to write binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let now = Instant::now();

    // First check
    let result = checker.poll_at(now).expect("first check failed");
    assert!(result.is_some());

    // Replace binary immediately after check
    fs::write(&binary_path, vec![b'B'; 10_000]).expect("failed to update binary");

    // Next check should detect change normally (no race condition)
    let result = checker
        .poll_at(now + Duration::from_secs(2))
        .expect("second check failed");
    assert!(result.is_some());

    match result.unwrap() {
        FreshnessCheck::NewBinary { .. } => {
            // Expected: detected the change
        }
        other => panic!("expected NewBinary after replacement, got {:?}", other),
    }
}

/// Test edge case: Very large binary (10MB+) to ensure hash computation is efficient.
#[test]
fn test_large_binary_hash_performance() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-large");

    // Create a 10MB binary
    let large_binary = vec![0xAB_u8; 10 * 1024 * 1024];
    fs::write(&binary_path, large_binary).expect("failed to write large binary");

    let start = Instant::now();

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let result = checker.poll().expect("large binary check failed");

    let elapsed = start.elapsed();

    assert!(result.is_some(), "should check large binary");
    assert!(
        elapsed < Duration::from_secs(5),
        "hash computation should be fast (took {:?})",
        elapsed
    );
}

/// Test edge case: Empty binary file.
#[test]
fn test_empty_binary_file() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-empty");

    // Create empty file
    fs::write(&binary_path, b"").expect("failed to write empty file");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);

    // Should handle empty file gracefully
    let result = checker.poll().expect("empty file check failed");
    assert!(result.is_some());

    match result.unwrap() {
        FreshnessCheck::Unchanged { current_hash, .. } => {
            // Empty file has a valid SHA256 hash (all zeros)
            assert_eq!(
                current_hash,
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            );
        }
        other => panic!("expected Unchanged for empty file, got {:?}", other),
    }
}

/// Test edge case: Binary with special characters in path.
#[test]
fn test_binary_path_with_special_characters() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");

    // Test with space in path
    let binary_path = temp_dir.path().join("needle test binary");
    fs::write(&binary_path, b"v1").expect("failed to write binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);

    // Should handle spaces in path
    let result = checker.poll().expect("check with space failed");
    assert!(result.is_some());

    // Test with unicode characters
    let unicode_path = temp_dir.path().join("needle-日本語");
    fs::write(&unicode_path, b"v1").expect("failed to write unicode binary");

    let mut checker = BinaryFreshnessChecker::new(unicode_path, 1);
    let result = checker.poll().expect("check with unicode failed");
    assert!(result.is_some());
}

/// Test edge case: Symlink to binary.
#[test]
#[cfg(unix)]
fn test_symlink_to_binary() {
    use std::os::unix::fs::symlink;

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let real_binary = temp_dir.path().join("needle-real");
    let symlink_path = temp_dir.path().join("needle-symlink");

    // Create real binary
    fs::write(&real_binary, b"v1").expect("failed to write real binary");

    // Create symlink
    symlink(&real_binary, &symlink_path).expect("failed to create symlink");

    let mut checker = BinaryFreshnessChecker::new(symlink_path.clone(), 1);
    let now = Instant::now();

    // Should follow symlink and hash the real file
    let result = checker.poll_at(now).expect("symlink check failed");
    assert!(result.is_some());

    let v1_hash = match result.unwrap() {
        FreshnessCheck::Unchanged { current_hash, .. } => current_hash,
        other => panic!("expected Unchanged, got {:?}", other),
    };

    // Update real binary
    fs::write(&real_binary, b"v2").expect("failed to update real binary");

    // Should detect change through symlink
    let result = checker
        .poll_at(now + Duration::from_secs(2))
        .expect("symlink update check failed");
    assert!(result.is_some());

    match result.unwrap() {
        FreshnessCheck::NewBinary {
            old_hash, new_hash, ..
        } => {
            assert_eq!(old_hash, v1_hash);
            assert_ne!(new_hash, v1_hash);
        }
        other => panic!("expected NewBinary through symlink, got {:?}", other),
    }
}

/// Test edge case: Build metadata from corrupt binary.
#[test]
fn test_build_metadata_from_corrupt_binary() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-corrupt");

    // Write random garbage (not a valid binary)
    let garbage = vec![0xFF; 1000];
    fs::write(&binary_path, garbage).expect("failed to write corrupt binary");

    // Should not panic, should either fail gracefully or return fallback metadata
    let result = BuildMetadata::from_binary(&binary_path);

    match result {
        Ok(metadata) => {
            // Should return fallback metadata with "unknown" values
            assert_eq!(metadata.commit_sha, "unknown");
            assert_eq!(metadata.build_timestamp, "unknown");
        }
        Err(_) => {
            // Also acceptable: return error
        }
    }
}

/// Test edge case: Binary file truncated to zero size during operation.
#[test]
fn test_binary_truncated_to_zero() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-truncate");

    // Create initial binary
    fs::write(&binary_path, b"v1 content").expect("failed to write binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let now = Instant::now();

    // First check
    let result = checker.poll_at(now).expect("first check failed");
    assert!(result.is_some());

    let v1_hash = match result.unwrap() {
        FreshnessCheck::Unchanged { current_hash, .. } => current_hash,
        other => panic!("expected Unchanged, got {:?}", other),
    };

    // Truncate to zero (simulate corruption)
    fs::write(&binary_path, b"").expect("failed to truncate");

    // Should detect change
    let result = checker
        .poll_at(now + Duration::from_secs(2))
        .expect("truncated check failed");
    assert!(result.is_some());

    match result.unwrap() {
        FreshnessCheck::NewBinary {
            old_hash, new_hash, ..
        } => {
            assert_eq!(old_hash, v1_hash);
            assert_ne!(new_hash, v1_hash);
            // Empty file hash
            assert_eq!(
                new_hash,
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            );
        }
        other => panic!("expected NewBinary for truncated file, got {:?}", other),
    }
}

/// Test edge case: Rapid successive changes (multiple deployments in quick succession).
#[test]
fn test_rapid_successive_binary_changes() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-rapid");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let now = Instant::now();

    // Version sequence: v1 -> v2 -> v3 -> v4 in quick succession
    let versions = [b"v1", b"v2", b"v3", b"v4"];

    for (i, version) in versions.iter().enumerate() {
        fs::write(&binary_path, *version).expect("failed to write version");

        let check_time = now + Duration::from_secs(i as u64);
        let result = checker
            .poll_at(check_time)
            .unwrap_or_else(|_| panic!("check {} failed", i));

        assert!(result.is_some());

        match result.unwrap() {
            FreshnessCheck::Unchanged { .. } if i == 0 => {
                // First version - initial recording
            }
            FreshnessCheck::NewBinary { .. } if i > 0 => {
                // Subsequent versions should detect changes
            }
            other => panic!("unexpected result for version {}: {:?}", i, other),
        }
    }
}

/// Test edge case: Multiple checkers monitoring same binary (shared resource).
#[test]
fn test_multiple_checkers_same_binary() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-shared");

    fs::write(&binary_path, b"v1").expect("failed to write binary");

    // Create multiple checkers
    let mut checker1 = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let mut checker2 = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let mut checker3 = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let now = Instant::now();

    // All checkers should record initial hash
    for (i, checker) in [&mut checker1, &mut checker2, &mut checker3]
        .iter_mut()
        .enumerate()
    {
        let result = checker
            .poll_at(now)
            .unwrap_or_else(|_| panic!("checker {} first check failed", i));
        assert!(result.is_some());
    }

    // Update binary
    fs::write(&binary_path, b"v2").expect("failed to update binary");

    // All checkers should detect change independently
    for (i, checker) in [&mut checker1, &mut checker2, &mut checker3]
        .iter_mut()
        .enumerate()
    {
        let result = checker
            .poll_at(now + Duration::from_secs(2))
            .unwrap_or_else(|_| panic!("checker {} second check failed", i));
        assert!(result.is_some());

        match result.unwrap() {
            FreshnessCheck::NewBinary { .. } => {
                // Expected
            }
            other => panic!("checker {} expected NewBinary, got {:?}", i, other),
        }
    }
}

/// Test edge case: Checker with very long interval (hours).
#[test]
fn test_checker_with_very_long_interval() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-long-interval");

    fs::write(&binary_path, b"v1").expect("failed to write binary");

    // Create checker with 1-hour interval
    let mut checker = BinaryFreshnessChecker::new(binary_path, 3600);
    let now = Instant::now();

    // First check should execute
    let result = checker.poll_at(now).expect("first check failed");
    assert!(result.is_some());

    // Checks within the hour should be skipped
    for secs in [60, 300, 1800, 3599] {
        let result = checker
            .poll_at(now + Duration::from_secs(secs))
            .unwrap_or_else(|_| panic!("check at {}s failed", secs));
        assert!(result.is_none(), "check at {}s should be skipped", secs);
    }

    // Check after 1 hour should execute
    let result = checker
        .poll_at(now + Duration::from_secs(3600))
        .expect("hour check failed");
    assert!(result.is_some());
}

/// Test edge case: Binary path is a FIFO pipe (not a regular file).
/// DISABLED: mkfifo is unstable in stable Rust (see issue #139324)
/// This test cannot be enabled until `unix_mkfifo` is stabilized.
/// See: https://github.com/rust-lang/rust/issues/139324
///
/// Test edge case: Zero check interval (should clamp to minimum).
#[test]
fn test_checker_zero_interval_clamps_to_minimum() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-zero-interval");

    fs::write(&binary_path, b"v1").expect("failed to write binary");

    // Create checker with 0 interval (should clamp to 1 second minimum)
    let mut checker = BinaryFreshnessChecker::new(binary_path, 0);
    let now = Instant::now();

    // First check should execute
    let result = checker.poll_at(now).expect("first check failed");
    assert!(result.is_some());

    // Immediate second check should be skipped (clamped to minimum interval)
    let result = checker.poll_at(now).expect("immediate check failed");
    assert!(
        result.is_none(),
        "should be rate-limited even with 0 interval"
    );
}
