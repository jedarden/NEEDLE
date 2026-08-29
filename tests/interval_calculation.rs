//! Interval calculation tests for NEEDLE polling infrastructure.
//!
//! Tests the core interval calculation logic including:
//! - Next poll time calculation
//! - Zero interval edge case handling (minimum interval enforcement)
//! - System clock respect (monotonic time usage)
//! - Interval boundary precision

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use needle::supervisor::UpgradePoller;
use needle::telemetry::Telemetry;

// ──────────────────────────────────────────────────────────────────────────────
// Interval calculation tests
// ──────────────────────────────────────────────────────────────────────────────

/// Test zero interval is clamped to minimum (1 second).
#[test]
fn zero_interval_clamped_to_minimum() {
    let poller = UpgradePoller::new(true, 0);
    assert_eq!(poller.interval(), Duration::from_secs(1));
}

/// Test interval calculation preserves configured values above minimum.
#[test]
fn interval_preserves_configured_value_above_minimum() {
    let test_cases = vec![
        (1, Duration::from_secs(1)),
        (10, Duration::from_secs(10)),
        (60, Duration::from_secs(60)),
        (3600, Duration::from_secs(3600)),
        (86400, Duration::from_secs(86400)),
    ];

    for (secs, expected) in test_cases {
        let poller = UpgradePoller::new(true, secs);
        assert_eq!(
            poller.interval(),
            expected,
            "Interval should preserve configured value for {} seconds",
            secs
        );
    }
}

/// Test that first poll runs immediately (next_poll_time = 0).
#[test]
fn first_poll_runs_immediately() {
    let mut poller = UpgradePoller::new(true, 60);
    let telemetry = Telemetry::new("test-immediate".to_string());
    let now = Instant::now();

    // First poll should always run regardless of interval
    assert!(poller.poll_at(&telemetry, now));
}

/// Test next poll time calculation after first poll.
#[test]
fn next_poll_time_after_first_poll() {
    let mut poller = UpgradePoller::new(true, 10);
    let telemetry = Telemetry::new("test-next-poll".to_string());
    let now = Instant::now();

    // First poll runs immediately
    assert!(poller.poll_at(&telemetry, now));

    // Next poll should be at t=10 (one interval from first poll)
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(9)));
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(10)));

    // Following poll should be at t=20 (two intervals from first poll)
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(19)));
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(20)));
}

/// Test that interval respects system clock (monotonic time).
#[test]
fn interval_respects_system_clock() {
    let mut poller = UpgradePoller::new(true, 5);
    let telemetry = Telemetry::new("test-clock".to_string());
    let now = Instant::now();

    // First poll
    assert!(poller.poll_at(&telemetry, now));

    // System clock time passes
    let elapsed = now.elapsed();

    // Next poll should respect actual elapsed time
    // If elapsed >= 5 seconds, poll should run; otherwise it should skip
    let _should_poll = elapsed >= Duration::from_secs(5);
    let poll_result = poller.poll_at(&telemetry, Instant::now());

    // Note: This test documents behavior but elapsed time is environment-dependent
    // In a real test environment, elapsed is typically < 5 seconds
    if elapsed < Duration::from_secs(5) {
        assert!(
            !poll_result,
            "Should skip poll when interval hasn't elapsed"
        );
    } else {
        assert!(poll_result, "Should run poll when interval has elapsed");
    }
}

/// Test interval calculation with explicit clock control.
#[test]
fn interval_with_explicit_clock_control() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker = Arc::new(move |_: &Telemetry| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let mut poller = UpgradePoller::with_checker(true, 10, checker);
    let telemetry = Telemetry::new("test-explicit-clock".to_string());

    let base_time = Instant::now();

    // First poll at t=0
    assert!(poller.poll_at(&telemetry, base_time));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Poll at t=5 (before interval) should skip
    assert!(!poller.poll_at(&telemetry, base_time + Duration::from_secs(5)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Poll at t=10 (at interval) should run
    assert!(poller.poll_at(&telemetry, base_time + Duration::from_secs(10)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Poll at t=15 (before next interval) should skip
    assert!(!poller.poll_at(&telemetry, base_time + Duration::from_secs(15)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Poll at t=20 (at second interval) should run
    assert!(poller.poll_at(&telemetry, base_time + Duration::from_secs(20)));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// Test interval boundary precision (exact vs. before/after).
#[test]
fn interval_boundary_precision() {
    let mut poller = UpgradePoller::new(true, 30);
    let telemetry = Telemetry::new("test-boundaries".to_string());
    let now = Instant::now();

    // First poll at t=0
    assert!(poller.poll_at(&telemetry, now));

    // Just before interval (29.999s) should skip
    assert!(!poller.poll_at(
        &telemetry,
        now + Duration::from_secs(29) + Duration::from_millis(999)
    ));

    // At exact interval (30.0s) should run
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(30)));

    // Just after interval (30.001s) should skip (next interval is at 60s)
    assert!(!poller.poll_at(
        &telemetry,
        now + Duration::from_secs(30) + Duration::from_millis(1)
    ));

    // At next interval (60.0s) should run
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(60)));
}

/// Test interval calculation with very short intervals (1 second).
#[test]
fn interval_with_very_short_duration() {
    let mut poller = UpgradePoller::new(true, 1);
    let telemetry = Telemetry::new("test-short-interval".to_string());
    let now = Instant::now();

    // First poll
    assert!(poller.poll_at(&telemetry, now));

    // At 1ms - skipped
    assert!(!poller.poll_at(&telemetry, now + Duration::from_millis(1)));

    // At 999ms - skipped
    assert!(!poller.poll_at(&telemetry, now + Duration::from_millis(999)));

    // At 1000ms (1 second) - runs
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(1)));

    // At 2000ms (2 seconds) - runs
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(2)));
}

/// Test interval calculation with very long intervals (24 hours).
#[test]
fn interval_with_very_long_duration() {
    let mut poller = UpgradePoller::new(true, 86400);
    let telemetry = Telemetry::new("test-long-interval".to_string());
    let now = Instant::now();

    // First poll
    assert!(poller.poll_at(&telemetry, now));

    // After 1 hour - skipped
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(3600)));

    // After 12 hours - skipped
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(43200)));

    // After 23:59:59 - skipped
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(86399)));

    // At exactly 24 hours - runs
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(86400)));
}

/// Test that disabled poller never runs regardless of interval.
#[test]
fn disabled_poller_never_runs() {
    let mut poller = UpgradePoller::new(false, 1);
    let telemetry = Telemetry::new("test-disabled".to_string());
    let now = Instant::now();

    // Even with 1-second interval and disabled, should never run
    assert!(!poller.poll_at(&telemetry, now));
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(1)));
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(10)));
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(100)));
}

/// Test consecutive poll calculations maintain state.
#[test]
fn consecutive_polls_maintain_state() {
    let mut poller = UpgradePoller::new(true, 10);
    let telemetry = Telemetry::new("test-consecutive".to_string());
    let now = Instant::now();

    // Series of polls at exact interval boundaries
    assert!(poller.poll_at(&telemetry, now)); // t=0
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(10))); // t=10
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(20))); // t=20
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(30))); // t=30

    // Verify state is maintained by checking mid-interval skips
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(35)));
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(39)));

    // Next interval should still work
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(40)));
}

/// Test that checker errors don't affect interval calculation.
#[test]
fn checker_errors_dont_affect_interval_calculation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker = Arc::new(move |_: &Telemetry| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Err::<(), _>(anyhow::anyhow!("test error"))
    });

    let mut poller = UpgradePoller::with_checker(true, 5, checker);
    let telemetry = Telemetry::new("test-error-interval".to_string());
    let now = Instant::now();

    // First poll runs (checker is called)
    assert!(poller.poll_at(&telemetry, now));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Poll before interval should skip even though checker failed
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(4)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Poll at interval should run and call checker again
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(5)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// Test interval calculation with subsecond precision.
#[test]
fn interval_with_subsecond_precision() {
    let mut poller = UpgradePoller::new(true, 10);
    let telemetry = Telemetry::new("test-subsecond".to_string());
    let now = Instant::now();

    // First poll
    assert!(poller.poll_at(&telemetry, now));

    // Test millisecond precision around interval boundary
    for millis in [9990, 9991, 9995, 9999] {
        assert!(
            !poller.poll_at(&telemetry, now + Duration::from_millis(millis)),
            "Should skip at {}ms (before 10s interval)",
            millis
        );
    }

    // At exactly 10 seconds (10000ms) should run
    assert!(poller.poll_at(&telemetry, now + Duration::from_millis(10000)));

    // Just after interval should skip
    assert!(!poller.poll_at(&telemetry, now + Duration::from_millis(10001)));
}

/// Test that minimum interval enforcement applies to all edge cases.
#[test]
fn minimum_interval_enforcement_edge_cases() {
    let test_cases = vec![
        (0, Duration::from_secs(1)),
        (1, Duration::from_secs(1)),
        (2, Duration::from_secs(2)),
    ];

    for (input, expected) in test_cases {
        let poller = UpgradePoller::new(true, input);
        assert_eq!(
            poller.interval(),
            expected,
            "Minimum interval enforcement failed for input: {}",
            input
        );
    }
}

/// Test interval calculation doesn't drift over multiple iterations.
#[test]
fn interval_calculation_no_drift() {
    let mut poller = UpgradePoller::new(true, 60);
    let telemetry = Telemetry::new("test-drift".to_string());
    let now = Instant::now();

    // Run 10 intervals
    for i in 0..10 {
        let expected_time = now + Duration::from_secs(i * 60);
        assert!(
            poller.poll_at(&telemetry, expected_time),
            "Poll should run at interval {} (t={}s)",
            i,
            i * 60
        );
    }

    // Verify mid-interval checks still skip
    for i in 0..10 {
        let mid_time = now + Duration::from_secs(i * 60 + 30);
        assert!(
            !poller.poll_at(&telemetry, mid_time),
            "Mid-interval check should skip at interval {} (t={}s)",
            i,
            i * 60 + 30
        );
    }
}

/// Test interval calculation with different start times.
#[test]
fn interval_with_different_start_times() {
    let _poller = UpgradePoller::new(true, 15);
    let telemetry = Telemetry::new("test-start-times".to_string());

    // Test with different base times
    let start_times = vec![
        Instant::now(),
        Instant::now() + Duration::from_secs(1000),
        Instant::now() + Duration::from_secs(10000),
    ];

    for base_time in start_times {
        let mut poller = UpgradePoller::new(true, 15);

        // First poll
        assert!(poller.poll_at(&telemetry, base_time));

        // Poll at interval
        assert!(poller.poll_at(&telemetry, base_time + Duration::from_secs(15)));

        // Poll before interval should skip
        assert!(!poller.poll_at(&telemetry, base_time + Duration::from_secs(7)));
    }
}

/// Test that interval calculation works with mock times (deterministic).
#[test]
fn interval_with_mock_times_deterministic() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker = Arc::new(move |_: &Telemetry| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let mut poller = UpgradePoller::with_checker(true, 10, checker);
    let telemetry = Telemetry::new("test-mock-time".to_string());

    // Use a fixed base time for deterministic testing
    let base = Instant::now();

    // Simulate a sequence of polls at specific times
    let poll_times = vec![
        base + Duration::from_secs(0),  // First poll
        base + Duration::from_secs(10), // Second poll
        base + Duration::from_secs(20), // Third poll
        base + Duration::from_secs(35), // Should skip
        base + Duration::from_secs(40), // Fourth poll
    ];

    let expected_runs = vec![true, true, true, false, true];

    for (time, should_run) in poll_times.into_iter().zip(expected_runs) {
        let result = poller.poll_at(&telemetry, time);
        assert_eq!(
            result,
            should_run,
            "Poll at t={:?} should {}",
            time.duration_since(base),
            if should_run { "run" } else { "skip" }
        );
    }
}
