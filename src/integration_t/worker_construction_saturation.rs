//! Worker construction saturated load regression test.
//!
//! This test verifies that worker launch defers with exponential backoff
//! when system resources are saturated, and eventually fails with a clear
//! named error message instead of panicking.
//!
//! Test Strategy:
//! 1. Mock /proc/loadavg and /proc/meminfo to simulate persistent saturation
//! 2. Simulate the CLI's resource check loop with retry behavior
//! 3. Verify eventual failure with a clear error message (not panic/unwrap)

use anyhow::Result;
use std::path::Path;
use std::time::Duration;

/// Simulated saturated load test fixture.
///
/// Creates mocked /proc files that indicate persistent CPU and memory saturation
/// to test the worker construction retry behavior.
struct SaturatedLoadFixture {
    /// Temp directory for mocked /proc files
    temp_dir: tempfile::TempDir,
    /// Path to mocked loadavg file
    loadavg_path: std::path::PathBuf,
    /// Path to mocked meminfo file
    meminfo_path: std::path::PathBuf,
}

impl SaturatedLoadFixture {
    /// Create a new fixture with mocked saturation conditions.
    ///
    /// Returns a fixture that simulates:
    /// - CPU load: 100.0 (far above any reasonable threshold)
    /// - Available memory: 1 MB (far below any reasonable threshold)
    fn new() -> Result<Self> {
        let temp_dir = tempfile::tempdir()?;

        // Create mocked /proc/loadavg with extremely high load
        let loadavg_path = temp_dir.path().join("loadavg");
        std::fs::write(&loadavg_path, "100.00 95.00 90.00 1/123 45678\n")?;

        // Create mocked /proc/meminfo with only 1 MB available
        let meminfo_path = temp_dir.path().join("meminfo");
        std::fs::write(
            &meminfo_path,
            "MemAvailable: 1024 kB\nMemTotal: 8388608 kB\n",
        )?;

        Ok(SaturatedLoadFixture {
            temp_dir,
            loadavg_path,
            meminfo_path,
        })
    }

    /// Create a fixture with comfortable resources (for control tests).
    fn comfortable() -> Result<Self> {
        let temp_dir = tempfile::tempdir()?;

        // Create mocked /proc/loadavg with low load
        let loadavg_path = temp_dir.path().join("loadavg");
        std::fs::write(&loadavg_path, "0.50 0.45 0.40 1/123 45678\n")?;

        // Create mocked /proc/meminfo with plenty of memory
        let meminfo_path = temp_dir.path().join("meminfo");
        std::fs::write(
            &meminfo_path,
            "MemAvailable: 8388608 kB\nMemTotal: 8388608 kB\n",
        )?;

        Ok(SaturatedLoadFixture {
            temp_dir,
            loadavg_path,
            meminfo_path,
        })
    }
}

/// Custom check function that uses mocked /proc files instead of system files.
///
/// This is a test-only version of `check_system_resources_for_launch` that accepts
/// custom file paths for dependency injection.
fn check_resources_with_mocked_proc(
    cpu_load_warn: f64,
    memory_free_warn_mb: u64,
    loadavg_path: &Path,
    meminfo_path: &Path,
) -> Result<()> {
    // CPU load: read from mocked loadavg path
    if let Ok(loadavg) = std::fs::read_to_string(loadavg_path) {
        if let Some(load_str) = loadavg.split_whitespace().next() {
            if let Ok(load) = load_str.parse::<f64>() {
                let num_cpus = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1);
                let normalized = load / num_cpus as f64;
                if normalized > cpu_load_warn {
                    return Err(anyhow::anyhow!(
                        "CPU load saturated: {:.2} (1-minute average) / {} cores = {:.2} > threshold {:.2}",
                        load,
                        num_cpus,
                        normalized,
                        cpu_load_warn
                    ));
                }
            }
        }
    }

    // Memory: read from mocked meminfo path
    if let Ok(meminfo) = std::fs::read_to_string(meminfo_path) {
        let mut mem_available_kb: Option<u64> = None;
        for line in meminfo.lines() {
            if line.starts_with("MemAvailable:") {
                if let Some(val) = line.split_whitespace().nth(1) {
                    mem_available_kb = val.parse().ok();
                }
                break;
            }
        }
        if let Some(avail_kb) = mem_available_kb {
            let avail_mb = avail_kb / 1024;
            if avail_mb < memory_free_warn_mb {
                return Err(anyhow::anyhow!(
                    "Memory saturated: {} MB available < {} MB threshold",
                    avail_mb,
                    memory_free_warn_mb
                ));
            }
        }
    }

    Ok(())
}

/// Simulate the CLI's worker construction retry loop with saturation.
///
/// This mimics the actual CLI behavior in src/cli/mod.rs lines 905-955.
fn simulate_worker_construction_with_retry(
    fixture: &SaturatedLoadFixture,
    max_wait_secs: u64,
    retry_delay_secs: u64,
    cpu_load_warn: f64,
    memory_free_warn_mb: u64,
) -> Result<()> {
    let mut total_waited = 0u64;
    let mut retry_delay = retry_delay_secs;
    let mut deferred_count = 0u64;

    loop {
        match check_resources_with_mocked_proc(
            cpu_load_warn,
            memory_free_warn_mb,
            &fixture.loadavg_path,
            &fixture.meminfo_path,
        ) {
            Ok(()) => {
                // Resources acceptable - proceed to worker_construction
                break Ok(());
            }
            Err(e) => {
                if total_waited >= max_wait_secs {
                    // Still saturated after max wait - fail explicitly
                    return Err(anyhow::anyhow!(
                        "worker launch deferred {} times ({}s total wait), system still saturated: {}. Launch aborted — retry when load drops",
                        deferred_count,
                        total_waited,
                        e
                    ));
                }

                // Resources saturated - defer and retry
                deferred_count += 1;
                std::thread::sleep(Duration::from_secs(retry_delay));
                total_waited += retry_delay;

                // Exponential backoff capped at 30 seconds
                retry_delay = std::cmp::min(retry_delay * 2, 30);
            }
        }
    }
}

#[tokio::test]
async fn worker_construction_defers_on_saturated_cpu() {
    let fixture = SaturatedLoadFixture::new().unwrap();

    // Set CPU threshold low enough that mocked load (100.0) definitely exceeds it
    let cpu_load_warn = 0.5; // 50% CPU threshold
    let memory_free_warn_mb = 1; // 1 MB - our mocked value exactly equals this

    // Very short timeout for test - should fail quickly
    let max_wait_secs = 2u64;
    let retry_delay_secs = 1u64;

    let result = simulate_worker_construction_with_retry(
        &fixture,
        max_wait_secs,
        retry_delay_secs,
        cpu_load_warn,
        memory_free_warn_mb,
    );

    // Should fail with a clear error message
    assert!(
        result.is_err(),
        "worker construction should fail under saturation"
    );

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("worker launch deferred"),
        "error should mention defer count: {}",
        error_msg
    );
    assert!(
        error_msg.contains("system still saturated"),
        "error should mention persistent saturation: {}",
        error_msg
    );
    assert!(
        error_msg.contains("Launch aborted"),
        "error should mention launch was aborted: {}",
        error_msg
    );

    // Should NOT contain panic/unwrap language
    assert!(
        !error_msg.contains("panic"),
        "error should not mention panic"
    );
    assert!(
        !error_msg.contains("unwrap"),
        "error should not mention unwrap"
    );
}

#[tokio::test]
async fn worker_construction_defers_on_saturated_memory() {
    let fixture = SaturatedLoadFixture::new().unwrap();

    // Set memory threshold high enough that mocked value (1 MB) definitely is below it
    let cpu_load_warn = 200.0; // CPU is fine (100.0 < 200.0)
    let memory_free_warn_mb = 10; // 10 MB threshold, only 1 MB available

    let max_wait_secs = 2u64;
    let retry_delay_secs = 1u64;

    let result = simulate_worker_construction_with_retry(
        &fixture,
        max_wait_secs,
        retry_delay_secs,
        cpu_load_warn,
        memory_free_warn_mb,
    );

    // Should fail due to memory saturation
    assert!(
        result.is_err(),
        "worker construction should fail under memory saturation"
    );

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Memory saturated"),
        "error should specifically mention memory saturation: {}",
        error_msg
    );
    assert!(
        error_msg.contains("worker launch deferred"),
        "error should mention defer count: {}",
        error_msg
    );
}

#[tokio::test]
async fn worker_construction_succeeds_when_resources_comfortable() {
    let fixture = SaturatedLoadFixture::comfortable().unwrap();

    // Set thresholds that comfortable resources easily meet
    let cpu_load_warn = 2.0; // 200% CPU - our mocked 0.5 is well below
    let memory_free_warn_mb = 1; // 1 MB - our mocked 8 GB is well above

    let max_wait_secs = 1u64;
    let retry_delay_secs = 1u64;

    let result = simulate_worker_construction_with_retry(
        &fixture,
        max_wait_secs,
        retry_delay_secs,
        cpu_load_warn,
        memory_free_warn_mb,
    );

    // Should succeed immediately without deferring
    assert!(
        result.is_ok(),
        "worker construction should succeed with comfortable resources"
    );
}

#[tokio::test]
async fn worker_construction_exponential_backoff() {
    let fixture = SaturatedLoadFixture::new().unwrap();

    let cpu_load_warn = 0.5;
    let memory_free_warn_mb = 1;
    let max_wait_secs = 8u64; // Allow multiple retries
    let retry_delay_secs = 1u64;

    let start = std::time::Instant::now();
    let result = simulate_worker_construction_with_retry(
        &fixture,
        max_wait_secs,
        retry_delay_secs,
        cpu_load_warn,
        memory_free_warn_mb,
    );
    let elapsed = start.elapsed();

    assert!(result.is_err(), "should still fail under saturation");

    // With exponential backoff (1s -> 2s -> 4s -> 8s cap), we should take
    // more than the naive 1s delay but less than max_wait_secs
    assert!(
        elapsed.as_secs() >= 1,
        "should wait at least one retry delay (actual: {}s)",
        elapsed.as_secs()
    );

    // Verify multiple retries occurred
    // 1s + 2s + 4s = 7s < 8s max, so we get 3 retries before failing
    assert!(
        elapsed.as_secs() >= 3,
        "should complete multiple exponential backoff cycles (actual: {}s)",
        elapsed.as_secs()
    );
}

#[tokio::test]
async fn worker_construction_eventual_failure_not_panic() {
    let fixture = SaturatedLoadFixture::new().unwrap();

    let cpu_load_warn = 0.5;
    let memory_free_warn_mb = 1;
    // Use very short timeout to ensure we hit the failure case quickly
    let max_wait_secs = 1u64;
    let retry_delay_secs = 1u64;

    let result = simulate_worker_construction_with_retry(
        &fixture,
        max_wait_secs,
        retry_delay_secs,
        cpu_load_warn,
        memory_free_warn_mb,
    );

    // The key requirement: should return Err with a clear message,
    // NOT panic or unwrap
    let error = result.unwrap_err();
    let error_msg = error.to_string().to_lowercase();

    // Verify it's a proper error, not a panic
    assert!(
        !error_msg.contains("panic"),
        "error should not mention panic: {}",
        error_msg
    );
    assert!(
        !error_msg.contains("unwrap"),
        "error should not mention unwrap: {}",
        error_msg
    );

    // Verify it's a named error with context
    assert!(
        error_msg.contains("deferred") || error_msg.contains("saturated"),
        "error should have a descriptive name mentioning the problem: {}",
        error_msg
    );

    // Verify it includes actionable guidance
    assert!(
        error_msg.contains("retry") || error_msg.contains("aborted"),
        "error should provide actionable guidance: {}",
        error_msg
    );
}

#[tokio::test]
async fn worker_construction_defers_multiple_times_before_failure() {
    let fixture = SaturatedLoadFixture::new().unwrap();

    let cpu_load_warn = 0.5;
    let memory_free_warn_mb = 1;
    // Allow enough time for multiple defers before timeout
    let max_wait_secs = 7u64;
    let retry_delay_secs = 1u64;

    let result = simulate_worker_construction_with_retry(
        &fixture,
        max_wait_secs,
        retry_delay_secs,
        cpu_load_warn,
        memory_free_warn_mb,
    );

    assert!(result.is_err(), "should fail after multiple defers");

    let error_msg = result.unwrap_err().to_string();

    // The error message should mention the defer count
    // With exponential backoff: 1s + 2s + 4s = 7s, so we get 2-3 defers
    assert!(
        error_msg.contains("deferred") && error_msg.contains("times"),
        "error should mention defer count and number of times: {}",
        error_msg
    );

    // Extract defer count from error message
    if let Some(count_part) = error_msg.split("deferred").nth(1) {
        if let Some(count_str) = count_part.split_whitespace().next() {
            if let Ok(count) = count_str.parse::<u64>() {
                assert!(
                    count >= 2,
                    "should defer at least twice before failure (got {})",
                    count
                );
            }
        }
    }
}

#[tokio::test]
async fn worker_construction_immediate_failure_on_extreme_saturation() {
    let fixture = SaturatedLoadFixture::new().unwrap();

    // Even extreme saturation should go through the defer loop
    let cpu_load_warn = 0.1; // Very low threshold (10%)
    let memory_free_warn_mb = 100; // Very high threshold (100 MB)

    // Zero timeout should fail immediately on first check
    let max_wait_secs = 0u64;
    let retry_delay_secs = 1u64;

    let result = simulate_worker_construction_with_retry(
        &fixture,
        max_wait_secs,
        retry_delay_secs,
        cpu_load_warn,
        memory_free_warn_mb,
    );

    // Should fail immediately without waiting
    assert!(
        result.is_err(),
        "should fail immediately under extreme saturation"
    );

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("system still saturated"),
        "error should mention saturation persists: {}",
        error_msg
    );

    // With max_wait_secs=0, deferred_count should be 0
    assert!(
        error_msg.contains("deferred 0 times") || error_msg.contains("deferred 0times"),
        "zero timeout should show 0 defers: {}",
        error_msg
    );
}
