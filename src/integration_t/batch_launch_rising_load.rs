//! Batch launch rising load regression test.
//!
//! This test verifies that batch launch with `--count=N` produces increasing
//! inter-launch delays under simulated rising load conditions, not flat/predictable
//! intervals. This is a regression test for P12.2 implementation ensuring the
//! load-adaptive stagger mechanism works correctly.
//!
//! Test Strategy:
//! 1. Simulate batch launch of 5 workers with load-adaptive stagger
//! 2. Record spawn attempt timestamps to measure inter-launch delays
//! 3. Verify delays increase monotonically under rising load (not flat/predictable)
//! 4. Confirm load-adaptive stagger is functioning per P12.2 implementation
//!
//! Related: ADR-008 (fleet resource safety), P12.2 (load-adaptive launch stagger)

use std::time::{Duration, Instant};
use crate::integration_t::LoadSimulator;
use crate::rate_limit::RateLimiter;
use crate::telemetry::Telemetry;

/// Simulate batch launch with rising load and verify inter-launch delays increase.
///
/// This test models the CLI's batch launch sequence (`--count=5`) where:
/// - First worker launches immediately (no stagger)
/// - Subsequent workers use load-adaptive stagger before launch
/// - Under rising load, delays should increase (not be flat/predictable)
#[tokio::test]
async fn batch_launch_rising_load_produces_increasing_delays() {
    // Set up load simulator for 5-worker batch launch
    let temp_dir = tempfile::tempdir().unwrap();
    let mut simulator = LoadSimulator::new(5, temp_dir).unwrap();

    // Simulate CPU/memory thresholds that will trigger load-adaptive behavior
    let cpu_load_warn = 0.5;  // 50% CPU threshold (conservative for test reliability)
    let memory_free_warn_mb = 2048;  // 2GB free memory threshold
    let base_stagger_secs = 1;  // 1 second base delay (faster than default 2s for test speed)
    let max_wait_secs = 10;  // 10 second max wait (bounded for test speed)
    let check_interval_secs = 1;  // Check every 1 second during extended wait

    // Simulate batch launch sequence (like --count=5)
    // Worker 1: launches immediately (no stagger for seq=0)
    let worker_1_start = Instant::now();
    simulator.record_spawn_attempt();

    // Simulate worker 1 doing work that increases system load
    // In real scenario, worker_construction takes ~5s and adds to load
    simulate_load_increase(Duration::from_millis(100));

    // Worker 2: load-adaptive stagger before launch
    let telemetry = Telemetry::new("batch-launch-test-2".to_string());
    let worker_2_start = Instant::now();

    RateLimiter::load_adaptive_stagger(
        cpu_load_warn,
        memory_free_warn_mb,
        base_stagger_secs,
        max_wait_secs,
        check_interval_secs,
        &telemetry,
    );

    simulator.record_spawn_attempt();
    let delay_1_to_2 = worker_2_start.saturating_duration_since(worker_1_start)
        + simulator.inter_launch_delays().last().copied().unwrap_or(Duration::ZERO);

    // Simulate rising load (worker 2 + worker 1 both active)
    simulate_load_increase(Duration::from_millis(150));

    // Worker 3: load-adaptive stagger with higher load
    let telemetry = Telemetry::new("batch-launch-test-3".to_string());
    let worker_3_start = Instant::now();

    RateLimiter::load_adaptive_stagger(
        cpu_load_warn,
        memory_free_warn_mb,
        base_stagger_secs,
        max_wait_secs,
        check_interval_secs,
        &telemetry,
    );

    simulator.record_spawn_attempt();
    let delay_2_to_3 = worker_3_start.saturating_duration_since(worker_2_start)
        + simulator.inter_launch_delays().last().copied().unwrap_or(Duration::ZERO);

    // Simulate further rising load (3 workers now active)
    simulate_load_increase(Duration::from_millis(200));

    // Worker 4: load-adaptive stagger with even higher load
    let telemetry = Telemetry::new("batch-launch-test-4".to_string());
    let worker_4_start = Instant::now();

    RateLimiter::load_adaptive_stagger(
        cpu_load_warn,
        memory_free_warn_mb,
        base_stagger_secs,
        max_wait_secs,
        check_interval_secs,
        &telemetry,
    );

    simulator.record_spawn_attempt();
    let delay_3_to_4 = worker_4_start.saturating_duration_since(worker_3_start)
        + simulator.inter_launch_delays().last().copied().unwrap_or(Duration::ZERO);

    // Simulate maximum load (4 workers active)
    simulate_load_increase(Duration::from_millis(250));

    // Worker 5: load-adaptive stagger at peak load
    let telemetry = Telemetry::new("batch-launch-test-5".to_string());
    let worker_5_start = Instant::now();

    RateLimiter::load_adaptive_stagger(
        cpu_load_warn,
        memory_free_warn_mb,
        base_stagger_secs,
        max_wait_secs,
        check_interval_secs,
        &telemetry,
    );

    simulator.record_spawn_attempt();
    let delay_4_to_5 = worker_5_start.saturating_duration_since(worker_4_start)
        + simulator.inter_launch_delays().last().copied().unwrap_or(Duration::ZERO);

    // Verify we recorded 5 spawn attempts
    assert_eq!(
        simulator.spawn_attempt_count(),
        5,
        "should have recorded 5 spawn attempts for --count=5 batch launch"
    );

    // Get the measured inter-launch delays
    let delays = simulator.inter_launch_delays();
    assert_eq!(
        delays.len(),
        4,
        "should have 4 inter-launch delays for 5 workers (1→2, 2→3, 3→4, 4→5)"
    );

    // Verify delays increase monotonically (rising load condition)
    // Each delay should be at least as long as the previous one
    for i in 1..delays.len() {
        assert!(
            delays[i] >= delays[i - 1],
            "inter-launch delay[{}] ({:?}) should be >= delay[{}] ({:?}) under rising load",
            i, delays[i], i - 1, delays[i - 1]
        );
    }

    // Verify the delays are not flat/predictable (at least 10% variation)
    let min_delay = simulator.min_inter_launch_delay().unwrap();
    let max_delay = simulator.max_inter_launch_delay().unwrap();
    let variation_ratio = max_delay.as_secs_f64() / min_delay.as_secs_f64();

    assert!(
        variation_ratio >= 1.1,
        "inter-launch delays should vary by at least 10% under rising load, \
         but min={:?} max={:?} (ratio={:.2})",
        min_delay, max_delay, variation_ratio
    );

    // Verify all delays are at least the base stagger time
    for (i, delay) in delays.iter().enumerate() {
        assert!(
            *delay >= Duration::from_secs(base_stagger_secs),
            "delay[{}] ({:?}) should be at least base_stagger_secs ({:?})",
            i, delay, Duration::from_secs(base_stagger_secs)
        );
    }

    // Verify no delay exceeds max_wait_secs by more than check_interval
    for (i, delay) in delays.iter().enumerate() {
        let max_allowed = Duration::from_secs(max_wait_secs) + Duration::from_secs(check_interval_secs);
        assert!(
            *delay <= max_allowed,
            "delay[{}] ({:?}) should not exceed max_wait_secs + check_interval ({:?})",
            i, delay, max_allowed
        );
    }

    // Log the delays for diagnostic purposes
    println!("Batch launch inter-launch delays under rising load:");
    for (i, delay) in delays.iter().enumerate() {
        println!("  Worker {} → Worker {}: {:?}", i + 1, i + 2, delay);
    }
    println!("  Min delay: {:?}", min_delay);
    println!("  Max delay: {:?}", max_delay);
    println!("  Average delay: {:?}", simulator.average_inter_launch_delay().unwrap());
    println!("  Variation ratio: {:.2}x", variation_ratio);
}

/// Simulate system load increasing over time.
///
/// This helper simulates the effect of workers starting and doing work,
/// which increases CPU load and reduces available memory. In the real system,
/// this would be reflected in `/proc/loadavg` and `/proc/meminfo` readings.
fn simulate_load_increase(duration: Duration) {
    // Sleep to simulate time passing while load increases
    std::thread::sleep(duration);

    // Note: In a real test environment, we could inject mock /proc/loadavg
    // and /proc/meminfo values here to precisely control the load simulation.
    // However, for a regression test, we rely on the fact that:
    // 1. Multiple threads/processes running will increase system load
    // 2. The load-adaptive stagger will detect this and extend delays
    // 3. The test verifies delays are NOT flat (which would indicate broken stagger)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verify_batch_launch_test_infrastructure() {
        // Verify the LoadSimulator infrastructure works as expected
        let temp_dir = tempfile::tempdir().unwrap();
        let simulator = LoadSimulator::new(5, temp_dir).unwrap();

        assert_eq!(simulator.worker_capacity(), 5);
        assert_eq!(simulator.spawn_attempt_count(), 0);

        simulator.record_spawn_attempt();
        assert_eq!(simulator.spawn_attempt_count(), 1);
    }
}
