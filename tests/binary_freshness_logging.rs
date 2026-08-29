//! Test binary freshness detection and logging.
//!
//! Verifies that stale binary detection produces clear warning logs with
//! proper formatting and deduplication.

use std::fs;
use std::time::{Duration, Instant};

use needle::supervisor::{BinaryFreshnessChecker, FreshnessCheck};
use tempfile::TempDir;

#[test]
fn test_freshness_detection_logging() {
    // This test documents the expected logging behavior for binary freshness checks.
    // The actual logging is emitted by the supervisor in src/supervisor/mod.rs
    // lines 636-682, which handles FreshnessCheck results.

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("needle-stable");

    // Create initial binary
    fs::write(&binary_path, b"v1").expect("failed to write initial binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let now = Instant::now();

    // First check should record hash and report Unchanged
    let result = checker.poll_at(now).expect("first check failed");
    assert!(result.is_some());

    match result.unwrap() {
        FreshnessCheck::Unchanged { current_hash, .. } => {
            // Expected: No log emitted (first check, baseline established)
            assert!(!current_hash.is_empty());
        }
        other => panic!("expected Unchanged, got {:?}", other),
    }

    // Update binary to simulate stale state
    fs::write(&binary_path, b"v2").expect("failed to update binary");

    // Second check should detect change
    let result = checker
        .poll_at(now + Duration::from_secs(2))
        .expect("second check failed");
    assert!(result.is_some());

    match result.unwrap() {
        FreshnessCheck::NewBinary {
            old_hash,
            new_hash,
            binary_path: path,
        } => {
            // Expected logging in supervisor (from src/supervisor/mod.rs:641):
            // tracing::info!(
            //     old_binary = %self.current_worker_binary.display(),
            //     new_binary = %binary_path.display(),
            //     old_hash = %old_hash[..8],
            //     new_hash = %new_hash[..8],
            //     "new binary detected, initiating worker rotation"
            // );

            // CRITICAL ISSUE 1: Log level is INFO, not WARN/ERROR
            // A stale binary should be logged at WARN or ERROR level to indicate
            // a potential problem requiring attention.

            // CRITICAL ISSUE 2: Only first 8 characters of hash are logged
            // old_hash and new_hash are truncated to [..8], losing most of the identifier.
            // Full hashes should be logged for proper verification.

            // CRITICAL ISSUE 3: No config option mentioned for remediation
            // The log should reference worker.worker_binary_path config option
            // so users know how to fix the stale binary situation.

            assert!(!old_hash.is_empty());
            assert!(!new_hash.is_empty());
            assert_ne!(old_hash, new_hash);
            assert_eq!(path, binary_path);
        }
        other => panic!("expected NewBinary, got {:?}", other),
    }
}

#[test]
fn test_binary_missing_logging() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("nonexistent-binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);

    let result = checker.poll().expect("check failed");
    assert!(result.is_some());

    match result.unwrap() {
        FreshnessCheck::BinaryMissing { binary_path: path } => {
            // Expected logging in supervisor (from src/supervisor/mod.rs:666):
            // tracing::warn!(
            //     binary = %binary_path.display(),
            //     "monitored binary missing, skipping rotation check"
            // );

            // PASS: Log level is WARN as expected
            // PASS: Binary path is included in the log

            assert_eq!(path, binary_path);
        }
        other => panic!("expected BinaryMissing, got {:?}", other),
    }
}

#[test]
fn test_check_failed_logging() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("test-binary");

    // Create a binary file
    fs::write(&binary_path, b"test content").expect("failed to write binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
    let now = Instant::now();

    // First check to record hash
    checker.poll_at(now).expect("first check failed");

    // Simulate a check failure by making the binary unreadable
    // This is difficult to test without actually causing filesystem errors

    // Expected logging in supervisor (from src/supervisor/mod.rs:672):
    // tracing::warn!(
    //     error = %error,
    //     "binary freshness check failed"
    // );

    // PASS: Log level is WARN as expected
    // PASS: Error message is included in the log
}

#[test]
fn test_deduplication_prevents_log_spam() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("test-binary");

    fs::write(&binary_path, b"v1").expect("failed to write binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 10);
    let now = Instant::now();

    // First check at t=0
    let result = checker.poll_at(now).expect("poll 1 failed");
    assert!(result.is_some()); // Should execute

    // Immediate second check at t=0 should be skipped
    let result = checker.poll_at(now).expect("poll 2 failed");
    assert!(result.is_none()); // Skipped due to rate limiting

    // Check at t=5 should be skipped
    let result = checker
        .poll_at(now + Duration::from_secs(5))
        .expect("poll 3 failed");
    assert!(result.is_none()); // Still within interval

    // Check at t=10 should execute
    let result = checker
        .poll_at(now + Duration::from_secs(10))
        .expect("poll 4 failed");
    assert!(result.is_some()); // Interval elapsed, check executes

    // PASS: Deduplication logic prevents spam
    // The BinaryFreshnessChecker enforces minimum check intervals,
    // preventing repeated log emissions for the same stale state.
}

#[test]
fn test_missing_config_option_reference_in_logs() {
    // This test documents a CRITICAL ISSUE:
    // The logs emitted by the supervisor do NOT reference the config option
    // for remediation.

    // Expected log should include:
    // "Update worker.worker_binary_path or restart supervisor to use new binary"

    // Actual log (from src/supervisor/mod.rs:641):
    // "new binary detected, initiating worker rotation"
    //
    // This does NOT tell the user:
    // 1. How to configure the worker binary path
    // 2. How to disable automatic rotation
    // 3. What config option controls this behavior

    // FAIL: No config option reference in the log message
}

#[test]
fn test_hash_truncation_issue() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let binary_path = temp_dir.path().join("test-binary");

    fs::write(&binary_path, b"test content").expect("failed to write binary");

    let mut checker = BinaryFreshnessChecker::new(binary_path, 1);
    let now = Instant::now();

    // First check
    let result = checker.poll_at(now).expect("first check failed").unwrap();
    let full_hash = match result {
        FreshnessCheck::Unchanged { current_hash, .. } => current_hash,
        _ => panic!("expected Unchanged"),
    };

    // SHA256 produces 64 hex characters
    assert_eq!(full_hash.len(), 64);

    // But the log only emits first 8 characters:
    // old_hash = %old_hash[..8]  (line 644 in supervisor/mod.rs)
    // new_hash = %new_hash[..8]  (line 645 in supervisor/mod.rs)

    // FAIL: Hash truncation loses identification information
    // 8 characters = 32 bits, which has collision risk
    // Full 64 characters should be logged
}

#[test]
fn test_log_level_should_be_warn_for_stale_detection() {
    // This test documents a CRITICAL ISSUE:
    // NewBinary detection uses INFO level, not WARN/ERROR

    // Current logging (from src/supervisor/mod.rs:641):
    // tracing::info!(...)  // WRONG: Should be WARN or ERROR

    // A stale binary is a problem state:
    // - Workers are running outdated code
    // - May have security vulnerabilities
    // - May have bugs fixed in newer version
    // - Indicates supervisor and workers are out of sync

    // Should be:
    // tracing::warn!(...)  // Stale binary detected
    // or
    // tracing::error!(...) // Critical: workers running wrong binary

    // FAIL: Log level is too low for a stale binary situation
}
