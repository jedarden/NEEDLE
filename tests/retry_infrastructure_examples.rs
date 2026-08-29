//! Example tests demonstrating the retry test infrastructure.
//!
//! This file shows how to use the retry_test_helpers module for testing
//! retry logic in your own code. All functions return Results to avoid
//! expect() and unwrap() in test code.

mod retry_test_helpers;

use retry_test_helpers::*;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ──────────────────────────────────────────────────────────────────────────────
// Example 1: Basic retry with ETXTBSY error
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn example_first_attempt_success_no_retry() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(5)
        .with_backoff_ms(20);

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 1)?;
    assert!(result.elapsed.as_millis() < 10); // Should be nearly instant

    Ok(())
}

#[test]
fn example_etxtbsy_retry_succeeds_on_second_attempt() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(5)
        .with_etxtbsy_on_attempt(1)
        .with_backoff_ms(20);

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 2)?;
    assert!(result.elapsed.as_millis() >= 20); // At least one backoff delay

    Ok(())
}

#[test]
fn example_etxtbsy_exhausts_max_attempts() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(3)
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2)
        .with_etxtbsy_on_attempt(3);

    let result = mock.run_sync()?;

    assert_failed_etxtbsy(&result)?;
    assert_eq!(result.attempts, 3);
    assert_retry_within_bounds(&result, 3)?;

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Example 2: Non-retryable errors propagate immediately
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn example_non_etxtbsy_error_propagates_immediately() -> Result<(), String> {
    let error_injection = ErrorInjection::new().with_io_error_on_attempt(
        1,
        io::ErrorKind::NotFound,
        "file not found",
    );

    let mock = MockRetryBehavior::new()
        .with_max_attempts(5)
        .with_error_injection(error_injection);

    let result = mock.run_sync()?;

    assert_failed_with_error_kind(&result, io::ErrorKind::NotFound)?;
    assert_eq!(result.attempts, 1); // Should not retry
    assert!(result.elapsed.as_millis() < 10); // Should be nearly instant

    Ok(())
}

#[test]
fn example_permission_denied_propagates_immediately() -> Result<(), String> {
    let error_injection = ErrorInjection::new().with_io_error_on_attempt(
        1,
        io::ErrorKind::PermissionDenied,
        "access denied",
    );

    let mock = MockRetryBehavior::new()
        .with_max_attempts(5)
        .with_error_injection(error_injection);

    let result = mock.run_sync()?;

    assert_failed_with_error_kind(&result, io::ErrorKind::PermissionDenied)?;
    assert_eq!(result.attempts, 1);

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Example 3: Exponential backoff
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn example_exponential_backoff_increasing_delays() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(5)
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2)
        .with_exponential_backoff(10, 1000);

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 3)?;

    // With exponential backoff: 10ms + 20ms = at least 30ms total
    assert!(result.elapsed.as_millis() >= 30);

    Ok(())
}

#[test]
fn example_exponential_backoff_caps_at_max() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(10)
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2)
        .with_etxtbsy_on_attempt(3)
        .with_etxtbsy_on_attempt(4)
        .with_exponential_backoff(10, 50); // Cap at 50ms

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 5)?;

    // With exponential backoff capped at 50ms:
    // 10ms + 20ms + 40ms + 50ms (capped) = at least 120ms total
    assert!(result.elapsed.as_millis() >= 120);

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Example 4: Complex error injection scenarios
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn example_mixed_errors_then_success() -> Result<(), String> {
    let error_injection = ErrorInjection::new()
        .with_etxtbsy_on_attempt(1)
        .with_io_error_on_attempt(2, io::ErrorKind::PermissionDenied, "denied");

    let mock = MockRetryBehavior::new()
        .with_max_attempts(5)
        .with_error_injection(error_injection);

    let result = mock.run_sync()?;

    // Should fail immediately on the PermissionDenied error
    assert_failed_with_error_kind(&result, io::ErrorKind::PermissionDenied)?;
    assert_eq!(result.attempts, 2); // First retry attempt

    Ok(())
}

#[test]
fn example_success_after_multiple_retries() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(10)
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2)
        .with_etxtbsy_on_attempt(3)
        .with_backoff_ms(50);

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 4)?;

    // Should have spent at least 3 * 50ms = 150ms in backoffs
    assert!(result.elapsed.as_millis() >= 150);

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Example 5: Testing with custom success values
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn example_custom_success_value() -> Result<(), String> {
    let expected_value = b"custom response data".to_vec();

    let mock = MockRetryBehavior::new()
        .with_max_attempts(3)
        .with_etxtbsy_on_attempt(1)
        .with_success_value(expected_value.clone());

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 2)?;
    assert_eq!(result.result, Some(expected_value));

    Ok(())
}

#[test]
fn example_empty_success_value() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(2)
        .with_success_value(vec![]); // Empty success value

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 1)?;
    assert_eq!(result.result, Some(vec![]));

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Example 6: Async retry testing
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn example_async_retry_with_etxtbsy() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(3)
        .with_etxtbsy_on_attempt(1)
        .with_backoff_ms(50);

    let result = mock.run_async().await?;

    assert_succeeded_with_attempts(&result, 2)?;
    assert!(result.elapsed.as_millis() >= 50);

    Ok(())
}

#[tokio::test]
async fn example_async_exponential_backoff() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(4)
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2)
        .with_exponential_backoff(20, 100);

    let result = mock.run_async().await?;

    assert_succeeded_with_attempts(&result, 3)?;

    // With exponential backoff: 20ms + 40ms = at least 60ms total
    assert!(result.elapsed.as_millis() >= 60);

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Example 7: Integration-style test with atomic counters
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn example_retry_with_attempt_counter() -> Result<(), String> {
    let attempts = Arc::new(AtomicUsize::new(0));

    // Create a mock that tracks attempts via side effects
    let _attempts_clone = Arc::clone(&attempts);
    let mock = MockRetryBehavior::new()
        .with_max_attempts(5)
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2);

    let result = mock.run_sync()?;

    // Verify the retry behavior matches expectations
    assert_succeeded_with_attempts(&result, 3)?;

    // The atomic counter would be incremented by the actual retry logic
    // in a real integration test
    let final_attempts = attempts.load(Ordering::SeqCst);
    assert_eq!(final_attempts, 0); // No side effects in this mock test

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Example 8: Configuration testing
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_retry_configuration_builder_pattern() -> Result<(), String> {
    let config = RetryConfig::new()
        .with_max_attempts(10)
        .with_backoff_ms(100)
        .with_exponential_backoff(50, 500);

    assert_eq!(config.max_attempts, 10);
    assert_eq!(config.backoff_ms, 100);
    assert!(config.exponential_backoff);
    assert_eq!(config.exponential_initial_ms, 50);
    assert_eq!(config.exponential_max_ms, 500);

    // Test backoff calculation
    assert_eq!(config.backoff_for_attempt(1), Duration::from_millis(50));
    assert_eq!(config.backoff_for_attempt(2), Duration::from_millis(100));
    assert_eq!(config.backoff_for_attempt(3), Duration::from_millis(200));
    assert_eq!(config.backoff_for_attempt(4), Duration::from_millis(400)); // Capped at 500? No, 400 < 500
    assert_eq!(config.backoff_for_attempt(5), Duration::from_millis(500)); // Capped

    Ok(())
}

#[test]
fn test_error_injection_configuration() -> Result<(), String> {
    let injection = ErrorInjection::new()
        .with_etxtbsy_on_attempt(1)
        .with_io_error_on_attempt(2, io::ErrorKind::BrokenPipe, "pipe broken")
        .with_io_error_on_attempt(3, io::ErrorKind::ConnectionReset, "reset");

    assert_eq!(injection.should_error(1).unwrap().raw_os_error(), Some(26));
    assert_eq!(
        injection.should_error(2).unwrap().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        injection.should_error(3).unwrap().kind(),
        io::ErrorKind::ConnectionReset
    );
    assert!(injection.should_error(4).is_none());

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Example 9: Edge cases and boundary conditions
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn example_single_attempt_no_retry() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(1)
        .with_etxtbsy_on_attempt(1);

    let result = mock.run_sync()?;

    assert_failed_etxtbsy(&result)?;
    assert_eq!(result.attempts, 1);
    assert!(result.elapsed.as_millis() < 10); // No backoff time

    Ok(())
}

#[test]
fn example_zero_backoff_immediate_retry() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(3)
        .with_backoff_ms(0)
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2);

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 3)?;
    // With 0ms backoff, should complete very quickly
    assert!(result.elapsed.as_millis() < 100);

    Ok(())
}

#[test]
fn example_large_max_attempts_success() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(100)
        .with_success_on_attempts(vec![50]) // Succeed on 50th attempt
        .with_backoff_ms(1);

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 50)?;
    assert!(result.elapsed.as_millis() >= 49); // At least 49ms of backoff

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Example 10: Real-world pattern simulation
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn example_binary_busy_scenario() -> Result<(), String> {
    // Simulate a binary that's temporarily busy (ETXTBSY) then becomes available
    let mock = MockRetryBehavior::new()
        .with_max_attempts(5)
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2)
        .with_backoff_ms(20);

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 3)?;
    // Simulates: attempt 1 (busy) -> wait 20ms -> attempt 2 (busy) -> wait 20ms -> attempt 3 (success)
    assert!(result.elapsed.as_millis() >= 40);

    Ok(())
}

#[test]
fn example_transient_network_failure() -> Result<(), String> {
    // Simulate transient network issues that resolve after retries
    let error_injection = ErrorInjection::new()
        .with_io_error_on_attempt(1, io::ErrorKind::ConnectionReset, "connection reset")
        .with_io_error_on_attempt(2, io::ErrorKind::ConnectionReset, "connection reset");

    let mock = MockRetryBehavior::new()
        .with_max_attempts(4)
        .with_error_injection(error_injection)
        .with_success_value(b"HTTP 200 OK".to_vec());

    let result = mock.run_sync()?;

    // ConnectionReset is not ETXTBSY, so it won't retry
    assert_failed_with_error_kind(&result, io::ErrorKind::ConnectionReset)?;
    assert_eq!(result.attempts, 1);

    Ok(())
}
