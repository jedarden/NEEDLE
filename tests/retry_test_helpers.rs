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
    /// Connection refused error (ECONNREFUSED)
    ConnectionRefused,
    /// Connection timed out error (ETIMEDOUT)
    TimedOut,
    /// Network unreachable error (ENETUNREACH)
    NetworkUnreachable,
    /// Host unreachable error (EHOSTUNREACH)
    HostUnreachable,
    /// Broken pipe error (EPIPE)
    BrokenPipe,
    /// Connection reset by peer (ECONNRESET)
    ConnectionReset,
    /// Address in use error (EADDRINUSE)
    AddressInUse,
    /// Permission denied error (EACCES)
    PermissionDenied,
}

impl ErrorSpec {
    /// Convert the spec to an actual io::Error.
    pub fn to_error(&self) -> io::Error {
        match self {
            ErrorSpec::Etxtbsy => io::Error::from_raw_os_error(26),
            ErrorSpec::Io(kind, msg) => io::Error::new(*kind, msg.as_str()),
            ErrorSpec::ConnectionRefused => io::Error::from_raw_os_error(111), // ECONNREFUSED
            ErrorSpec::TimedOut => io::Error::from_raw_os_error(110),          // ETIMEDOUT
            ErrorSpec::NetworkUnreachable => io::Error::from_raw_os_error(101), // ENETUNREACH
            ErrorSpec::HostUnreachable => io::Error::from_raw_os_error(113),   // EHOSTUNREACH
            ErrorSpec::BrokenPipe => io::Error::from_raw_os_error(32),         // EPIPE
            ErrorSpec::ConnectionReset => io::Error::from_raw_os_error(104),   // ECONNRESET
            ErrorSpec::AddressInUse => io::Error::from_raw_os_error(98),       // EADDRINUSE
            ErrorSpec::PermissionDenied => io::Error::from_raw_os_error(13),   // EACCES
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

    /// Inject connection refused error on a specific attempt.
    pub fn with_connection_refused_on_attempt(mut self, attempt: usize) -> Self {
        self.errors_on_attempts
            .push((attempt, ErrorSpec::ConnectionRefused));
        self
    }

    /// Inject timeout error on a specific attempt.
    pub fn with_timed_out_on_attempt(mut self, attempt: usize) -> Self {
        self.errors_on_attempts.push((attempt, ErrorSpec::TimedOut));
        self
    }

    /// Inject network unreachable error on a specific attempt.
    #[allow(dead_code)]
    pub fn with_network_unreachable_on_attempt(mut self, attempt: usize) -> Self {
        self.errors_on_attempts
            .push((attempt, ErrorSpec::NetworkUnreachable));
        self
    }

    /// Inject host unreachable error on a specific attempt.
    #[allow(dead_code)]
    pub fn with_host_unreachable_on_attempt(mut self, attempt: usize) -> Self {
        self.errors_on_attempts
            .push((attempt, ErrorSpec::HostUnreachable));
        self
    }

    /// Inject broken pipe error on a specific attempt.
    pub fn with_broken_pipe_on_attempt(mut self, attempt: usize) -> Self {
        self.errors_on_attempts
            .push((attempt, ErrorSpec::BrokenPipe));
        self
    }

    /// Inject connection reset error on a specific attempt.
    #[allow(dead_code)]
    pub fn with_connection_reset_on_attempt(mut self, attempt: usize) -> Self {
        self.errors_on_attempts
            .push((attempt, ErrorSpec::ConnectionReset));
        self
    }

    /// Inject address in use error on a specific attempt.
    #[allow(dead_code)]
    pub fn with_address_in_use_on_attempt(mut self, attempt: usize) -> Self {
        self.errors_on_attempts
            .push((attempt, ErrorSpec::AddressInUse));
        self
    }

    /// Inject permission denied error on a specific attempt.
    #[allow(dead_code)]
    pub fn with_permission_denied_on_attempt(mut self, attempt: usize) -> Self {
        self.errors_on_attempts
            .push((attempt, ErrorSpec::PermissionDenied));
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

    /// Inject connection refused error on a specific attempt.
    pub fn with_connection_refused_on_attempt(self, attempt: usize) -> Self {
        self.with_error_on_attempt(attempt, ErrorSpec::ConnectionRefused)
    }

    /// Inject timeout error on a specific attempt.
    pub fn with_timed_out_on_attempt(self, attempt: usize) -> Self {
        self.with_error_on_attempt(attempt, ErrorSpec::TimedOut)
    }

    /// Inject network unreachable error on a specific attempt.
    pub fn with_network_unreachable_on_attempt(self, attempt: usize) -> Self {
        self.with_error_on_attempt(attempt, ErrorSpec::NetworkUnreachable)
    }

    /// Inject host unreachable error on a specific attempt.
    #[allow(dead_code)]
    pub fn with_host_unreachable_on_attempt(self, attempt: usize) -> Self {
        self.with_error_on_attempt(attempt, ErrorSpec::HostUnreachable)
    }

    /// Inject broken pipe error on a specific attempt.
    pub fn with_broken_pipe_on_attempt(self, attempt: usize) -> Self {
        self.with_error_on_attempt(attempt, ErrorSpec::BrokenPipe)
    }

    /// Inject connection reset error on a specific attempt.
    pub fn with_connection_reset_on_attempt(self, attempt: usize) -> Self {
        self.with_error_on_attempt(attempt, ErrorSpec::ConnectionReset)
    }

    /// Inject address in use error on a specific attempt.
    #[allow(dead_code)]
    pub fn with_address_in_use_on_attempt(self, attempt: usize) -> Self {
        self.with_error_on_attempt(attempt, ErrorSpec::AddressInUse)
    }

    /// Inject permission denied error on a specific attempt.
    pub fn with_permission_denied_on_attempt(self, attempt: usize) -> Self {
        self.with_error_on_attempt(attempt, ErrorSpec::PermissionDenied)
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
#[allow(dead_code)]
pub fn io_error(kind: io::ErrorKind, msg: &str) -> io::Error {
    io::Error::new(kind, msg)
}

/// Create a connection refused error (ECONNREFUSED, errno 111).
pub fn connection_refused_error() -> io::Error {
    io::Error::from_raw_os_error(111)
}

/// Create a timeout error (ETIMEDOUT, errno 110).
pub fn timed_out_error() -> io::Error {
    io::Error::from_raw_os_error(110)
}

/// Create a network unreachable error (ENETUNREACH, errno 101).
pub fn network_unreachable_error() -> io::Error {
    io::Error::from_raw_os_error(101)
}

/// Create a host unreachable error (EHOSTUNREACH, errno 113).
pub fn host_unreachable_error() -> io::Error {
    io::Error::from_raw_os_error(113)
}

/// Create a broken pipe error (EPIPE, errno 32).
pub fn broken_pipe_error() -> io::Error {
    io::Error::from_raw_os_error(32)
}

/// Create a connection reset error (ECONNRESET, errno 104).
pub fn connection_reset_error() -> io::Error {
    io::Error::from_raw_os_error(104)
}

/// Create an address in use error (EADDRINUSE, errno 98).
pub fn address_in_use_error() -> io::Error {
    io::Error::from_raw_os_error(98)
}

/// Create a permission denied error (EACCES, errno 13).
pub fn permission_denied_error() -> io::Error {
    io::Error::from_raw_os_error(13)
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

/// Assert that a retry result failed with connection refused error.
pub fn assert_failed_connection_refused(result: &RetryResult) -> Result<(), String> {
    if result.succeeded {
        return Err(format!(
            "Expected connection refused failure, but operation succeeded after {} attempts",
            result.attempts
        ));
    }
    let errno = result
        .error
        .as_ref()
        .and_then(|e| e.raw_os_error())
        .ok_or("Error has no raw OS error code")?;

    if errno != 111 {
        return Err(format!(
            "Expected connection refused (errno 111), but got errno {}",
            errno
        ));
    }
    Ok(())
}

/// Assert that a retry result failed with timeout error.
pub fn assert_failed_timed_out(result: &RetryResult) -> Result<(), String> {
    if result.succeeded {
        return Err(format!(
            "Expected timeout failure, but operation succeeded after {} attempts",
            result.attempts
        ));
    }
    let errno = result
        .error
        .as_ref()
        .and_then(|e| e.raw_os_error())
        .ok_or("Error has no raw OS error code")?;

    if errno != 110 {
        return Err(format!(
            "Expected timeout (errno 110), but got errno {}",
            errno
        ));
    }
    Ok(())
}

/// Assert that a retry result failed with network unreachable error.
pub fn assert_failed_network_unreachable(result: &RetryResult) -> Result<(), String> {
    if result.succeeded {
        return Err(format!(
            "Expected network unreachable failure, but operation succeeded after {} attempts",
            result.attempts
        ));
    }
    let errno = result
        .error
        .as_ref()
        .and_then(|e| e.raw_os_error())
        .ok_or("Error has no raw OS error code")?;

    if errno != 101 {
        return Err(format!(
            "Expected network unreachable (errno 101), but got errno {}",
            errno
        ));
    }
    Ok(())
}

/// Assert that a retry result failed with broken pipe error.
pub fn assert_failed_broken_pipe(result: &RetryResult) -> Result<(), String> {
    if result.succeeded {
        return Err(format!(
            "Expected broken pipe failure, but operation succeeded after {} attempts",
            result.attempts
        ));
    }
    let errno = result
        .error
        .as_ref()
        .and_then(|e| e.raw_os_error())
        .ok_or("Error has no raw OS error code")?;

    if errno != 32 {
        return Err(format!(
            "Expected broken pipe (errno 32), but got errno {}",
            errno
        ));
    }
    Ok(())
}

/// Assert that a retry result failed with connection reset error.
pub fn assert_failed_connection_reset(result: &RetryResult) -> Result<(), String> {
    if result.succeeded {
        return Err(format!(
            "Expected connection reset failure, but operation succeeded after {} attempts",
            result.attempts
        ));
    }
    let errno = result
        .error
        .as_ref()
        .and_then(|e| e.raw_os_error())
        .ok_or("Error has no raw OS error code")?;

    if errno != 104 {
        return Err(format!(
            "Expected connection reset (errno 104), but got errno {}",
            errno
        ));
    }
    Ok(())
}

/// Assert that a retry result failed with permission denied error.
pub fn assert_failed_permission_denied(result: &RetryResult) -> Result<(), String> {
    if result.succeeded {
        return Err(format!(
            "Expected permission denied failure, but operation succeeded after {} attempts",
            result.attempts
        ));
    }
    let errno = result
        .error
        .as_ref()
        .and_then(|e| e.raw_os_error())
        .ok_or("Error has no raw OS error code")?;

    if errno != 13 {
        return Err(format!(
            "Expected permission denied (errno 13), but got errno {}",
            errno
        ));
    }
    Ok(())
}

/// Check if an error is retryable (network errors that should trigger retry).
pub fn is_retryable_error(error: &io::Error) -> bool {
    // ETXTBSY is retryable (file busy)
    if error.raw_os_error() == Some(26) {
        return true;
    }
    // Connection refused might be temporary
    if error.raw_os_error() == Some(111) {
        return true;
    }
    // Timed out is retryable
    if error.raw_os_error() == Some(110) {
        return true;
    }
    // Network unreachable might be temporary
    if error.raw_os_error() == Some(101) {
        return true;
    }
    // Connection reset by peer is retryable
    if error.raw_os_error() == Some(104) {
        return true;
    }
    false
}

// ──────────────────────────────────────────────────────────────────────────────
// Predefined retry configurations
// ──────────────────────────────────────────────────────────────────────────────

/// Fast-fail configuration: minimal retries, short backoff for quick failure detection.
pub fn fast_fail_config() -> RetryConfig {
    RetryConfig::new().with_max_attempts(2).with_backoff_ms(10)
}

/// Long-retry configuration: many retries with exponential backoff for resilient operations.
pub fn long_retry_config() -> RetryConfig {
    RetryConfig::new()
        .with_max_attempts(10)
        .with_exponential_backoff(50, 5000)
}

/// Aggressive retry configuration: frequent retries with minimal backoff.
pub fn aggressive_retry_config() -> RetryConfig {
    RetryConfig::new().with_max_attempts(20).with_backoff_ms(5)
}

/// Conservative retry configuration: few retries with long delays.
pub fn conservative_retry_config() -> RetryConfig {
    RetryConfig::new().with_max_attempts(3).with_backoff_ms(500)
}

/// Test configuration: minimal delays for fast test execution.
pub fn test_fast_config() -> RetryConfig {
    RetryConfig::new().with_max_attempts(3).with_backoff_ms(1)
}

/// No retry configuration: single attempt only.
pub fn no_retry_config() -> RetryConfig {
    RetryConfig::new().with_max_attempts(1).with_backoff_ms(0)
}

/// Production-like configuration: balanced retries for real workloads.
pub fn production_config() -> RetryConfig {
    RetryConfig::new()
        .with_max_attempts(5)
        .with_exponential_backoff(100, 1000)
}

/// Network operation configuration: retries for transient network failures.
pub fn network_retry_config() -> RetryConfig {
    RetryConfig::new()
        .with_max_attempts(7)
        .with_exponential_backoff(200, 10000)
}

/// Binary busy configuration: retries for ETXTBSY (binary busy) scenarios.
pub fn binary_busy_config() -> RetryConfig {
    RetryConfig::new().with_max_attempts(8).with_backoff_ms(20)
}

/// CI poll configuration: optimized for CI reconciliation polling.
pub fn ci_poll_config() -> RetryConfig {
    RetryConfig::new()
        .with_max_attempts(6)
        .with_exponential_backoff(30, 300)
}

// ──────────────────────────────────────────────────────────────────────────────
// Predefined mock retry behaviors
// ──────────────────────────────────────────────────────────────────────────────

/// Create a mock retry behavior with fast-fail configuration.
pub fn fast_fail_mock() -> MockRetryBehavior {
    MockRetryBehavior::new().with_config(fast_fail_config())
}

/// Create a mock retry behavior with long-retry configuration.
pub fn long_retry_mock() -> MockRetryBehavior {
    MockRetryBehavior::new().with_config(long_retry_config())
}

/// Create a mock retry behavior with aggressive retry configuration.
pub fn aggressive_retry_mock() -> MockRetryBehavior {
    MockRetryBehavior::new().with_config(aggressive_retry_config())
}

/// Create a mock retry behavior with conservative retry configuration.
#[allow(dead_code)]
pub fn conservative_retry_mock() -> MockRetryBehavior {
    MockRetryBehavior::new().with_config(conservative_retry_config())
}

/// Create a mock retry behavior optimized for fast tests.
pub fn test_fast_mock() -> MockRetryBehavior {
    MockRetryBehavior::new().with_config(test_fast_config())
}

/// Create a mock retry behavior with no retry (single attempt).
pub fn no_retry_mock() -> MockRetryBehavior {
    MockRetryBehavior::new().with_config(no_retry_config())
}

/// Create a mock retry behavior with production-like configuration.
pub fn production_mock() -> MockRetryBehavior {
    MockRetryBehavior::new().with_config(production_config())
}

/// Create a mock retry behavior for network operations.
pub fn network_retry_mock() -> MockRetryBehavior {
    MockRetryBehavior::new().with_config(network_retry_config())
}

/// Create a mock retry behavior for binary busy scenarios.
pub fn binary_busy_mock() -> MockRetryBehavior {
    MockRetryBehavior::new().with_config(binary_busy_config())
}

/// Create a mock retry behavior for CI polling scenarios.
pub fn ci_poll_mock() -> MockRetryBehavior {
    MockRetryBehavior::new().with_config(ci_poll_config())
}

// ──────────────────────────────────────────────────────────────────────────────
// Common retry scenario fixtures
// ──────────────────────────────────────────────────────────────────────────────

/// Scenario: Single transient ETXTBSY error, then success.
pub fn scenario_single_etxtbsy_then_success() -> MockRetryBehavior {
    binary_busy_mock().with_etxtbsy_on_attempt(1)
}

/// Scenario: Multiple transient ETXTBSY errors, then success.
pub fn scenario_multiple_etxtbsy_then_success() -> MockRetryBehavior {
    binary_busy_mock()
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2)
        .with_etxtbsy_on_attempt(3)
}

/// Scenario: ETXTBSY errors exhaust retries.
pub fn scenario_etxtbsy_exhausted() -> MockRetryBehavior {
    binary_busy_mock()
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2)
        .with_etxtbsy_on_attempt(3)
        .with_etxtbsy_on_attempt(4)
        .with_etxtbsy_on_attempt(5)
        .with_etxtbsy_on_attempt(6)
        .with_etxtbsy_on_attempt(7)
        .with_etxtbsy_on_attempt(8)
}

/// Scenario: Connection refused error, then success.
pub fn scenario_connection_refused_then_success() -> MockRetryBehavior {
    network_retry_mock().with_connection_refused_on_attempt(1)
}

/// Scenario: Timeout error, then success.
pub fn scenario_timeout_then_success() -> MockRetryBehavior {
    network_retry_mock().with_timed_out_on_attempt(1)
}

/// Scenario: Multiple network errors, then success.
pub fn scenario_multiple_network_errors_then_success() -> MockRetryBehavior {
    network_retry_mock()
        .with_connection_refused_on_attempt(1)
        .with_timed_out_on_attempt(2)
}

/// Scenario: Immediate success (no errors).
pub fn scenario_immediate_success() -> MockRetryBehavior {
    test_fast_mock()
}

/// Scenario: Non-retryable error fails immediately.
pub fn scenario_non_retryable_error() -> MockRetryBehavior {
    fast_fail_mock().with_permission_denied_on_attempt(1)
}

/// Scenario: Exponential backoff with multiple retries.
pub fn scenario_exponential_backoff_multiple_retries() -> MockRetryBehavior {
    production_mock()
        .with_connection_refused_on_attempt(1)
        .with_connection_refused_on_attempt(2)
        .with_connection_refused_on_attempt(3)
}

// ──────────────────────────────────────────────────────────────────────────────
// CI-specific retry configuration fixtures
// ──────────────────────────────────────────────────────────────────────────────

/// CI retry configuration matching default PostPushCiConfig settings.
pub fn ci_default_retry_config() -> RetryConfig {
    RetryConfig::new()
        .with_max_attempts(5)
        .with_exponential_backoff(30, 300)
}

/// CI retry configuration for quick-fail environments (development).
pub fn ci_dev_retry_config() -> RetryConfig {
    RetryConfig::new().with_max_attempts(2).with_backoff_ms(50)
}

/// CI retry configuration for patient environments (production).
pub fn ci_prod_retry_config() -> RetryConfig {
    RetryConfig::new()
        .with_max_attempts(10)
        .with_exponential_backoff(60, 600)
}

/// CI retry configuration for high-latency environments.
pub fn ci_high_latency_retry_config() -> RetryConfig {
    RetryConfig::new()
        .with_max_attempts(15)
        .with_exponential_backoff(100, 2000)
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

    #[test]
    fn test_network_error_constructors() {
        // Test that all network error constructors produce valid errors
        assert!(connection_refused_error().raw_os_error() == Some(111));
        assert!(timed_out_error().raw_os_error() == Some(110));
        assert!(network_unreachable_error().raw_os_error() == Some(101));
        assert!(host_unreachable_error().raw_os_error() == Some(113));
        assert!(broken_pipe_error().raw_os_error() == Some(32));
        assert!(connection_reset_error().raw_os_error() == Some(104));
        assert!(address_in_use_error().raw_os_error() == Some(98));
        assert!(permission_denied_error().raw_os_error() == Some(13));
    }

    #[test]
    fn test_error_spec_to_error_conversion() {
        // Test that all ErrorSpec variants convert to io::Error correctly
        let specs = vec![
            ErrorSpec::Etxtbsy,
            ErrorSpec::ConnectionRefused,
            ErrorSpec::TimedOut,
            ErrorSpec::NetworkUnreachable,
            ErrorSpec::HostUnreachable,
            ErrorSpec::BrokenPipe,
            ErrorSpec::ConnectionReset,
            ErrorSpec::AddressInUse,
            ErrorSpec::PermissionDenied,
            ErrorSpec::Io(io::ErrorKind::NotFound, "test".to_string()),
        ];

        for spec in specs {
            let error = spec.to_error();
            // Verify the error is valid and has appropriate metadata
            match spec {
                ErrorSpec::Io(kind, _) => {
                    assert_eq!(error.kind(), kind);
                }
                _ => {
                    // OS errors should have a raw OS error code
                    assert!(error.raw_os_error().is_some());
                }
            }
        }
    }

    #[test]
    fn test_connection_refused_retry() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(3)
            .with_connection_refused_on_attempt(1)
            .with_success_on_attempts(vec![2]);

        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 2)?;
        Ok(())
    }

    #[test]
    fn test_timeout_error_retry() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(3)
            .with_timed_out_on_attempt(1)
            .with_success_on_attempts(vec![2]);

        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 2)?;
        Ok(())
    }

    #[test]
    fn test_network_unreachable_retry() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(3)
            .with_network_unreachable_on_attempt(1)
            .with_success_on_attempts(vec![2]);

        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 2)?;
        Ok(())
    }

    #[test]
    fn test_connection_reset_retry() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(3)
            .with_connection_reset_on_attempt(1)
            .with_success_on_attempts(vec![2]);

        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 2)?;
        Ok(())
    }

    #[test]
    fn test_broken_pipe_fails_immediately() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(5)
            .with_broken_pipe_on_attempt(1);

        let result = mock.run_sync()?;

        assert_failed_broken_pipe(&result)?;
        assert_eq!(result.attempts, 1); // Should not retry
        Ok(())
    }

    #[test]
    fn test_permission_denied_fails_immediately() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(5)
            .with_permission_denied_on_attempt(1);

        let result = mock.run_sync()?;

        assert_failed_permission_denied(&result)?;
        assert_eq!(result.attempts, 1); // Should not retry
        Ok(())
    }

    #[test]
    fn test_mixed_error_injection() -> Result<(), String> {
        // Test injecting different errors on different attempts
        let mock = MockRetryBehavior::new()
            .with_max_attempts(5)
            .with_timed_out_on_attempt(1)
            .with_connection_refused_on_attempt(2)
            .with_success_on_attempts(vec![3]);

        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 3)?;
        Ok(())
    }

    #[test]
    fn test_exponential_backoff_with_timeouts() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(4)
            .with_exponential_backoff(10, 100)
            .with_timed_out_on_attempt(1)
            .with_timed_out_on_attempt(2)
            .with_success_on_attempts(vec![3]);

        let start = Instant::now();
        let result = mock.run_sync()?;
        let elapsed = start.elapsed();

        assert_succeeded_with_attempts(&result, 3)?;
        // With exponential backoff: 10ms (attempt 1) + 20ms (attempt 2) = at least 30ms
        assert!(elapsed >= Duration::from_millis(30));
        Ok(())
    }

    #[test]
    fn test_is_retryable_error() {
        // Test retryable errors
        assert!(is_retryable_error(&etxtbsy_error()));
        assert!(is_retryable_error(&connection_refused_error()));
        assert!(is_retryable_error(&timed_out_error()));
        assert!(is_retryable_error(&network_unreachable_error()));
        assert!(is_retryable_error(&connection_reset_error()));

        // Test non-retryable errors
        assert!(!is_retryable_error(&broken_pipe_error()));
        assert!(!is_retryable_error(&permission_denied_error()));
        assert!(!is_retryable_error(&address_in_use_error()));
        assert!(!is_retryable_error(&host_unreachable_error()));
        assert!(!is_retryable_error(&io_error(
            io::ErrorKind::NotFound,
            "not found"
        )));
    }

    #[tokio::test]
    async fn test_network_error_async_retry() -> Result<(), String> {
        let mock = MockRetryBehavior::new()
            .with_max_attempts(3)
            .with_connection_refused_on_attempt(1)
            .with_success_on_attempts(vec![2]);

        let result = mock.run_async().await?;

        assert_succeeded_with_attempts(&result, 2)?;
        Ok(())
    }

    #[test]
    fn test_error_injection_multiple_errors() {
        let injection = ErrorInjection::new()
            .with_etxtbsy_on_attempt(1)
            .with_timed_out_on_attempt(2)
            .with_connection_refused_on_attempt(3)
            .with_broken_pipe_on_attempt(4);

        assert!(injection.should_error(1).is_some());
        assert!(injection.should_error(2).is_some());
        assert!(injection.should_error(3).is_some());
        assert!(injection.should_error(4).is_some());
        assert!(injection.should_error(5).is_none());
    }

    #[test]
    fn test_mock_retry_all_assertions() -> Result<(), String> {
        // Test all new assertion helpers
        let mut result = RetryResult {
            attempts: 2,
            succeeded: false,
            elapsed: Duration::from_millis(100),
            result: None,
            error: Some(connection_refused_error()),
        };

        assert_failed_connection_refused(&result)?;

        result.error = Some(timed_out_error());
        assert_failed_timed_out(&result)?;

        result.error = Some(network_unreachable_error());
        assert_failed_network_unreachable(&result)?;

        result.error = Some(broken_pipe_error());
        assert_failed_broken_pipe(&result)?;

        result.error = Some(connection_reset_error());
        assert_failed_connection_reset(&result)?;

        result.error = Some(permission_denied_error());
        assert_failed_permission_denied(&result)?;

        Ok(())
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tests for predefined configurations
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_fast_fail_config() {
        let config = fast_fail_config();
        assert_eq!(config.max_attempts, 2);
        assert_eq!(config.backoff_ms, 10);
        assert!(!config.exponential_backoff);
    }

    #[test]
    fn test_long_retry_config() {
        let config = long_retry_config();
        assert_eq!(config.max_attempts, 10);
        assert!(config.exponential_backoff);
        assert_eq!(config.exponential_initial_ms, 50);
        assert_eq!(config.exponential_max_ms, 5000);
    }

    #[test]
    fn test_aggressive_retry_config() {
        let config = aggressive_retry_config();
        assert_eq!(config.max_attempts, 20);
        assert_eq!(config.backoff_ms, 5);
        assert!(!config.exponential_backoff);
    }

    #[test]
    fn test_conservative_retry_config() {
        let config = conservative_retry_config();
        assert_eq!(config.max_attempts, 3);
        assert_eq!(config.backoff_ms, 500);
        assert!(!config.exponential_backoff);
    }

    #[test]
    fn test_test_fast_config() {
        let config = test_fast_config();
        assert_eq!(config.max_attempts, 3);
        assert_eq!(config.backoff_ms, 1);
        assert!(!config.exponential_backoff);
    }

    #[test]
    fn test_no_retry_config() {
        let config = no_retry_config();
        assert_eq!(config.max_attempts, 1);
        assert_eq!(config.backoff_ms, 0);
        assert!(!config.exponential_backoff);
    }

    #[test]
    fn test_production_config() {
        let config = production_config();
        assert_eq!(config.max_attempts, 5);
        assert!(config.exponential_backoff);
        assert_eq!(config.exponential_initial_ms, 100);
        assert_eq!(config.exponential_max_ms, 1000);
    }

    #[test]
    fn test_network_retry_config() {
        let config = network_retry_config();
        assert_eq!(config.max_attempts, 7);
        assert!(config.exponential_backoff);
        assert_eq!(config.exponential_initial_ms, 200);
        assert_eq!(config.exponential_max_ms, 10000);
    }

    #[test]
    fn test_binary_busy_config() {
        let config = binary_busy_config();
        assert_eq!(config.max_attempts, 8);
        assert_eq!(config.backoff_ms, 20);
        assert!(!config.exponential_backoff);
    }

    #[test]
    fn test_ci_poll_config() {
        let config = ci_poll_config();
        assert_eq!(config.max_attempts, 6);
        assert!(config.exponential_backoff);
        assert_eq!(config.exponential_initial_ms, 30);
        assert_eq!(config.exponential_max_ms, 300);
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tests for predefined mock behaviors
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_fast_fail_mock() {
        let mock = fast_fail_mock();
        assert_eq!(mock.config.max_attempts, 2);
        assert_eq!(mock.config.backoff_ms, 10);
    }

    #[test]
    fn test_long_retry_mock() {
        let mock = long_retry_mock();
        assert_eq!(mock.config.max_attempts, 10);
        assert!(mock.config.exponential_backoff);
    }

    #[test]
    fn test_aggressive_retry_mock() {
        let mock = aggressive_retry_mock();
        assert_eq!(mock.config.max_attempts, 20);
        assert_eq!(mock.config.backoff_ms, 5);
    }

    #[test]
    fn test_test_fast_mock() {
        let mock = test_fast_mock();
        assert_eq!(mock.config.max_attempts, 3);
        assert_eq!(mock.config.backoff_ms, 1);
    }

    #[test]
    fn test_no_retry_mock() {
        let mock = no_retry_mock();
        assert_eq!(mock.config.max_attempts, 1);
        assert_eq!(mock.config.backoff_ms, 0);
    }

    #[test]
    fn test_production_mock() {
        let mock = production_mock();
        assert_eq!(mock.config.max_attempts, 5);
        assert!(mock.config.exponential_backoff);
    }

    #[test]
    fn test_network_retry_mock() {
        let mock = network_retry_mock();
        assert_eq!(mock.config.max_attempts, 7);
        assert!(mock.config.exponential_backoff);
    }

    #[test]
    fn test_binary_busy_mock() {
        let mock = binary_busy_mock();
        assert_eq!(mock.config.max_attempts, 8);
        assert_eq!(mock.config.backoff_ms, 20);
    }

    #[test]
    fn test_ci_poll_mock() {
        let mock = ci_poll_mock();
        assert_eq!(mock.config.max_attempts, 6);
        assert!(mock.config.exponential_backoff);
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tests for predefined scenarios
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_scenario_single_etxtbsy_then_success() -> Result<(), String> {
        let mock = scenario_single_etxtbsy_then_success();
        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 2)?;
        Ok(())
    }

    #[test]
    fn test_scenario_multiple_etxtbsy_then_success() -> Result<(), String> {
        let mock = scenario_multiple_etxtbsy_then_success();
        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 4)?;
        Ok(())
    }

    #[test]
    fn test_scenario_etxtbsy_exhausted() -> Result<(), String> {
        let mock = scenario_etxtbsy_exhausted();
        let result = mock.run_sync()?;

        assert_failed_etxtbsy(&result)?;
        assert_eq!(result.attempts, 8);
        Ok(())
    }

    #[test]
    fn test_scenario_connection_refused_then_success() -> Result<(), String> {
        let mock = scenario_connection_refused_then_success();
        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 2)?;
        Ok(())
    }

    #[test]
    fn test_scenario_timeout_then_success() -> Result<(), String> {
        let mock = scenario_timeout_then_success();
        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 2)?;
        Ok(())
    }

    #[test]
    fn test_scenario_multiple_network_errors_then_success() -> Result<(), String> {
        let mock = scenario_multiple_network_errors_then_success();
        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 3)?;
        Ok(())
    }

    #[test]
    fn test_scenario_immediate_success() -> Result<(), String> {
        let mock = scenario_immediate_success();
        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 1)?;
        assert!(result.elapsed.as_millis() < 10);
        Ok(())
    }

    #[test]
    fn test_scenario_non_retryable_error() -> Result<(), String> {
        let mock = scenario_non_retryable_error();
        let result = mock.run_sync()?;

        assert_failed_permission_denied(&result)?;
        assert_eq!(result.attempts, 1);
        Ok(())
    }

    #[test]
    fn test_scenario_exponential_backoff_multiple_retries() -> Result<(), String> {
        let mock = scenario_exponential_backoff_multiple_retries();
        let result = mock.run_sync()?;

        assert_succeeded_with_attempts(&result, 4)?;
        // Should have exponential backoff delays: 100ms + 200ms + 400ms = 700ms minimum
        assert!(result.elapsed.as_millis() >= 700);
        Ok(())
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tests for CI-specific configurations
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_ci_default_retry_config() {
        let config = ci_default_retry_config();
        assert_eq!(config.max_attempts, 5);
        assert!(config.exponential_backoff);
        assert_eq!(config.exponential_initial_ms, 30);
        assert_eq!(config.exponential_max_ms, 300);
    }

    #[test]
    fn test_ci_dev_retry_config() {
        let config = ci_dev_retry_config();
        assert_eq!(config.max_attempts, 2);
        assert_eq!(config.backoff_ms, 50);
        assert!(!config.exponential_backoff);
    }

    #[test]
    fn test_ci_prod_retry_config() {
        let config = ci_prod_retry_config();
        assert_eq!(config.max_attempts, 10);
        assert!(config.exponential_backoff);
        assert_eq!(config.exponential_initial_ms, 60);
        assert_eq!(config.exponential_max_ms, 600);
    }

    #[test]
    fn test_ci_high_latency_retry_config() {
        let config = ci_high_latency_retry_config();
        assert_eq!(config.max_attempts, 15);
        assert!(config.exponential_backoff);
        assert_eq!(config.exponential_initial_ms, 100);
        assert_eq!(config.exponential_max_ms, 2000);
    }
}
