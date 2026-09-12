//! Immediate check trigger tests for NEEDLE polling infrastructure.
//!
//! Tests verify that polling triggers immediately when enabled, that the immediate
//! check happens on first cycle, and that subsequent checks respect the configured
//! interval.
//!
//! These tests use the UpgradePoller from the supervisor module, which is the
//! canonical implementation of immediate-first polling behavior in NEEDLE.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use needle::supervisor::{UpgradeCheckFn, UpgradePoller};
use needle::telemetry::Telemetry;

// ──────────────────────────────────────────────────────────────────────────────
// Immediate trigger tests
// ──────────────────────────────────────────────────────────────────────────────

/// Test that polling triggers immediately when enabled.
///
/// This test verifies that when a poller is enabled, the first poll happens
/// immediately without waiting for the interval to elapse.
#[test]
fn polling_triggers_immediately_when_enabled() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker: UpgradeCheckFn = Arc::new(move |_| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let mut poller = UpgradePoller::with_checker(true, 60, checker);
    let telemetry = Telemetry::new("test-immediate-trigger".to_string());
    let now = Instant::now();

    // First poll should run immediately when enabled
    assert!(poller.poll_at(&telemetry, now));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "Checker should be called immediately"
    );
}

/// Test that immediate check happens on first cycle.
///
/// This test verifies that the very first call to `poll_at` succeeds
/// regardless of the configured interval, demonstrating the immediate-first
/// behavior.
#[test]
fn immediate_check_happens_on_first_cycle() {
    let mut poller = UpgradePoller::new(true, 3600); // 1 hour interval
    let telemetry = Telemetry::new("test-first-cycle".to_string());
    let now = Instant::now();

    // Even with a 1-hour interval, first poll runs immediately
    assert!(
        poller.poll_at(&telemetry, now),
        "First poll should run immediately"
    );

    // Verify the poll was recorded
    assert_eq!(poller.interval(), Duration::from_secs(3600));
    assert!(poller.enabled());
}

/// Test that immediate check only happens once on first poll.
///
/// This test verifies that the immediate behavior is specific to the first
/// poll and does not repeat on subsequent polls before the interval elapses.
#[test]
fn immediate_check_only_happens_once_on_first_poll() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker: UpgradeCheckFn = Arc::new(move |_| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let mut poller = UpgradePoller::with_checker(true, 30, checker);
    let telemetry = Telemetry::new("test-once-immediate".to_string());
    let now = Instant::now();

    // First poll runs immediately
    assert!(poller.poll_at(&telemetry, now));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Second poll before interval should NOT run
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(29)));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "Should not call again before interval"
    );
}

/// Test that subsequent checks respect the configured interval.
///
/// This test verifies that after the immediate first poll, the poller
/// waits for the full interval before polling again.
#[test]
fn subsequent_checks_respect_interval() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker: UpgradeCheckFn = Arc::new(move |_| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let mut poller = UpgradePoller::with_checker(true, 10, checker);
    let telemetry = Telemetry::new("test-respect-interval".to_string());
    let now = Instant::now();

    // First poll runs immediately
    assert!(poller.poll_at(&telemetry, now));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Polls before interval are skipped
    for offset in [1, 5, 9] {
        assert!(
            !poller.poll_at(&telemetry, now + Duration::from_secs(offset)),
            "Should skip poll at t={}s (before 10s interval)",
            offset
        );
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "No additional calls before interval"
    );

    // Poll at exact interval runs
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(10)));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "Should call again at interval boundary"
    );

    // Polls between intervals are skipped
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(15)));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "Should skip mid-interval poll"
    );

    // Poll at next interval runs
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(20)));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "Should call at second interval boundary"
    );
}

/// Test interval enforcement across multiple cycles.
///
/// This test verifies that the interval is respected consistently across
/// multiple polling cycles, not just the first few.
#[test]
fn interval_enforcement_across_multiple_cycles() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker: UpgradeCheckFn = Arc::new(move |_| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let mut poller = UpgradePoller::with_checker(true, 5, checker);
    let telemetry = Telemetry::new("test-multiple-cycles".to_string());
    let now = Instant::now();

    // Test 5 full cycles
    for cycle in 0..5usize {
        let expected_time = now + Duration::from_secs((cycle * 5) as u64);
        assert!(
            poller.poll_at(&telemetry, expected_time),
            "Poll should run at cycle {} (t={}s)",
            cycle,
            cycle * 5
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            cycle + 1,
            "Checker should be called once per cycle"
        );
    }

    // Verify mid-interval checks are skipped
    for offset in [1, 2, 3, 4, 6, 7, 8, 9, 11, 12, 13, 14] {
        assert!(
            !poller.poll_at(&telemetry, now + Duration::from_secs(offset)),
            "Should skip poll at t={}s (not on 5s interval)",
            offset
        );
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        5,
        "No additional calls mid-interval"
    );
}

/// Test that disabled poller never runs, regardless of timing.
///
/// This test verifies that when polling is disabled, the poller never runs
/// even if the timing would otherwise be correct.
#[test]
fn disabled_poller_never_runs() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker: UpgradeCheckFn = Arc::new(move |_| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let mut poller = UpgradePoller::with_checker(false, 10, checker);
    let telemetry = Telemetry::new("test-disabled-poller".to_string());
    let now = Instant::now();

    // Disabled poller should never run, even at t=0
    assert!(!poller.poll_at(&telemetry, now));
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(10)));
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(20)));
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(100)));

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "Disabled poller should never call checker"
    );
}

/// Test immediate behavior with very short intervals.
///
/// This test verifies that immediate-first polling works correctly even
/// with very short intervals (1 second).
#[test]
fn immediate_behavior_with_very_short_interval() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker: UpgradeCheckFn = Arc::new(move |_| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let mut poller = UpgradePoller::with_checker(true, 1, checker);
    let telemetry = Telemetry::new("test-short-interval".to_string());
    let now = Instant::now();

    // First poll runs immediately
    assert!(poller.poll_at(&telemetry, now));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // At 1ms - should skip
    assert!(!poller.poll_at(&telemetry, now + Duration::from_millis(1)));

    // At 999ms - should skip
    assert!(!poller.poll_at(&telemetry, now + Duration::from_millis(999)));

    // At 1000ms (1 second) - should run
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(1)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // At 2000ms (2 seconds) - should run
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(2)));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// Test immediate behavior with very long intervals.
///
/// This test verifies that immediate-first polling works correctly even
/// with very long intervals (24 hours).
#[test]
fn immediate_behavior_with_very_long_interval() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker: UpgradeCheckFn = Arc::new(move |_| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let mut poller = UpgradePoller::with_checker(true, 86400, checker); // 24 hours
    let telemetry = Telemetry::new("test-long-interval".to_string());
    let now = Instant::now();

    // First poll runs immediately even with 24-hour interval
    assert!(poller.poll_at(&telemetry, now));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // After 1 hour - should skip
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(3600)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // After 12 hours - should skip
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(43200)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // At exactly 24 hours - should run
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(86400)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// Test that checker errors don't affect interval enforcement.
///
/// This test verifies that if the checker function returns an error,
/// the poller still enforces the interval correctly for subsequent polls.
#[test]
fn checker_errors_dont_affect_interval_enforcement() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker: UpgradeCheckFn = Arc::new(move |_| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Err::<(), _>(anyhow::anyhow!("simulated checker failure"))
    });

    let mut poller = UpgradePoller::with_checker(true, 10, checker);
    let telemetry = Telemetry::new("test-error-interval".to_string());
    let now = Instant::now();

    // First poll runs (checker is called and fails)
    assert!(poller.poll_at(&telemetry, now));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Poll before interval should skip even though checker failed
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(5)));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "Should not retry before interval"
    );

    // Poll at interval should run and call checker again
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(10)));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "Should retry at interval");
}

/// Test boundary conditions around the interval.
///
/// This test verifies that the interval boundary is exact: polls just
/// before the interval are skipped, and polls at or after the interval run.
#[test]
fn interval_boundary_conditions() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_checker = Arc::clone(&calls);
    let checker: UpgradeCheckFn = Arc::new(move |_| {
        calls_for_checker.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let mut poller = UpgradePoller::with_checker(true, 30, checker);
    let telemetry = Telemetry::new("test-boundaries".to_string());
    let now = Instant::now();

    // First poll at t=0
    assert!(poller.poll_at(&telemetry, now));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Just before interval (29.999s) - should skip
    assert!(!poller.poll_at(
        &telemetry,
        now + Duration::from_secs(29) + Duration::from_millis(999)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // At exact interval (30.0s) - should run
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(30)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Just after interval (30.001s) - should skip (next interval is at 60s)
    assert!(!poller.poll_at(
        &telemetry,
        now + Duration::from_secs(30) + Duration::from_millis(1)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // At next interval (60.0s) - should run
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(60)));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// Test that interval timing is deterministic.
///
/// This test verifies that given the same starting time and interval,
/// the poller behaves deterministically, producing the same sequence
/// of poll execution times.
#[test]
fn interval_timing_is_deterministic() {
    let mut poller_a = UpgradePoller::new(true, 15);
    let mut poller_b = UpgradePoller::new(true, 15);
    let telemetry = Telemetry::new("test-deterministic".to_string());
    let base_time = Instant::now();

    // Test the same sequence on two independent pollers
    let test_times = vec![
        base_time,
        base_time + Duration::from_secs(15),
        base_time + Duration::from_secs(30),
        base_time + Duration::from_secs(7), // Should skip
        base_time + Duration::from_secs(45),
        base_time + Duration::from_secs(22), // Should skip
    ];

    for time in &test_times {
        let result_a = poller_a.poll_at(&telemetry, *time);
        let result_b = poller_b.poll_at(&telemetry, *time);
        assert_eq!(
            result_a,
            result_b,
            "Both pollers should produce the same result at t={:?}",
            time.duration_since(base_time)
        );
    }
}

/// Test multiple pollers maintain independent state.
///
/// This test verifies that when multiple pollers are running (e.g., for
/// different purposes), each maintains its own independent state and timing.
#[test]
fn multiple_pollers_maintain_independent_state() {
    let telemetry = Telemetry::new("test-multiple-pollers".to_string());
    let now = Instant::now();

    let mut poller_a = UpgradePoller::new(true, 60);
    let mut poller_b = UpgradePoller::new(true, 120);

    // Both pollers should run immediately
    assert!(poller_a.poll_at(&telemetry, now));
    assert!(poller_b.poll_at(&telemetry, now));

    // At 60 seconds, poller_a runs, poller_b skips
    assert!(poller_a.poll_at(&telemetry, now + Duration::from_secs(60)));
    assert!(!poller_b.poll_at(&telemetry, now + Duration::from_secs(60)));

    // At 120 seconds, both run
    assert!(poller_a.poll_at(&telemetry, now + Duration::from_secs(120)));
    assert!(poller_b.poll_at(&telemetry, now + Duration::from_secs(120)));
}

/// Test consecutive polls at exact interval boundaries.
///
/// This test verifies that when polls are executed exactly at each interval
/// boundary, they all succeed and the poller maintains state correctly.
#[test]
fn consecutive_polls_at_exact_boundaries() {
    let mut poller = UpgradePoller::new(true, 10);
    let telemetry = Telemetry::new("test-consecutive-boundaries".to_string());
    let now = Instant::now();

    // Run 10 consecutive polls at exact interval boundaries
    for i in 0..10 {
        let poll_time = now + Duration::from_secs(i * 10);
        assert!(
            poller.poll_at(&telemetry, poll_time),
            "Poll should succeed at interval {} (t={}s)",
            i,
            i * 10
        );
    }

    // Verify that mid-interval checks still skip
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(35)));
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(79)));

    // Next interval should still work
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(100)));
}

/// Test that interval is respected even after many consecutive skips.
///
/// This test verifies that even if many polls are skipped (e.g., due to
/// the caller not polling at the right times), the interval is still
/// enforced correctly when polling resumes.
#[test]
fn interval_respected_after_many_skips() {
    let mut poller = UpgradePoller::new(true, 30);
    let telemetry = Telemetry::new("test-many-skips".to_string());
    let now = Instant::now();

    // First poll
    assert!(poller.poll_at(&telemetry, now));

    // Many consecutive skipped polls at various times
    let skip_times = vec![
        Duration::from_secs(5),
        Duration::from_secs(10),
        Duration::from_secs(15),
        Duration::from_secs(20),
        Duration::from_secs(25),
    ];

    for skip_offset in skip_times {
        assert!(
            !poller.poll_at(&telemetry, now + skip_offset),
            "Should skip poll at t={}s (before 30s interval)",
            skip_offset.as_secs()
        );
    }

    // Poll at exact interval should still work
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(30)));

    // Continue with normal interval behavior
    assert!(!poller.poll_at(&telemetry, now + Duration::from_secs(45)));
    assert!(poller.poll_at(&telemetry, now + Duration::from_secs(60)));
}

/// Test subsecond precision in interval enforcement.
///
/// This test verifies that the poller handles subsecond time correctly,
/// ensuring that polls at 999.999ms are skipped while polls at 1000.0ms run.
#[test]
fn subsecond_precision_in_interval_enforcement() {
    let mut poller = UpgradePoller::new(true, 10);
    let telemetry = Telemetry::new("test-subsecond".to_string());
    let now = Instant::now();

    // First poll
    assert!(poller.poll_at(&telemetry, now));

    // Test millisecond precision around interval boundary
    assert!(!poller.poll_at(&telemetry, now + Duration::from_millis(9990)));
    assert!(!poller.poll_at(&telemetry, now + Duration::from_millis(9999)));
    assert!(poller.poll_at(&telemetry, now + Duration::from_millis(10000)));
    assert!(!poller.poll_at(&telemetry, now + Duration::from_millis(10001)));
}
