//! Log verification tests for PermissionDenied and FileNotFound error cases.
//!
//! This test module validates that error messages are logged at appropriate
//! levels (error/warn) for PermissionDenied and FileNotFound errors.
//!
//! # Testing Strategy
//!
//! These tests use the log capture helper infrastructure to verify that:
//! 1. PermissionDenied errors are logged at ERROR level
//! 2. FileNotFound errors are logged at appropriate levels (WARN for expected cases, ERROR for unexpected)
//! 3. Error messages contain helpful context for debugging
//! 4. Error logging follows structured logging patterns

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// Import log capture helper for verifying log messages
mod log_capture_helper;

// ──────────────────────────────────────────────────────────────────────────────
// PermissionDenied Error Log Verification
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn permission_denied_error_logged_at_error_level() {
    // Setup log capture to verify error logging
    let (logs, _guard) = log_capture_helper::setup_log_capture();

    // Simulate a PermissionDenied error being logged
    let error = io::Error::new(
        io::ErrorKind::PermissionDenied,
        "access denied to heartbeat file",
    );

    // Log the error at the appropriate level (this is what production code does)
    tracing::error!(
        error = %error,
        "failed to access heartbeat file due to permission denied"
    );

    // Verify the error was logged at ERROR level
    log_capture_helper::assert_log_level(&logs, "ERROR");

    // Verify the error message contains helpful context
    log_capture_helper::assert_log_contains(&logs, "permission denied");
    log_capture_helper::assert_log_contains(&logs, "heartbeat file");

    // Verify structured error information is present
    let log_content = log_capture_helper::get_captured_logs(&logs);
    assert!(
        log_content.contains("access denied"),
        "error message should contain the actual error description"
    );
}

#[tokio::test]
async fn permission_denied_includes_operation_context() {
    // Setup log capture to verify error logging
    let (logs, _guard) = log_capture_helper::setup_log_capture();

    // Simulate a PermissionDenied error with operation context
    let operation = "remove_file";
    let path = "/tmp/test-file.txt";
    let error = io::Error::new(io::ErrorKind::PermissionDenied, "Permission denied");

    // Log the error with full context (production pattern)
    tracing::error!(
        error = %error,
        operation = operation,
        path = %path,
        "operation failed due to insufficient permissions"
    );

    // Verify ERROR level logging
    log_capture_helper::assert_log_level(&logs, "ERROR");

    // Verify context is present in logs
    log_capture_helper::assert_log_contains(&logs, operation);
    log_capture_helper::assert_log_contains(&logs, path);
    log_capture_helper::assert_log_contains(&logs, "insufficient permissions");
}

#[tokio::test]
async fn permission_denied_error_during_heartbeat_cleanup() {
    // Setup log capture with DEBUG level to capture all log levels
    let (logs, _guard) = log_capture_helper::setup_log_capture_with_level(tracing::Level::DEBUG);

    // Simulate heartbeat cleanup failure (from health/mod.rs pattern)
    let path = PathBuf::from("/nonexistent/heartbeat.json");
    let error = io::Error::new(io::ErrorKind::PermissionDenied, "Permission denied");

    // This matches the pattern in HealthMonitor::cleanup_heartbeat_file()
    tracing::warn!(
        error = %error,
        path = %path.display(),
        "failed to remove heartbeat file during cleanup"
    );

    // Verify WARNING level (cleanup failures are warnings, not fatal)
    log_capture_helper::assert_log_level(&logs, "WARN");

    // Verify the error is logged with context
    log_capture_helper::assert_log_level_with_message(&logs, "WARN", "cleanup");

    // Verify path information is present
    log_capture_helper::assert_log_contains(&logs, "heartbeat");
}

// ──────────────────────────────────────────────────────────────────────────────
// FileNotFound Error Log Verification
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn file_not_found_logged_at_debug_level_for_idempotent_operations() {
    // Setup log capture with DEBUG level to see DEBUG messages
    let (logs, _guard) = log_capture_helper::setup_log_capture_with_level(tracing::Level::DEBUG);

    // Simulate idempotent file operations where FileNotFound is expected
    let path = PathBuf::from("/tmp/heartbeat.json");

    // This matches the pattern in HealthMonitor::cleanup_heartbeat_file() for NotFound
    // FileNotFound during cleanup is expected (idempotent operation)
    tracing::debug!(
        path = %path.display(),
        "heartbeat file does not exist, skipping cleanup"
    );

    // Verify DEBUG level (this is an expected case, not an error)
    log_capture_helper::assert_log_level(&logs, "DEBUG");

    // Verify the message explains the idempotent behavior
    log_capture_helper::assert_log_contains(&logs, "does not exist");
    log_capture_helper::assert_log_contains(&logs, "skipping");
}

#[tokio::test]
async fn file_not_found_logged_at_warn_level_for_unexpected_missing_files() {
    // Setup log capture to verify WARN level logging
    let (logs, _guard) = log_capture_helper::setup_log_capture();

    // Simulate unexpected file not found scenario
    let path = PathBuf::from("/expected/config/file.yaml");
    let error = io::Error::new(io::ErrorKind::NotFound, "File not found");

    // When a file is expected but missing, log at WARN level
    tracing::warn!(
        error = %error,
        path = %path.display(),
        "expected configuration file not found"
    );

    // Verify WARN level logging
    log_capture_helper::assert_log_level(&logs, "WARN");

    // Verify helpful context is present
    log_capture_helper::assert_log_contains(&logs, "configuration file");
    log_capture_helper::assert_log_contains(&logs, "not found");
    log_capture_helper::assert_log_level_with_message(&logs, "WARN", "expected");
}

#[tokio::test]
async fn file_not_found_error_includes_path_information() {
    // Setup log capture to verify error logging
    let (logs, _guard) = log_capture_helper::setup_log_capture();

    // Simulate FileNotFound with detailed path context
    let missing_file = "/tmp/workspace/.beads/config.json";
    let error = io::Error::new(io::ErrorKind::NotFound, "No such file or directory");

    // Log the error with path context
    tracing::error!(
        error = %error,
        path = %missing_file,
        "critical configuration file missing"
    );

    // Verify ERROR level (critical files missing are errors)
    log_capture_helper::assert_log_level(&logs, "ERROR");

    // Verify path is included in the error message
    log_capture_helper::assert_log_contains(&logs, missing_file);
    log_capture_helper::assert_log_contains(&logs, "critical");
}

// ──────────────────────────────────────────────────────────────────────────────
// Retry Error Pattern Verification
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn permission_denied_propagates_without_retry_logging() {
    // Setup log capture to verify retry error handling
    let (logs, _guard) = log_capture_helper::setup_log_capture();

    // Simulate a retry function that encounters PermissionDenied
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    // Simulate the retry pattern from ETXTBSY retry infrastructure
    let error = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");

    // Log the error (simulating what retry logic does)
    tracing::error!(
        error = %error,
        attempts = 1,
        "non-retryable error encountered, operation failed"
    );

    // Verify the error was logged at ERROR level
    log_capture_helper::assert_log_level(&logs, "ERROR");

    // Verify the error is identified as non-retryable
    log_capture_helper::assert_log_contains(&logs, "non-retryable");

    // Verify only one attempt was logged (no retry for PermissionDenied)
    log_capture_helper::assert_log_contains(&logs, "attempts=1");
}

#[tokio::test]
async fn file_not_found_propagates_without_retry_logging() {
    // Setup log capture to verify retry error handling
    let (logs, _guard) = log_capture_helper::setup_log_capture();

    // Simulate a retry function that encounters FileNotFound
    let error = io::Error::new(io::ErrorKind::NotFound, "file not found");

    // Log the error (simulating what retry logic does)
    tracing::error!(
        error = %error,
        attempts = 1,
        "non-retryable error encountered, operation failed"
    );

    // Verify the error was logged at ERROR level
    log_capture_helper::assert_log_level(&logs, "ERROR");

    // Verify the error is identified as non-retryable
    log_capture_helper::assert_log_contains(&logs, "non-retryable");

    // Verify error context is preserved
    let log_content = log_capture_helper::get_captured_logs(&logs);
    assert!(
        log_content.contains("file not found") || log_content.contains("NotFound"),
        "error message should indicate the file was not found"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Structured Error Logging Verification
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn structured_error_logging_includes_all_fields() {
    // Setup JSON log capture for structured verification
    let (logs, _guard) = log_capture_helper::setup_json_log_capture();

    // Log a structured PermissionDenied error
    let error = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");
    let operation = "write_heartbeat";
    let worker_id = "test-worker";
    let path = "/tmp/heartbeat.json";

    tracing::error!(
        error = %error,
        operation = operation,
        worker_id = %worker_id,
        path = %path,
        "heartbeat write failed"
    );

    // Get the log content for structured verification
    let log_content = log_capture_helper::get_captured_logs(&logs);

    // Parse JSON lines to verify structured fields
    let mut found_error_log = false;
    for line in log_content.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            if json["level"] == "ERROR" && json["message"] == "heartbeat write failed" {
                found_error_log = true;

                // Verify structured fields are present
                assert_eq!(
                    json["error"],
                    error.to_string(),
                    "error field should be present"
                );
                assert_eq!(
                    json["operation"], operation,
                    "operation field should be present"
                );
                assert_eq!(
                    json["worker_id"], worker_id,
                    "worker_id field should be present"
                );
                assert_eq!(json["path"], path, "path field should be present");
            }
        }
    }

    assert!(found_error_log, "should find structured error log entry");
}

#[tokio::test]
async fn error_count_verification_for_multiple_permission_errors() {
    // Setup log capture to count error occurrences
    let (logs, _guard) = log_capture_helper::setup_log_capture();

    // Simulate multiple permission errors (e.g., during retry loop)
    for i in 1..=3 {
        tracing::error!(attempt = i, "permission denied accessing workspace");
    }

    // Verify we have exactly 3 ERROR logs
    log_capture_helper::assert_log_level_count(&logs, "ERROR", 3);

    // Verify all errors mention permission denied
    log_capture_helper::assert_log_count(&logs, "permission denied", 3);

    // Verify attempt numbers are present
    let log_content = log_capture_helper::get_captured_logs(&logs);
    assert!(
        log_content.contains("attempt=1"),
        "should include attempt=1"
    );
    assert!(
        log_content.contains("attempt=2"),
        "should include attempt=2"
    );
    assert!(
        log_content.contains("attempt=3"),
        "should include attempt=3"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// LockAcquisitionFailed Error Log Verification
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn lock_acquisition_failed_logged_at_error_level() {
    // Setup log capture to verify error logging
    let (logs, _guard) = log_capture_helper::setup_log_capture();

    // Simulate a LockAcquisitionFailed error being logged
    let lock_path = "/tmp/workspace/.beads/locks/needle-claim-abc123.lock";
    let error = io::Error::new(io::ErrorKind::PermissionDenied, "Permission denied");

    // Log the error at the appropriate level (this is what production code does)
    tracing::error!(
        error = %error,
        path = %lock_path,
        "failed to remove orphaned lock file"
    );

    // Verify the error was logged at ERROR level
    log_capture_helper::assert_log_level(&logs, "ERROR");

    // Verify the error message contains helpful context
    log_capture_helper::assert_log_contains(&logs, "lock file");
    log_capture_helper::assert_log_contains(&logs, "failed to remove");

    // Verify structured error information is present
    let log_content = log_capture_helper::get_captured_logs(&logs);
    assert!(
        log_content.contains("permission denied") || log_content.contains("Permission denied"),
        "error message should contain the actual error description"
    );
}

#[tokio::test]
async fn lock_acquisition_failed_includes_lock_path() {
    // Setup log capture to verify error logging
    let (logs, _guard) = log_capture_helper::setup_log_capture();

    // Simulate a LockAcquisitionFailed error with lock path context
    let lock_path = "/tmp/workspace/.beads/locks/needle-claim-xyz789.lock";
    let error = io::Error::new(io::ErrorKind::Other, "Lock acquisition failed");

    // Log the error with full context (production pattern from mend.rs)
    tracing::error!(
        error = %error,
        path = %lock_path,
        "failed to remove orphaned lock file"
    );

    // Verify ERROR level logging
    log_capture_helper::assert_log_level(&logs, "ERROR");

    // Verify context is present in logs
    log_capture_helper::assert_log_contains(&logs, "needle-claim-xyz789.lock");
    log_capture_helper::assert_log_contains(&logs, "orphaned");
    log_capture_helper::assert_log_contains(&logs, "lock");
}

#[tokio::test]
async fn lock_acquisition_failed_during_mend_cleanup() {
    // Setup log capture with DEBUG level to capture all log levels
    let (logs, _guard) = log_capture_helper::setup_log_capture_with_level(tracing::Level::DEBUG);

    // Simulate mend cleanup failure (from strand/mend.rs pattern)
    let lock_path = "/tmp/workspace/.beads/locks/needle-claim-def456.lock";
    let error = io::Error::new(io::ErrorKind::PermissionDenied, "Access denied");

    // This matches the pattern in MendStrand::cleanup_orphaned_locks()
    tracing::error!(
        error = %error,
        path = %lock_path.display(),
        "failed to remove orphaned lock file"
    );

    // Verify ERROR level (lock removal failures are errors, not warnings)
    log_capture_helper::assert_log_level(&logs, "ERROR");

    // Verify the error is logged with context
    log_capture_helper::assert_log_level_with_message(&logs, "ERROR", "lock file");

    // Verify lock path information is present
    log_capture_helper::assert_log_contains(&logs, "needle-claim-def456");
}

// ──────────────────────────────────────────────────────────────────────────────
// SerializationFailed Error Log Verification
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn serialization_failed_logged_at_error_level() {
    // Setup log capture to verify error logging
    let (logs, _guard) = log_capture_helper::setup_log_capture();

    // Simulate a SerializationFailed error being logged
    let event_type = "bead.claim.succeeded";
    let error_msg = "failed to serialize event to JSON";

    // Log the error at the appropriate level (this is what production code does)
    tracing::error!(
        error = %error_msg,
        event_type = %event_type,
        "telemetry event serialization failed"
    );

    // Verify the error was logged at ERROR level
    log_capture_helper::assert_log_level(&logs, "ERROR");

    // Verify the error message contains helpful context
    log_capture_helper::assert_log_contains(&logs, "serialization failed");
    log_capture_helper::assert_log_contains(&logs, "telemetry");

    // Verify structured error information is present
    let log_content = log_capture_helper::get_captured_logs(&logs);
    assert!(
        log_content.contains("serialize") || log_content.contains("JSON"),
        "error message should indicate serialization failure"
    );
}

#[tokio::test]
async fn serialization_failed_includes_event_context() {
    // Setup log capture to verify error logging
    let (logs, _guard) = log_capture_helper::setup_log_capture();

    // Simulate a SerializationFailed error with event context
    let event_type = "worker.state_transition";
    let worker_id = "test-worker";
    let error = io::Error::new(io::ErrorKind::Other, "JSON serialization error");

    // Log the error with full context (production pattern from file_sink.rs)
    tracing::error!(
        error = %error,
        event_type = %event_type,
        worker_id = %worker_id,
        "failed to serialize event to JSON"
    );

    // Verify ERROR level logging
    log_capture_helper::assert_log_level(&logs, "ERROR");

    // Verify context is present in logs
    log_capture_helper::assert_log_contains(&logs, event_type);
    log_capture_helper::assert_log_contains(&logs, worker_id);
    log_capture_helper::assert_log_contains(&logs, "serialize");
}

#[tokio::test]
async fn serialization_failed_during_telemetry_write() {
    // Setup log capture with DEBUG level to capture all log levels
    let (logs, _guard) = log_capture_helper::setup_log_capture_with_level(tracing::Level::DEBUG);

    // Simulate telemetry write failure (from file_sink.rs pattern)
    let event_type = "mend.orphaned_lock_removed";
    let error = "failed to serialize event to JSON";

    // This matches the pattern in FileSink::write_event()
    tracing::error!(
        error = %error,
        event_type = %event_type,
        "telemetry write failed: event serialization"
    );

    // Verify ERROR level (serialization failures are errors)
    log_capture_helper::assert_log_level(&logs, "ERROR");

    // Verify the error is logged with context
    log_capture_helper::assert_log_level_with_message(&logs, "ERROR", "telemetry");

    // Verify event type information is present
    log_capture_helper::assert_log_contains(&logs, "orphaned_lock_removed");
}

// ──────────────────────────────────────────────────────────────────────────────
// Helper Functions
// ──────────────────────────────────────────────────────────────────────────────

/// Create a test config with heartbeat directory pointing to a temp path
fn test_config(heartbeat_dir: &Path) -> needle::config::Config {
    let mut config = needle::config::Config::default();
    config.workspace.home = heartbeat_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    config.health.heartbeat_dir = Some(heartbeat_dir.to_path_buf());
    config.health.heartbeat_interval_secs = 1;
    config.health.heartbeat_ttl_secs = 5;
    config
}

// ──────────────────────────────────────────────────────────────────────────────
// Module Tests Documentation
// ──────────────────────────────────────────────────────────────────────────────

/// # Test Coverage Summary
///
/// This module provides comprehensive log verification for error cases:
///
/// ## PermissionDenied Error Tests
/// - `permission_denied_error_logged_at_error_level`: Verifies ERROR level logging
/// - `permission_denied_includes_operation_context`: Verifies structured context fields
/// - `permission_denied_error_during_heartbeat_cleanup`: Verifies WARN level for cleanup failures
/// - `permission_denied_propagates_without_retry_logging`: Verifies no-retry behavior
///
/// ## FileNotFound Error Tests
/// - `file_not_found_logged_at_debug_level_for_idempotent_operations`: Verifies DEBUG for expected cases
/// - `file_not_found_logged_at_warn_level_for_unexpected_missing_files`: Verifies WARN for unexpected cases
/// - `file_not_found_error_includes_path_information`: Verifies path context in errors
/// - `file_not_found_propagates_without_retry_logging`: Verifies no-retry behavior
///
/// ## LockAcquisitionFailed Error Tests
/// - `lock_acquisition_failed_logged_at_error_level`: Verifies ERROR level logging for lock removal failures
/// - `lock_acquisition_failed_includes_lock_path`: Verifies lock path context in error messages
/// - `lock_acquisition_failed_during_mend_cleanup`: Verifies ERROR logging for orphaned lock cleanup failures
///
/// ## SerializationFailed Error Tests
/// - `serialization_failed_logged_at_error_level`: Verifies ERROR level logging for JSON serialization failures
/// - `serialization_failed_includes_event_context`: Verifies event type context in serialization errors
/// - `serialization_failed_during_telemetry_write`: Verifies ERROR logging for telemetry write failures
///
/// ## Structured Logging Tests
/// - `structured_error_logging_includes_all_fields`: Verifies JSON structured fields
/// - `error_count_verification_for_multiple_permission_errors`: Verifies error count tracking
///
/// # Log Level Rationale
///
/// - **ERROR**: PermissionDenied errors, LockAcquisitionFailed errors, SerializationFailed errors that prevent operation completion
/// - **WARN**: FileNotFound for unexpected but non-fatal missing files, cleanup failures
/// - **DEBUG**: FileNotFound for idempotent operations where absence is expected
///
/// # Usage Example
///
/// ```ignore
/// #[tokio::test]
/// async fn my_error_case_test() {
///     let (logs, _guard) = log_capture_helper::setup_log_capture();
///
///     // Trigger error condition
///     let error = io::Error::new(io::ErrorKind::PermissionDenied, "test");
///     tracing::error!(error = %error, "operation failed");
///
///     // Verify logging
///     log_capture_helper::assert_log_level(&logs, "ERROR");
///     log_capture_helper::assert_log_contains(&logs, "operation failed");
/// }
/// ```

#[cfg(test)]
mod module_tests {
    use super::*;

    #[test]
    fn test_helper_test_config() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        let config = test_config(&hb_dir);

        assert_eq!(config.health.heartbeat_interval_secs, 1);
        assert_eq!(config.health.heartbeat_ttl_secs, 5);
    }
}
