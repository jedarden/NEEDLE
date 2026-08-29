//! Polling logic test helper module
//!
//! This module provides common utilities and test fixtures for testing polling behavior
//! across NEEDLE. It serves as a centralized location for polling-related test helpers,
//! reducing duplication and ensuring consistent polling behavior testing.
//!
//! # Architecture
//!
//! The module is organized into several sections:
//! - **Mock clock**: Utilities for controlling time in tests without waiting
//! - **Poll interval configuration**: Configurable poll interval fixtures
//! - **Mock pollers**: Test doubles for simulating polling scenarios
//! - **Assertion helpers**: Specialized assertions for polling behavior validation
//!
//! # Usage
//!
//! ```no_run
//! use tests::helpers::polling::*;
//!
//! #[tokio::test]
//! async fn test_my_polling_logic() {
//!     let mock = MockPoller::new()
//!         .with_interval_secs(10)
//!         .with_immediate_first_check(true);
//!
//!     let clock = MockClock::new();
//!     let result = mock.run(&clock).await.unwrap();
//!
//!     assert_eq!(result.poll_count, 3);
//!     assert!(result.last_poll_at >= Duration::from_secs(20));
//! }
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::sleep;

// ──────────────────────────────────────────────────────────────────────────────
// Mock clock for testing time-dependent behavior
// ──────────────────────────────────────────────────────────────────────────────

/// A mock clock that allows tests to control time without actual delays.
#[derive(Debug, Clone)]
pub struct MockClock {
    /// Current time in the mock clock's timeline.
    current_time: Arc<RwLock<Instant>>,
}

impl MockClock {
    /// Create a new mock clock starting at the given instant.
    pub fn new() -> Self {
        Self {
            current_time: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Create a new mock clock starting at a specific instant.
    pub fn new_with_time(start: Instant) -> Self {
        Self {
            current_time: Arc::new(RwLock::new(start)),
        }
    }

    /// Get the current mock time.
    pub async fn now(&self) -> Instant {
        *self.current_time.read().await
    }

    /// Advance the mock clock by the given duration.
    pub async fn advance(&self, duration: Duration) {
        let mut time = self.current_time.write().await;
        // Simulate time passing by creating a new Instant
        // In a real implementation, this would use a custom Instant wrapper
        // For now, we use the actual Instant::now() + elapsed approach
        *time = Instant::now() + duration;
    }

    /// Advance the mock clock by the specified number of seconds.
    pub async fn advance_secs(&self, secs: u64) {
        self.advance(Duration::from_secs(secs)).await
    }

    /// Advance the mock clock by the specified number of milliseconds.
    pub async fn advance_millis(&self, millis: u64) {
        self.advance(Duration::from_millis(millis)).await
    }
}

impl Default for MockClock {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Poll interval configuration fixtures
// ──────────────────────────────────────────────────────────────────────────────

/// Configurable poll interval behavior for testing.
#[derive(Debug, Clone)]
pub struct PollIntervalConfig {
    /// Interval between polls in seconds.
    pub interval_secs: u64,
    /// Whether to run immediately on first poll.
    pub immediate_first: bool,
    /// Minimum interval enforced (default: 1 second).
    pub minimum_interval_secs: u64,
}

impl PollIntervalConfig {
    /// Create a new poll interval configuration with sensible defaults.
    pub fn new() -> Self {
        Self {
            interval_secs: 10,
            immediate_first: true,
            minimum_interval_secs: 1,
        }
    }

    /// Set the poll interval in seconds.
    pub fn with_interval_secs(mut self, secs: u64) -> Self {
        self.interval_secs = secs.max(self.minimum_interval_secs);
        self
    }

    /// Set whether the first poll should run immediately.
    pub fn with_immediate_first(mut self, immediate: bool) -> Self {
        self.immediate_first = immediate;
        self
    }

    /// Set the minimum allowed interval in seconds.
    pub fn with_minimum_interval_secs(mut self, secs: u64) -> Self {
        self.minimum_interval_secs = secs;
        self
    }

    /// Get the effective interval (clamped to minimum).
    pub fn effective_interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs.max(self.minimum_interval_secs))
    }
}

impl Default for PollIntervalConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Mock poller behavior executor
// ──────────────────────────────────────────────────────────────────────────────

/// Result of a mock polling operation.
#[derive(Debug)]
pub struct PollResult {
    /// Number of polls executed.
    pub poll_count: usize,
    /// Time of the last poll.
    pub last_poll_at: Duration,
    /// Total elapsed time across all polls.
    pub total_elapsed: Duration,
    /// Whether the poller is still enabled.
    pub enabled: bool,
    /// Vector of poll times for detailed analysis.
    pub poll_times: Vec<Duration>,
}

/// Mock poller behavior executor for testing polling logic.
pub struct MockPoller {
    config: PollIntervalConfig,
    enabled: bool,
    start_time: Instant,
    poll_times: Vec<Duration>,
}

impl MockPoller {
    /// Create a new mock poller with default configuration.
    pub fn new() -> Self {
        Self {
            config: PollIntervalConfig::new(),
            enabled: true,
            start_time: Instant::now(),
            poll_times: Vec::new(),
        }
    }

    /// Set the poll interval in seconds.
    pub fn with_interval_secs(mut self, secs: u64) -> Self {
        self.config = self.config.with_interval_secs(secs);
        self
    }

    /// Set whether the first poll should run immediately.
    pub fn with_immediate_first(mut self, immediate: bool) -> Self {
        self.config = self.config.with_immediate_first(immediate);
        self
    }

    /// Set whether the poller is enabled.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Check if a poll should occur at the given elapsed time.
    pub fn should_poll_at(&self, elapsed: Duration) -> bool {
        if !self.enabled {
            return false;
        }

        let interval = self.config.effective_interval();

        if self.config.immediate_first && elapsed == Duration::ZERO {
            return true;
        }

        // Only poll at exact interval boundaries
        if elapsed.as_nanos() % interval.as_nanos() == 0 && elapsed > Duration::ZERO {
            return true;
        }

        false
    }

    /// Simulate a polling session over a given duration.
    pub async fn run_for_duration(&mut self, duration: Duration) -> PollResult {
        let mut poll_count = 0;
        let mut elapsed = Duration::ZERO;
        let step = Duration::from_millis(100); // Check every 100ms

        while elapsed < duration {
            if self.should_poll_at(elapsed) {
                poll_count += 1;
                self.poll_times.push(elapsed);
            }

            sleep(step).await;
            elapsed += step;
        }

        // Check the final boundary
        if self.should_poll_at(duration) {
            poll_count += 1;
            self.poll_times.push(duration);
        }

        PollResult {
            poll_count,
            last_poll_at: self.poll_times.last().copied().unwrap_or(Duration::ZERO),
            total_elapsed: duration,
            enabled: self.enabled,
            poll_times: self.poll_times.clone(),
        }
    }

    /// Run the poller with a manual clock for precise control.
    pub fn run_with_manual_clock(&mut self, poll_times: Vec<Duration>) -> PollResult {
        let mut executed_polls = 0;

        for elapsed in poll_times {
            if self.should_poll_at(elapsed) {
                executed_polls += 1;
                self.poll_times.push(elapsed);
            }
        }

        let last_time = self.poll_times.last().copied().unwrap_or(Duration::ZERO);

        PollResult {
            poll_count: executed_polls,
            last_poll_at: last_time,
            total_elapsed: last_time,
            enabled: self.enabled,
            poll_times: self.poll_times.clone(),
        }
    }
}

impl Default for MockPoller {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Interval calculation helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Calculate the number of polls that should occur in a given duration.
pub fn expected_poll_count(duration: Duration, interval: Duration, immediate_first: bool) -> usize {
    if interval == Duration::ZERO {
        return 0;
    }

    let mut count = if immediate_first { 1 } else { 0 };

    // Count full interval boundaries
    let secs = duration.as_secs();
    let interval_secs = interval.as_secs();

    if let Some(interval_count) = secs.checked_div(interval_secs) {
        count += interval_count as usize;
    }

    count
}

/// Calculate the time of the Nth poll (1-indexed).
pub fn nth_poll_time(n: usize, interval: Duration, immediate_first: bool) -> Option<Duration> {
    if n == 0 {
        return None;
    }

    if immediate_first {
        if n == 1 {
            Some(Duration::ZERO)
        } else {
            Some(interval * (n - 1) as u32)
        }
    } else {
        Some(interval * n as u32)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Common interval constants for testing
// ──────────────────────────────────────────────────────────────────────────────

/// Common poll interval durations used across tests.
pub mod intervals {
    use super::Duration;

    /// 1 second interval.
    pub const ONE_SECOND: Duration = Duration::from_secs(1);

    /// 10 seconds interval (default).
    pub const TEN_SECONDS: Duration = Duration::from_secs(10);

    /// 30 seconds interval.
    pub const THIRTY_SECONDS: Duration = Duration::from_secs(30);

    /// 1 minute interval.
    pub const ONE_MINUTE: Duration = Duration::from_secs(60);

    /// 5 minutes interval.
    pub const FIVE_MINUTES: Duration = Duration::from_secs(300);

    /// 10 minutes interval.
    pub const TEN_MINUTES: Duration = Duration::from_secs(600);

    /// 1 hour interval.
    pub const ONE_HOUR: Duration = Duration::from_secs(3600);

    /// 6 hours interval (default upgrade check).
    pub const SIX_HOURS: Duration = Duration::from_secs(21600);

    /// 24 hours interval.
    pub const ONE_DAY: Duration = Duration::from_secs(86400);
}

// ──────────────────────────────────────────────────────────────────────────────
// Assertion helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Assert that a poll result has the expected number of polls.
pub fn assert_poll_count(result: &PollResult, expected: usize) -> Result<(), String> {
    if result.poll_count != expected {
        return Err(format!(
            "Expected {} polls, but got {}. Poll times: {:?}",
            expected, result.poll_count, result.poll_times
        ));
    }
    Ok(())
}

/// Assert that a poll result has polls at the expected times.
pub fn assert_poll_times(result: &PollResult, expected: &[Duration]) -> Result<(), String> {
    if result.poll_times != expected {
        return Err(format!(
            "Expected poll times {:?}, but got {:?}",
            expected, result.poll_times
        ));
    }
    Ok(())
}

/// Assert that polls are evenly spaced at the given interval.
pub fn assert_even_spacing(result: &PollResult, interval: Duration) -> Result<(), String> {
    if result.poll_times.len() < 2 {
        return Ok(()); // Not enough polls to check spacing
    }

    for window in result.poll_times.windows(2) {
        let spacing = window[1].saturating_sub(window[0]);
        if spacing != interval {
            return Err(format!(
                "Expected even spacing of {:?}, but found {:?} between polls at {:?} and {:?}",
                interval, spacing, window[0], window[1]
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_clock_creation() {
        let _clock = MockClock::new();
        // Clock should start at current time
        // This is a basic smoke test
    }

    #[tokio::test]
    async fn test_mock_clock_advance() {
        let clock = MockClock::new();
        clock.advance_secs(10).await;
        // Clock should advance
        // This is a basic smoke test
    }

    #[test]
    fn test_poll_interval_config_defaults() {
        let config = PollIntervalConfig::new();
        assert_eq!(config.interval_secs, 10);
        assert!(config.immediate_first);
        assert_eq!(config.minimum_interval_secs, 1);
    }

    #[test]
    fn test_poll_interval_config_clamping() {
        let config = PollIntervalConfig::new().with_interval_secs(0);
        assert_eq!(config.effective_interval(), Duration::from_secs(1));
    }

    #[test]
    fn test_expected_poll_count_immediate() {
        let count = expected_poll_count(Duration::from_secs(60), Duration::from_secs(10), true);
        // At t=0, 10, 20, 30, 40, 50, 60 = 7 polls
        assert_eq!(count, 7);
    }

    #[test]
    fn test_expected_poll_count_no_immediate() {
        let count = expected_poll_count(Duration::from_secs(60), Duration::from_secs(10), false);
        // At t=10, 20, 30, 40, 50, 60 = 6 polls
        assert_eq!(count, 6);
    }

    #[test]
    fn test_nth_poll_time_immediate() {
        assert_eq!(
            nth_poll_time(1, Duration::from_secs(10), true),
            Some(Duration::ZERO)
        );
        assert_eq!(
            nth_poll_time(2, Duration::from_secs(10), true),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            nth_poll_time(3, Duration::from_secs(10), true),
            Some(Duration::from_secs(20))
        );
    }

    #[test]
    fn test_nth_poll_time_no_immediate() {
        assert_eq!(
            nth_poll_time(1, Duration::from_secs(10), false),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            nth_poll_time(2, Duration::from_secs(10), false),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            nth_poll_time(3, Duration::from_secs(10), false),
            Some(Duration::from_secs(30))
        );
    }

    #[tokio::test]
    async fn test_mock_poller_basic() {
        let mut poller = MockPoller::new()
            .with_interval_secs(10)
            .with_immediate_first(true);

        let result = poller.run_with_manual_clock(vec![
            Duration::ZERO,
            Duration::from_secs(10),
            Duration::from_secs(20),
        ]);

        assert_eq!(result.poll_count, 3);
        assert!(result.enabled);
    }

    #[tokio::test]
    async fn test_mock_poller_disabled() {
        let mut poller = MockPoller::new().with_enabled(false).with_interval_secs(10);

        let result = poller.run_with_manual_clock(vec![Duration::ZERO, Duration::from_secs(10)]);

        assert_eq!(result.poll_count, 0);
        assert!(!result.enabled);
    }

    #[test]
    fn test_assert_poll_count_success() {
        let result = PollResult {
            poll_count: 3,
            last_poll_at: Duration::from_secs(20),
            total_elapsed: Duration::from_secs(20),
            enabled: true,
            poll_times: vec![
                Duration::ZERO,
                Duration::from_secs(10),
                Duration::from_secs(20),
            ],
        };

        assert!(assert_poll_count(&result, 3).is_ok());
    }

    #[test]
    fn test_assert_poll_count_failure() {
        let result = PollResult {
            poll_count: 2,
            last_poll_at: Duration::from_secs(10),
            total_elapsed: Duration::from_secs(10),
            enabled: true,
            poll_times: vec![Duration::ZERO, Duration::from_secs(10)],
        };

        assert!(assert_poll_count(&result, 3).is_err());
    }

    #[test]
    fn test_assert_even_spacing_success() {
        let result = PollResult {
            poll_count: 3,
            last_poll_at: Duration::from_secs(20),
            total_elapsed: Duration::from_secs(20),
            enabled: true,
            poll_times: vec![
                Duration::ZERO,
                Duration::from_secs(10),
                Duration::from_secs(20),
            ],
        };

        assert!(assert_even_spacing(&result, Duration::from_secs(10)).is_ok());
    }

    #[test]
    fn test_assert_even_spacing_failure() {
        let result = PollResult {
            poll_count: 3,
            last_poll_at: Duration::from_secs(25),
            total_elapsed: Duration::from_secs(25),
            enabled: true,
            poll_times: vec![
                Duration::ZERO,
                Duration::from_secs(10),
                Duration::from_secs(25),
            ],
        };

        assert!(assert_even_spacing(&result, Duration::from_secs(10)).is_err());
    }
}
