//! Test helper module for retry logic testing.
//!
//! This module provides common utilities, mock frameworks, and test fixtures
//! for testing retry behavior across NEEDLE. All functions return Results to
//! avoid expect() and unwrap() in test code.
//!
//! # Usage
//!
//! ```no_run
//! use retry_test_helpers::*;
//!
//! #[tokio::test]
//! async fn test_my_retry_logic() -> Result<()> {
//!     let mock = MockRetryBehavior::new()
//!         .with_max_attempts(3)
//!         .with_backoff_ms(100)
//!         .with_failure_on_attempt(1, std::io::Error::from_raw_os_error(26));
//!
//!     let result = mock.run().await?;
//!
//!     assert_eq!(result.attempts, 2);
//!     Ok(())
//! }
//! ```

use std::io;
use tokio::time::{Duration, Instant};

// ──────────────────────────────────────────────────────────────────────────────
// Error injection utilities
// ──────────────────────────────────────────────────────────────────────────────

/// Error injection configuration for testing retry behavior.
#[derive(Debug)]
pub struct ErrorInjection {
    /// Errors to inject on specific attempts (1-indexed).
    /// Stores error specs that can be converted to io::Error when needed.
    pub errors_on_attempts: Vec<(usize, ErrorSpec)>,
}

/// Specification for creating an io::Error on demand.
#[derive(Debug, Clone)]
pub enum ErrorSpec {
    /// ETXTBSY error (errno 26)
    Etxtbsy,
    /// Generic IO error with kind and message
    Io(io::ErrorKind, String),
}

impl ErrorSpec {
    /// Convert the spec to an actual io::Error.
    pub fn to_error(&self) -> io::Error {
        match self {
            ErrorSpec::Etxtbsy => io::Error::from_raw_os_error(26),
            ErrorSpec::Io(kind, msg) => io::Error::new(*kind, msg.as_str()),
        }
    }
}

impl ErrorInjection {
    /// Create a new error injection configuration.
    pub fn new() -> Self {
        Self {
            errors_on_attempts: Vec::new(),
        }
    }

    /// Inject an error spec on a specific attempt (1-indexed).
    pub fn with_error_on_attempt(mut self, attempt: usize, spec: ErrorSpec) -> Self {
        self.errors_on_attempts.push((attempt, spec));
        self
    }

    /// Inject ETXTBSY error on a specific attempt.
    pub fn with_etxtbsy_on_attempt(mut self, attempt: usize) -> Self {
        self.errors_on_attempts.push((attempt, ErrorSpec::Etxtbsy));
        self
    }

    /// Inject a generic IO error on a specific attempt.
    pub fn with_io_error_on_attempt(
        mut self,
        attempt: usize,
        kind: io::ErrorKind,
        msg: &str,
    ) -> Self {
        self.errors_on_attempts
            .push((attempt, ErrorSpec::Io(kind, msg.to_string())));
        self
    }

    /// Check if an error should be injected for the given attempt.
    pub fn should_error(&self, attempt: usize) -> Option<io::Error> {
        self.errors_on_attempts
            .iter()
            .find(|(att, _)| *att == attempt)
            .map(|(_, spec)| spec.to_error())
    }
}

impl Default for ErrorInjection {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Retry configuration fixtures
// ──────────────────────────────────────────────────────────────────────────────

/// Configurable retry behavior for testing.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of attempts before giving up.
    pub max_attempts: usize,
    /// Backoff delay in milliseconds between attempts.
    pub backoff_ms: u64,
    /// Whether to use exponential backoff.
    pub exponential_backoff: bool,
    /// Initial delay for exponential backoff (ms).
    pub exponential_initial_ms: u64,
    /// Maximum delay for exponential backoff (ms).
    pub exponential_max_ms: u64,
}

impl RetryConfig {
    /// Create a new retry configuration with sensible defaults.
    pub fn new() -> Self {
        Self {
            max_attempts: 5,
            backoff_ms: 20,
            exponential_backoff: false,
            exponential_initial_ms: 10,
            exponential_max_ms: 1000,
        }
    }

    /// Set the maximum number of attempts.
    pub fn with_max_attempts(mut self, max: usize) -> Self {
        self.max_attempts = max;
        self
    }

    /// Set the backoff delay in milliseconds.
    pub fn with_backoff_ms(mut self, ms: u64) -> Self {
        self.backoff_ms = ms;
        self
    }

    /// Enable exponential backoff with given initial and maximum delays.
    pub fn with_exponential_backoff(mut self, initial_ms: u64, max_ms: u64) -> Self {
        self.exponential_backoff = true;
        self.exponential_initial_ms = initial_ms;
        self.exponential_max_ms = max_ms;
        self
    }

    /// Calculate the backoff delay for a given attempt number.
    pub fn backoff_for_attempt(&self, attempt: usize) -> Duration {
        if self.exponential_backoff {
            let delay_ms = (self.exponential_initial_ms * 2_u64.pow(attempt as u32 - 1))
                .min(self.exponential_max_ms);
            Duration::from_millis(delay_ms)
        } else {
            Duration::from_millis(self.backoff_ms)
        }
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Mock retry behavior executor
// ──────────────────────────────────────────────────────────────────────────────

/// Result of a mock retry operation.
#[derive(Debug)]
pub struct RetryResult {
    /// Number of attempts made.
    pub attempts: usize,
    /// Whether the operation ultimately succeeded.
    pub succeeded: bool,
    /// Total time elapsed across all attempts.
    pub elapsed: Duration,
    /// The final result (if successful).
    pub result: Option<Vec<u8>>,
    /// The final error (if failed).
    pub error: Option<io::Error>,
}

/// Mock retry behavior executor for testing retry logic.
pub struct MockRetryBehavior {
    config: RetryConfig,
    error_injection: ErrorInjection,
    success_on_attempts: Vec<usize>,
    success_value: Vec<u8>,
}

impl MockRetryBehavior {
    /// Create a new mock retry behavior with default configuration.
    pub fn new() -> Self {
        Self {
            config: RetryConfig::new(),
            error_injection: ErrorInjection::new(),
            success_on_attempts: vec![1], // Succeed on first attempt by default
            success_value: b"success".to_vec(),
        }
    }

    /// Set the retry configuration.
    pub fn with_config(mut self, config: RetryConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the error injection configuration.
    pub fn with_error_injection(mut self, injection: ErrorInjection) -> Self {
        self.error_injection = injection;
        self
    }

    /// Set the maximum number of attempts.
    pub fn with_max_attempts(mut self, max: usize) -> Self {
        self.config = self.config.with_max_attempts(max);
        self
    }

    /// Set the backoff delay in milliseconds.
    pub fn with_backoff_ms(mut self, ms: u64) -> Self {
        self.config = self.config.with_backoff_ms(ms);
        self
    }

    /// Enable exponential backoff.
    pub fn with_exponential_backoff(mut self, initial_ms: u64, max_ms: u64) -> Self {
        self.config = self.config.with_exponential_backoff(initial_ms, max_ms);
        self
    }

    /// Inject an error on a specific attempt.
    pub fn with_error_on_attempt(mut self, attempt: usize, spec: ErrorSpec) -> Self {
        self.error_injection = self.error_injection.with_error_on_attempt(attempt, spec);
        self
    }

    /// Inject ETXTBSY error on a specific attempt.
    pub fn with_etxtbsy_on_attempt(self, attempt: usize) -> Self {
        self.with_error_on_attempt(attempt, ErrorSpec::Etxtbsy)
    }

    /// Inject a generic IO error on a specific attempt.
    pub fn with_io_error_on_attempt(
        mut self,
        attempt: usize,
        kind: io::ErrorKind,
        msg: &str,
    ) -> Self {
        self.error_injection = self
            .error_injection
            .with_io_error_on_attempt(attempt, kind, msg);
        self
    }

    /// Set which attempts should succeed (default: first attempt only).
    pub fn with_success_on_attempts(mut self, attempts: Vec<usize>) -> Self {
        self.success_on_attempts = attempts;
        self
    }

    /// Set the value to return on success.
    pub fn with_success_value(mut self, value: Vec<u8>) -> Self {
        self.success_value = value;
        self
    }

    /// Run the mock retry behavior synchronously.
    pub fn run_sync(self) -> Result<RetryResult, String> {
        let start = Instant::now();
        let mut attempts = 0;
        let mut result: Option<Vec<u8>> = None;
        let mut error: Option<io::Error> = None;

        for attempt in 1..=self.config.max_attempts {
            attempts = attempt;

            // Check for injected error
            if let Some(injected_error) = self.error_injection.should_error(attempt) {
                // Check if this is a retryable error (ETXTBSY is retryable)
                if injected_error.raw_os_error() == Some(26) && attempt < self.config.max_attempts {
                    // Retry after backoff
                    let delay = self.config.backoff_for_attempt(attempt);
                    std::thread::sleep(delay);
                    continue;
                } else {
                    // Non-retryable error or max attempts exhausted
                    error = Some(injected_error);
                    break;
                }
            }

            // Check if this attempt should succeed
            if self.success_on_attempts.contains(&attempt) {
                result = Some(self.success_value.clone());
                break;
            }

            // If we get here, the attempt failed - retry if possible
            if attempt < self.config.max_attempts {
                let delay = self.config.backoff_for_attempt(attempt);
                std::thread::sleep(delay);
            }
        }

        let elapsed = start.elapsed();
        let succeeded = result.is_some();

        Ok(RetryResult {
            attempts,
            succeeded,
            elapsed,
            result,
            error,
        })
    }

    /// Run the mock retry behavior asynchronously.
    pub async fn run_async(self) -> Result<RetryResult, String> {
        let start = Instant::now();
        let mut attempts = 0;
        let mut result: Option<Vec<u8>> = None;
        let mut error: Option<io::Error> = None;

        for attempt in 1..=self.config.max_attempts {
            attempts = attempt;

            // Check for injected error
            if let Some(injected_error) = self.error_injection.should_error(attempt) {
                // Check if this is a retryable error (ETXTBSY is retryable)
                if injected_error.raw_os_error() == Some(26) && attempt < self.config.max_attempts {
                    // Retry after backoff
                    let delay = self.config.backoff_for_attempt(attempt);
                    tokio::time::sleep(delay).await;
                    continue;
                } else {
                    // Non-retryable error or max attempts exhausted
                    error = Some(injected_error);
                    break;
                }
            }

            // Check if this attempt should succeed
            if self.success_on_attempts.contains(&attempt) {
                result = Some(self.success_value.clone());
                break;
            }

            // If we get here, the attempt failed - retry if possible
            if attempt < self.config.max_attempts {
                let delay = self.config.backoff_for_attempt(attempt);
                tokio::time::sleep(delay).await;
            }
        }

        let elapsed = start.elapsed();
        let succeeded = result.is_some();

        Ok(RetryResult {
            attempts,
            succeeded,
            elapsed,
            result,
            error,
        })
    }
}

impl Default for MockRetryBehavior {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Common error constructors
// ──────────────────────────────────────────────────────────────────────────────

/// Create an ETXTBSY error (errno 26).
pub fn etxtbsy_error() -> io::Error {
    io::Error::from_raw_os_error(26)
}

/// Create an error of the given kind with a message.
pub fn io_error(kind: io::ErrorKind, msg: &str) -> io::Error {
    io::Error::new(kind, msg)
}

// ──────────────────────────────────────────────────────────────────────────────
// Assertion helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Assert that a retry result succeeded with the expected number of attempts.
pub fn assert_succeeded_with_attempts(
    result: &RetryResult,
    expected_attempts: usize,
) -> Result<(), String> {
    if !result.succeeded {
        return Err(format!(
            "Expected success after {} attempts, but operation failed. Last error: {:?}",
            expected_attempts, result.error
        ));
    }
    if result.attempts != expected_attempts {
        return Err(format!(
            "Expected {} attempts, but got {}",
            expected_attempts, result.attempts
        ));
    }
    Ok(())
}

/// Assert that a retry result failed with the expected error kind.
pub fn assert_failed_with_error_kind(
    result: &RetryResult,
    expected_kind: io::ErrorKind,
) -> Result<(), String> {
    if result.succeeded {
        return Err(format!(
            "Expected failure with {:?}, but operation succeeded after {} attempts",
            expected_kind, result.attempts
        ));
    }
    let actual_kind = result
        .error
        .as_ref()
        .map(|e| e.kind())
        .ok_or("No error present in failed result")?;

    if actual_kind != expected_kind {
        return Err(format!(
            "Expected error kind {:?}, but got {:?}",
            expected_kind, actual_kind
        ));
    }
    Ok(())
}

/// Assert that a retry result failed with ETXTBSY error.
pub fn assert_failed_etxtbsy(result: &RetryResult) -> Result<(), String> {
    if result.succeeded {
        return Err(format!(
            "Expected ETXTBSY failure, but operation succeeded after {} attempts",
            result.attempts
        ));
    }
    let errno = result
        .error
        .as_ref()
        .and_then(|e| e.raw_os_error())
        .ok_or("Error has no raw OS error code")?;

    if errno != 26 {
        return Err(format!(
            "Expected ETXTBSY (errno 26), but got errno {}",
            errno
        ));
    }
    Ok(())
}

/// Assert that retry attempts stayed within configured limits.
pub fn assert_retry_within_bounds(result: &RetryResult, max_attempts: usize) -> Result<(), String> {
    if result.attempts > max_attempts {
        return Err(format!(
            "Expected at most {} attempts, but got {}",
            max_attempts, result.attempts
        ));
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Test fixtures
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_injection_should_error() {
        let injection = ErrorInjection::new()
            .with_etxtbsy_on_attempt(1)
            .with_io_error_on_attempt(2, io::ErrorKind::NotFound, "not found");

        assert!(injection.should_error(1).is_some());
        assert!(injection.should_error(2).is_some());
        assert!(injection.should_error(3).is_none());
    }

    #[test]
    fn test_retry_config_defaults() {
        let config = RetryConfig::new();
        assert_eq!(config.max_attempts, 5);
        assert_eq!(config.backoff_ms, 20);
        assert!(!config.exponential_backoff);
    }

    #[test]
    fn test_retry_config_builder() {
        let config = RetryConfig::new()
            .with_max_attempts(10)
            .with_backoff_ms(50)
            .with_exponential_backoff(100, 500);

        assert_eq!(config.max_attempts, 10);
        assert_eq!(config.backoff_ms, 50);
        assert!(config.exponential_backoff);
        assert_eq!(config.exponential_initial_ms, 100);
        assert_eq!(config.exponential_max_ms, 500);
    }

    #[test]
    fn test_retry_config_linear_backoff() {
        let config = RetryConfig::new().with_backoff_ms(100);

        assert_eq!(config.backoff_for_attempt(1), Duration::from_millis(100));
        assert_eq!(config.backoff_for_attempt(2), Duration::from_millis(100));
        assert_eq!(config.backoff_for_attempt(5), Duration::from_millis(100));
    }

    #[test]
    fn test_retry_config_exponential_backoff() {
        let config = RetryConfig::new().with_exponential_backoff(10, 1000);

        assert_eq!(config.backoff_for_attempt(1), Duration::from_millis(10));
        assert_eq!(config.backoff_for_attempt(2), Duration::from_millis(20));
        assert_eq!(config.backoff_for_attempt(3), Duration::from_millis(40));
        assert_eq!(config.backoff_for_attempt(4), Duration::from_millis(80));
        // Should cap at max
        assert_eq!(config.backoff_for_attempt(8), Duration::from_millis(1000));
    }

    #[test]
    fn test_mock_retry_success_on_first_attempt() -> Result<(), String> {
        let mock = MockRetryBehavior::new().with_max_attempts(5);
        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 1)?;
        assert_eq!(result.result, Some(b"success".to_vec()));
        Ok(())
    }

    #[test]
    fn test_mock_retry_success_after_retries() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(5)
            .with_success_on_attempts(vec![3]) // Succeed on 3rd attempt
            .with_backoff_ms(10);

        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 3)?;
        Ok(())
    }

    #[test]
    fn test_mock_retry_etxtbsy_retry_succeeds() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(5)
            .with_etxtbsy_on_attempt(1)
            .with_success_on_attempts(vec![2]);

        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 2)?;
        Ok(())
    }

    #[test]
    fn test_mock_retry_etxtbsy_exhausted() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(3)
            .with_etxtbsy_on_attempt(1)
            .with_etxtbsy_on_attempt(2)
            .with_etxtbsy_on_attempt(3);

        let result = mock.run_sync()?;

        assert_failed_etxtbsy(&result)?;
        assert_eq!(result.attempts, 3);
        Ok(())
    }

    #[test]
    fn test_mock_retry_non_etxtbsy_fails_immediately() -> Result<(), String> {
        let error_injection =
            ErrorInjection::new().with_io_error_on_attempt(1, io::ErrorKind::NotFound, "not found");

        let mock = MockRetryBehavior::new()
            .with_max_attempts(5)
            .with_error_injection(error_injection);

        let result = mock.run_sync()?;

        assert_failed_with_error_kind(&result, io::ErrorKind::NotFound)?;
        assert_eq!(result.attempts, 1); // Should not retry
        Ok(())
    }

    #[test]
    fn test_assertion_helpers() -> Result<(), String> {
        let success_result = RetryResult {
            attempts: 2,
            succeeded: true,
            elapsed: Duration::from_millis(50),
            result: Some(b"success".to_vec()),
            error: None,
        };

        assert_succeeded_with_attempts(&success_result, 2)?;
        assert_retry_within_bounds(&success_result, 5)?;

        let failure_result = RetryResult {
            attempts: 3,
            succeeded: false,
            elapsed: Duration::from_millis(100),
            result: None,
            error: Some(etxtbsy_error()),
        };

        assert_failed_etxtbsy(&failure_result)?;
        assert_retry_within_bounds(&failure_result, 5)?;

        Ok(())
    }

    #[tokio::test]
    async fn test_mock_retry_async() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(3)
            .with_etxtbsy_on_attempt(1)
            .with_success_on_attempts(vec![2]);

        let result = mock.run_async().await?;

        assert_succeeded_with_attempts(&result, 2)?;
        Ok(())
    }
}
