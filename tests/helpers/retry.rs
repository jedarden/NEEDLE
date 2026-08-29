//! Retry logic test helper module
//!
//! This module provides common utilities and test fixtures for testing retry behavior
//! across NEEDLE. It serves as a centralized location for retry-related test helpers,
//! reducing duplication and ensuring consistent retry behavior testing.
//!
//! # Architecture
//!
//! The module is organized into several sections:
//! - **Error injection**: Utilities for injecting specific errors at retry attempts
//! - **Retry configuration**: Configurable retry behavior fixtures
//! - **Mock executors**: Test doubles for simulating retry scenarios
//! - **Assertion helpers**: Specialized assertions for retry behavior validation
//!
//! # Usage
//!
//! ```no_run
//! use tests::helpers::retry::*;
//!
//! #[tokio::test]
//! async fn test_my_retry_logic() {
//!     let mock = MockRetryBehavior::new()
//!         .with_max_attempts(3)
//!         .with_backoff_ms(100)
//!         .with_etxtbsy_on_attempt(1);
//!
//!     let result = mock.run_async().await.unwrap();
//!
//!     assert_eq!(result.attempts, 2);
//!     assert!(result.succeeded);
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

    /// Check if an error should be injected for the given attempt.
    pub fn should_error(&self, attempt: usize) -> Option<io::Error> {
        self.errors_on_attempts
            .iter()
            .find(|(att, _)| *att == attempt)
            .map(|(_, spec)| match spec {
                ErrorSpec::Etxtbsy => io::Error::from_raw_os_error(26),
                ErrorSpec::Io(kind, msg) => io::Error::new(*kind, msg.as_str()),
            })
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
            success_on_attempts: vec![1],
            success_value: b"success".to_vec(),
        }
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

    /// Inject ETXTBSY error on a specific attempt.
    pub fn with_etxtbsy_on_attempt(mut self, attempt: usize) -> Self {
        self.error_injection = self.error_injection.with_etxtbsy_on_attempt(attempt);
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

    /// Run the mock retry behavior asynchronously.
    pub async fn run_async(self) -> Result<RetryResult, String> {
        let start = Instant::now();
        let mut attempts = 0;
        let mut result: Option<Vec<u8>> = None;
        let mut error: Option<io::Error> = None;

        for attempt in 1..=self.config.max_attempts {
            attempts = attempt;

            if let Some(injected_error) = self.error_injection.should_error(attempt) {
                if injected_error.raw_os_error() == Some(26) && attempt < self.config.max_attempts {
                    let delay = self.config.backoff_for_attempt(attempt);
                    tokio::time::sleep(delay).await;
                    continue;
                } else {
                    error = Some(injected_error);
                    break;
                }
            }

            if self.success_on_attempts.contains(&attempt) {
                result = Some(self.success_value.clone());
                break;
            }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_retry_basic() {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(3)
            .with_success_on_attempts(vec![1]);

        let result = mock.run_async().await.unwrap();
        assert_eq!(result.attempts, 1);
        assert!(result.succeeded);
    }

    #[tokio::test]
    async fn test_mock_retry_with_etxtbsy() {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(3)
            .with_etxtbsy_on_attempt(1)
            .with_success_on_attempts(vec![2]);

        let result = mock.run_async().await.unwrap();
        assert_eq!(result.attempts, 2);
        assert!(result.succeeded);
    }
}
