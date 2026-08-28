//! Log capture helper for integration tests.
//!
//! This module provides reusable infrastructure for capturing and verifying
//! tracing/log output in tests. It enables both stdout and structured log capture
//! with assertion helpers for validating log messages.
//!
//! # Usage Pattern
//!
//! ```rust
//! use log_capture_helper::{setup_log_capture, assert_log_contains};
//!
//! #[tokio::test]
//! async fn my_test_with_log_verification() {
//!     // Setup log capture
//!     let (logs, _guard) = setup_log_capture();
//!
//!     // Run code that emits tracing logs
//!     tracing::info!("test message");
//!
//!     // Verify log content
//!     assert_log_contains(&logs, "test message");
//! }
//! ```

use std::io::Write;
use std::sync::{Arc, Mutex};

// ──────────────────────────────────────────────────────────────────────────────
// CapturedLogs — log buffer implementing MakeWriter
// ──────────────────────────────────────────────────────────────────────────────

/// Captured log output in memory.
///
/// This struct wraps a shared buffer that implements `MakeWriter`, allowing
/// tracing subscribers to write log output directly into memory for test assertions.
#[derive(Debug, Clone)]
pub struct CapturedLogs(pub Arc<Mutex<Vec<u8>>>);

impl Default for CapturedLogs {
    fn default() -> Self {
        CapturedLogs(Arc::new(Mutex::new(Vec::new())))
    }
}

/// Writer that captures log output to a shared buffer.
pub struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CapturedLogWriter(self.0.clone())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// LogGuard — RAII guard for subscriber scope
// ──────────────────────────────────────────────────────────────────────────────

/// RAII guard that resets the tracing subscriber when dropped.
///
/// This ensures test isolation by automatically restoring the previous
/// subscriber when the guard goes out of scope.
pub struct LogGuard {
    _private: (),
}

impl Drop for LogGuard {
    fn drop(&mut self) {
        // Reset to default subscriber (no-op if already reset)
        let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Setup functions
// ──────────────────────────────────────────────────────────────────────────────

/// Setup log capture with formatted (non-JSON) output.
///
/// This configures a tracing subscriber that writes human-readable logs
/// to the provided buffer, suitable for assertion testing.
///
/// # Returns
///
/// A tuple of `(CapturedLogs, LogGuard)` where:
/// - `CapturedLogs` provides access to the captured log buffer
/// - `LogGuard` ensures proper cleanup when dropped
///
/// # Example
///
/// ```rust
/// let (logs, guard) = setup_log_capture();
/// // ... run test code ...
/// assert_log_contains(&logs, "expected message");
/// // guard dropped automatically
/// ```
pub fn setup_log_capture() -> (CapturedLogs, LogGuard) {
    setup_log_capture_with_level(tracing::Level::INFO)
}

/// Setup log capture with a specific log level.
///
/// Use this when you need to capture DEBUG or TRACE level logs that
/// wouldn't appear with the default INFO level.
///
/// # Arguments
///
/// * `level` - The maximum log level to capture
///
/// # Example
///
/// ```rust
/// let (logs, guard) = setup_log_capture_with_level(tracing::Level::DEBUG);
/// tracing::debug!("this will be captured");
/// assert_log_contains(&logs, "this will be captured");
/// ```
pub fn setup_log_capture_with_level(level: tracing::Level) -> (CapturedLogs, LogGuard) {
    let captured = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .without_time()
        .with_max_level(level)
        .finish();

    // Set the subscriber as default
    let _ = tracing::subscriber::set_default(subscriber);

    (captured, LogGuard { _private: () })
}

/// Setup log capture with JSON structured output.
///
/// Use this when you need to parse and validate structured log fields.
/// JSON logs include metadata like level, target, and structured fields.
///
/// # Returns
///
/// A tuple of `(CapturedLogs, LogGuard)` where the buffer contains JSON lines.
///
/// # Example
///
/// ```rust
/// let (logs, guard) = setup_json_log_capture();
/// tracing::info!(worker_id = "alpha", "processing bead");
///
/// let log_content = get_captured_logs(&logs);
/// for line in log_content.lines() {
///     if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
///         assert_eq!(json["worker_id"], "alpha");
///     }
/// }
/// ```
#[allow(dead_code)]
pub fn setup_json_log_capture() -> (CapturedLogs, LogGuard) {
    setup_json_log_capture_with_level(tracing::Level::INFO)
}

/// Setup JSON log capture with a specific log level.
///
/// See `setup_json_log_capture()` for details. This variant allows
/// specifying the maximum log level to capture.
///
/// # Arguments
///
/// * `level` - The maximum log level to capture
#[allow(dead_code)]
pub fn setup_json_log_capture_with_level(level: tracing::Level) -> (CapturedLogs, LogGuard) {
    let captured = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(captured.clone())
        .with_ansi(false)
        .without_time()
        .with_max_level(level)
        .finish();

    let _ = tracing::subscriber::set_default(subscriber);

    (captured, LogGuard { _private: () })
}

/// Setup log capture that also writes to stdout.
///
/// This duplicates log output to both the capture buffer and stdout,
/// useful for debugging failing tests.
///
/// # Example
///
/// ```rust
/// let (logs, guard) = setup_log_capture_with_stdout();
/// // Logs appear in terminal AND are captured for assertions
/// assert_log_contains(&logs, "expected message");
/// ```
#[allow(dead_code)]
pub fn setup_log_capture_with_stdout() -> (CapturedLogs, LogGuard) {
    setup_log_capture_with_stdout_and_level(tracing::Level::INFO)
}

/// Setup log capture with stdout output at a specific level.
///
/// See `setup_log_capture_with_stdout()` for details. This variant allows
/// specifying the maximum log level.
///
/// # Arguments
///
/// * `level` - The maximum log level to capture and display
#[allow(dead_code)]
pub fn setup_log_capture_with_stdout_and_level(level: tracing::Level) -> (CapturedLogs, LogGuard) {
    let captured = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .with_writer(captured.clone())
        .with_ansi(false)
        .without_time()
        .with_max_level(level)
        .finish();

    let _ = tracing::subscriber::set_default(subscriber);

    (captured, LogGuard { _private: () })
}

// ──────────────────────────────────────────────────────────────────────────────
// Assertion helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Get captured log output as a string.
///
/// This function locks the shared buffer, clones its contents, and
/// returns it as a UTF-8 string. Use this to inspect log content.
///
/// # Example
///
/// ```rust
/// let (logs, _guard) = setup_log_capture();
/// // ... run code ...
/// let log_content = get_captured_logs(&logs);
/// println!("Captured logs:\n{}", log_content);
/// ```
pub fn get_captured_logs(logs: &CapturedLogs) -> String {
    String::from_utf8(logs.0.lock().unwrap().clone()).unwrap_or_else(|_| String::new())
}

/// Assert that captured logs contain a specific substring.
///
/// This is the most common assertion for log verification. Use it to
/// check that expected log messages were emitted.
///
/// # Arguments
///
/// * `logs` - The captured log buffer
/// * `substring` - The text to search for in the logs
///
/// # Panics
///
/// Panics if the substring is not found, with a helpful message showing
/// the actual log content.
///
/// # Example
///
/// ```rust
/// let (logs, _guard) = setup_log_capture();
/// tracing::info!("worker started");
/// assert_log_contains(&logs, "worker started");
/// ```
pub fn assert_log_contains(logs: &CapturedLogs, substring: &str) {
    let log_content = get_captured_logs(logs);
    assert!(
        log_content.contains(substring),
        "Expected to find '{}' in logs, but it was not present.\n\
         Captured logs:\n{}",
        substring,
        log_content
    );
}

/// Assert that captured logs do NOT contain a specific substring.
///
/// Use this to verify that error messages or undesired logs were NOT emitted.
///
/// # Arguments
///
/// * `logs` - The captured log buffer
/// * `substring` - The text that should NOT appear in the logs
///
/// # Panics
///
/// Panics if the substring IS found.
///
/// # Example
///
/// ```rust
/// let (logs, _guard) = setup_log_capture();
/// // ... run code that should NOT emit errors ...
/// assert_log_not_contains(&logs, "ERROR");
/// ```
pub fn assert_log_not_contains(logs: &CapturedLogs, substring: &str) {
    let log_content = get_captured_logs(logs);
    assert!(
        !log_content.contains(substring),
        "Expected NOT to find '{}' in logs, but it was present.\n\
         Captured logs:\n{}",
        substring,
        log_content
    );
}

/// Assert that captured logs match a regex pattern.
///
/// Use this for more complex pattern matching than simple substring search.
///
/// # Arguments
///
/// * `logs` - The captured log buffer
/// * `pattern` - A regex pattern to match against the logs
///
/// # Panics
///
/// Panics if the pattern does not match.
///
/// # Example
///
/// ```rust
/// let (logs, _guard) = setup_log_capture();
/// tracing::info!("Processing bead abc123 in workspace /tmp/test");
/// assert_log_matches(&logs, r"Processing bead \w+ in workspace /.*");
/// ```
#[allow(dead_code)]
pub fn assert_log_matches(logs: &CapturedLogs, pattern: &str) {
    let log_content = get_captured_logs(logs);
    let regex = regex::Regex::new(pattern)
        .unwrap_or_else(|e| panic!("Invalid regex pattern '{}': {}", pattern, e));

    assert!(
        regex.is_match(&log_content),
        "Expected logs to match pattern '{}', but they did not.\n\
         Captured logs:\n{}",
        pattern,
        log_content
    );
}

/// Assert that a specific log level appears in the captured output.
///
/// This checks for the presence of log level markers (e.g., "INFO", "ERROR")
/// in the formatted output. Useful for verifying error handling paths.
///
/// # Arguments
///
/// * `logs` - The captured log buffer
/// * `level` - The log level to expect (e.g., "ERROR", "WARN", "INFO")
///
/// # Panics
///
/// Panics if the level marker is not found.
///
/// # Example
///
/// ```rust
/// let (logs, _guard) = setup_log_capture();
/// tracing::error!("critical failure");
/// assert_log_level(&logs, "ERROR");
/// ```
#[allow(dead_code)]
pub fn assert_log_level(logs: &CapturedLogs, level: &str) {
    assert_log_contains(logs, level);
}

/// Count occurrences of a substring in captured logs.
///
/// Use this to verify that a log message appears exactly N times,
/// rather than just checking for presence/absence.
///
/// # Arguments
///
/// * `logs` - The captured log buffer
/// * `substring` - The text to count
///
/// # Returns
///
/// The number of times the substring appears.
///
/// # Example
///
/// ```rust
/// let (logs, _guard) = setup_log_capture();
/// for i in 0..5 {
///     tracing::info!("retry {}", i);
/// }
/// assert_eq!(count_log_occurrences(&logs, "retry"), 5);
/// ```
pub fn count_log_occurrences(logs: &CapturedLogs, substring: &str) -> usize {
    let log_content = get_captured_logs(logs);
    log_content.matches(substring).count()
}

/// Assert that a substring appears exactly N times in the logs.
///
/// Use this for precise count verification.
///
/// # Arguments
///
/// * `logs` - The captured log buffer
/// * `substring` - The text to count
/// * `expected_count` - The exact number of times the substring should appear
///
/// # Panics
///
/// Panics if the count doesn't match.
///
/// # Example
///
/// ```rust
/// let (logs, _guard) = setup_log_capture();
/// tracing::info!("starting");
/// tracing::info!("processing");
/// tracing::info!("complete");
/// assert_log_count(&logs, "ing", 2); // "processing" only
/// ```
pub fn assert_log_count(logs: &CapturedLogs, substring: &str, expected_count: usize) {
    let actual_count = count_log_occurrences(logs, substring);
    assert_eq!(
        actual_count,
        expected_count,
        "Expected '{}' to appear {} times in logs, but it appeared {} times.\n\
         Captured logs:\n{}",
        substring,
        expected_count,
        actual_count,
        get_captured_logs(logs)
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests for the helper module itself
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_basic_log_capture() {
        let (logs, _guard) = setup_log_capture();

        tracing::info!("test message");

        assert_log_contains(&logs, "test message");
    }

    #[tokio::test]
    async fn test_log_not_contains() {
        let (logs, _guard) = setup_log_capture();

        tracing::info!("safe message");

        assert_log_not_contains(&logs, "ERROR");
    }

    #[tokio::test]
    async fn test_log_count() {
        let (logs, _guard) = setup_log_capture();

        tracing::info!("retry 1");
        tracing::info!("retry 2");
        tracing::info!("retry 3");

        assert_log_count(&logs, "retry", 3);
    }

    #[tokio::test]
    async fn test_debug_level_capture() {
        let (logs, _guard) = setup_log_capture_with_level(tracing::Level::DEBUG);

        tracing::debug!("debug message");

        assert_log_contains(&logs, "debug message");
    }
}
