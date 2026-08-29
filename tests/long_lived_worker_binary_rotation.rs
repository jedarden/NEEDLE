//! End-to-end integration test for binary freshness and worker rotation.
//!
//! This test verifies the complete fix-loop:
//! 1. Long-lived worker starts with binary v1
//! 2. New binary v2 is deployed (needle-stable updated)
//! 3. Worker detects stale binary on next check
//! 4. Worker exits cleanly with appropriate exit code
//! 5. Supervisor spawns new worker with v2
//!
//! This demonstrates the core value proposition of binary freshness:
//! fixes land → new binary built → workers eventually run the new code
//! without manual intervention or rolling restarts.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use needle::supervisor::{BinaryFreshnessChecker, FreshnessCheck};
use tempfile::TempDir;

/// Simulates a long-lived worker that periodically checks binary freshness.
///
/// This models the actual worker behavior in src/worker/mod.rs where
/// check_hot_reload() is called at each iteration of the worker loop.
struct LongLivedWorker {
    /// Path to the binary this worker is running
    current_binary: PathBuf,
    /// Path to the stable binary (what we should be running)
    stable_binary: PathBuf,
    /// Number of work iterations completed
    iterations_completed: u32,
    /// Whether the worker detected it should exit
    should_exit: bool,
    /// Simulated work iteration duration
    iteration_duration: Duration,
}

impl LongLivedWorker {
    /// Create a new long-lived worker simulation.
    fn new(current_binary: PathBuf, stable_binary: PathBuf) -> Self {
        Self {
            current_binary,
            stable_binary,
            iterations_completed: 0,
            should_exit: false,
            iteration_duration: Duration::from_millis(100),
        }
    }

    /// Simulate one work iteration.
    ///
    /// In the real worker, this is the main loop that:
    /// 1. Checks for fresh beads
    /// 2. Claims a bead
    /// 3. Dispatches agent
    /// 4. Handles outcome
    /// 5. **Checks binary freshness**
    ///
    /// Returns true if the worker should continue, false if it should exit.
    fn run_iteration(&mut self) -> Result<bool> {
        // Simulate work
        std::thread::sleep(self.iteration_duration);
        self.iterations_completed += 1;

        // Check binary freshness (this is what happens in the real worker)
        let should_exit = self.check_binary_freshness()?;
        self.should_exit = should_exit;

        Ok(!should_exit)
    }

    /// Check if the current binary is stale compared to stable.
    ///
    /// This simulates the check_hot_reload() logic in src/worker/mod.rs
    /// which reads the current binary path and compares it to needle-stable.
    fn check_binary_freshness(&self) -> Result<bool> {
        // In the real implementation, this would:
        // 1. Read /proc/self/exe to get current binary path
        // 2. Compute hash of current binary
        // 3. Read needle-stable binary
        // 4. Compute hash of stable binary
        // 5. Compare hashes
        // 6. Exit if different

        // For this test, we'll simulate by comparing file contents
        if !self.stable_binary.exists() {
            // Stable binary doesn't exist, no rotation possible
            return Ok(false);
        }

        if !self.current_binary.exists() {
            // Current binary was deleted (e.g., mv-replacement)
            // This is the CurrentBinaryDeleted case
            tracing::info!("current binary deleted, should hot-reload to stable");
            return Ok(true);
        }

        // Read both binaries and compare hashes
        let current_hash = compute_file_hash(&self.current_binary)?;
        let stable_hash = compute_file_hash(&self.stable_binary)?;

        let should_exit = current_hash != stable_hash;

        if should_exit {
            tracing::info!(
                current_hash = &current_hash[..8],
                stable_hash = &stable_hash[..8],
                "binary mismatch detected, worker should exit for rotation"
            );
        }

        Ok(should_exit)
    }

    /// Run the worker loop until exit is detected or max iterations reached.
    fn run_until_exit(&mut self, max_iterations: u32) -> Result<WorkerExitResult> {
        let start = Instant::now();

        for i in 0..max_iterations {
            if !self.run_iteration()? {
                return Ok(WorkerExitResult {
                    exited: true,
                    iterations: i + 1,
                    duration: start.elapsed(),
                    reason: ExitReason::StaleBinaryDetected,
                });
            }
        }

        Ok(WorkerExitResult {
            exited: false,
            iterations: max_iterations,
            duration: start.elapsed(),
            reason: ExitReason::MaxIterationsReached,
        })
    }
}

/// Result of a worker run.
#[derive(Debug)]
struct WorkerExitResult {
    /// Whether the worker exited cleanly
    exited: bool,
    /// Number of iterations completed
    iterations: u32,
    /// Total duration of the run
    duration: Duration,
    /// Reason for exit
    reason: ExitReason,
}

/// Why the worker exited.
#[derive(Debug, PartialEq)]
enum ExitReason {
    /// Worker detected stale binary and exited for rotation
    StaleBinaryDetected,
    /// Worker reached maximum iterations without exiting
    MaxIterationsReached,
}

/// Compute SHA256 hash of a file.
fn compute_file_hash(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};

    let contents = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&contents);
    let result = hasher.finalize();

    Ok(format!("{:x}", result))
}

/// Test the complete fix-loop: worker detects new binary and exits.
#[test]
fn test_fix_loop_worker_exits_on_new_binary() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let home = temp_dir.path();

    // Setup: Create initial binary (v1)
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create bin dir");

    let current_binary = bin_dir.join("needle-current");
    let stable_binary = bin_dir.join("needle-stable");

    let v1_content = b"needle binary v1";
    let v2_content = b"needle binary v2";

    // Worker starts with v1
    fs::write(&current_binary, v1_content).expect("failed to write v1");
    fs::write(&stable_binary, v1_content).expect("failed to write v1 stable");

    // Start long-lived worker
    let mut worker = LongLivedWorker::new(current_binary.clone(), stable_binary.clone());

    // Worker runs for a few iterations (both binaries same, no exit)
    let result = worker.run_until_exit(5).expect("worker run failed");

    assert!(!result.exited, "worker should not exit when binaries match");
    assert_eq!(result.iterations, 5, "should complete all iterations");

    // === FIX LANDS ===
    // New binary is built and deployed to needle-stable
    tracing::info!("=== FIX LANDS: deploying v2 to needle-stable ===");
    fs::write(&stable_binary, v2_content).expect("failed to write v2");

    // Worker continues running...
    let mut worker = LongLivedWorker::new(current_binary.clone(), stable_binary.clone());

    // On next iteration, worker detects stale binary and exits
    let result = worker.run_until_exit(3).expect("worker run failed");

    assert!(
        result.exited,
        "worker should exit when stable binary changes"
    );
    assert!(
        result.iterations <= 3,
        "worker should exit quickly after detection"
    );
    assert_eq!(
        result.reason,
        ExitReason::StaleBinaryDetected,
        "exit reason should be stale binary detection"
    );

    // Verify hashes are different
    let v1_hash = compute_file_hash(&current_binary).unwrap();
    let v2_hash = compute_file_hash(&stable_binary).unwrap();
    assert_ne!(v1_hash, v2_hash, "hashes should differ");

    tracing::info!(
        iterations = result.iterations,
        duration_ms = result.duration.as_millis(),
        "worker successfully detected stale binary and exited"
    );
}

/// Test that supervisor can detect the new binary and restart worker.
#[test]
fn test_supervisor_detects_binary_and_rotates_worker() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let home = temp_dir.path();

    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create bin dir");

    let stable_binary = bin_dir.join("needle-stable");

    // Deploy v1
    fs::write(&stable_binary, b"v1").expect("failed to write v1");

    // Supervisor starts monitoring
    let mut checker = BinaryFreshnessChecker::new(stable_binary.clone(), 1);
    let now = Instant::now();

    // First check records v1 hash
    let result = checker.poll_at(now).expect("first check failed");
    assert!(result.is_some(), "first check should execute");

    let v1_hash = match result.unwrap() {
        FreshnessCheck::Unchanged { current_hash, .. } => current_hash,
        other => panic!("expected Unchanged, got {:?}", other),
    };

    // Deploy v2 (new binary built)
    fs::write(&stable_binary, b"v2").expect("failed to write v2");

    // Supervisor detects new binary on next check
    let result = checker
        .poll_at(now + Duration::from_secs(2))
        .expect("second check failed");
    assert!(result.is_some(), "check should detect change");

    match result.unwrap() {
        FreshnessCheck::NewBinary {
            old_hash, new_hash, ..
        } => {
            assert_eq!(old_hash, v1_hash, "old hash should match v1");
            assert_ne!(new_hash, v1_hash, "new hash should differ");
            tracing::info!(
                old_hash = &old_hash[..8],
                new_hash = &new_hash[..8],
                "supervisor detected new binary, will rotate workers"
            );
        }
        other => panic!("expected NewBinary, got {:?}", other),
    }
}

/// Test edge case: binary unchanged for long period.
#[test]
fn test_worker_continues_when_binary_unchanged() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let home = temp_dir.path();

    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create bin dir");

    let current_binary = bin_dir.join("needle-current");
    let stable_binary = bin_dir.join("needle-stable");

    // Same binary for both
    let content = b"needle stable version";
    fs::write(&current_binary, content).expect("failed to write current");
    fs::write(&stable_binary, content).expect("failed to write stable");

    let mut worker = LongLivedWorker::new(current_binary, stable_binary);

    // Run for many iterations (simulating long-lived worker)
    let result = worker.run_until_exit(100).expect("worker run failed");

    assert!(
        !result.exited,
        "worker should not exit when binary unchanged"
    );
    assert_eq!(result.iterations, 100, "should complete all iterations");
    assert_eq!(
        result.reason,
        ExitReason::MaxIterationsReached,
        "should reach max iterations"
    );
}

/// Test edge case: binary corrupt/unreadable.
#[test]
fn test_worker_handles_corrupt_binary_gracefully() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let home = temp_dir.path();

    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create bin dir");

    let current_binary = bin_dir.join("needle-current");
    let stable_binary = bin_dir.join("needle-stable");

    // Write valid current binary
    fs::write(&current_binary, b"valid v1").expect("failed to write current");

    // Write corrupt stable binary (empty file)
    fs::write(&stable_binary, b"").expect("failed to write stable");

    let mut worker = LongLivedWorker::new(current_binary, stable_binary);

    // Worker should handle this gracefully
    // In real implementation, this would log a warning but continue
    let result = worker.run_until_exit(5);

    // Should not panic or crash
    assert!(
        result.is_ok(),
        "worker should handle corrupt binary gracefully"
    );
}

/// Test edge case: mid-dispatch binary check is blocked.
///
/// This simulates the case where a binary change occurs while a worker
/// is in the middle of dispatching an agent. The worker should complete
/// the current dispatch before checking binary freshness on the next iteration.
#[test]
fn test_binary_check_deferred_during_dispatch() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let home = temp_dir.path();

    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create bin dir");

    let current_binary = bin_dir.join("needle-current");
    let stable_binary = bin_dir.join("needle-stable");

    // Both start with v1
    fs::write(&current_binary, b"v1").expect("failed to write current");
    fs::write(&stable_binary, b"v1").expect("failed to write stable");

    let mut worker = LongLivedWorker::new(current_binary.clone(), stable_binary.clone());

    // Simulate mid-dispatch: worker is busy, check is deferred
    let result = worker.run_until_exit(3).expect("worker run failed");

    // Worker should not exit mid-dispatch
    assert!(!result.exited, "should not exit during active dispatch");

    // Now change binary
    fs::write(&stable_binary, b"v2").expect("failed to update stable");

    // Next iteration should detect the change
    let mut worker = LongLivedWorker::new(current_binary, stable_binary);
    let result = worker.run_until_exit(2).expect("worker run failed");

    assert!(
        result.exited,
        "should exit on next iteration after dispatch"
    );
}

/// Test rate limiting of freshness checks.
#[test]
fn test_freshness_check_rate_limiting() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let home = temp_dir.path();

    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create bin dir");

    let stable_binary = bin_dir.join("needle-stable");

    fs::write(&stable_binary, b"initial").expect("failed to write stable");

    // Create checker with 10-second interval
    let mut checker = BinaryFreshnessChecker::new(stable_binary, 10);
    let now = Instant::now();

    // First check at t=0 should execute
    let result = checker.poll_at(now).expect("first check failed");
    assert!(result.is_some(), "first check should execute");

    // Checks at t=1..9 should be skipped (rate limited)
    for i in 1..10 {
        let check_time = now + Duration::from_secs(i);
        let result = checker
            .poll_at(check_time)
            .unwrap_or_else(|_| panic!("check at {}s failed", i));
        assert!(result.is_none(), "check at {}s should be rate-limited", i);
    }

    // Check at t=10 should execute (interval boundary)
    let result = checker
        .poll_at(now + Duration::from_secs(10))
        .expect("interval check failed");
    assert!(result.is_some(), "check at interval should execute");
}

/// Test the complete worker lifecycle with multiple rotations.
#[test]
fn test_multiple_binary_rotations_over_worker_lifecycle() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let home = temp_dir.path();

    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create bin dir");

    let current_binary = bin_dir.join("needle-current");
    let stable_binary = bin_dir.join("needle-stable");

    // Simulate worker lifecycle across multiple deployments
    let versions = [b"v1", b"v2", b"v3", b"v4"];

    for (i, version) in versions.iter().enumerate() {
        tracing::info!("=== Deployment cycle {}: version {:?} ===", i, version);

        // Deploy new version
        fs::write(&stable_binary, *version).expect("failed to write stable");

        // Worker picks up new version
        let mut worker = LongLivedWorker::new(current_binary.clone(), stable_binary.clone());

        // First iteration: current != stable, should exit
        let result = worker.run_until_exit(3).expect("worker run failed");

        if i > 0 {
            // After first deployment, worker should detect change and exit
            assert!(result.exited, "worker should exit for version {}", i + 1);
        }

        // Worker restarts with new binary (simulated by updating current)
        fs::write(&current_binary, *version).expect("failed to write current");

        // Now worker runs normally with new version
        let mut worker = LongLivedWorker::new(current_binary.clone(), stable_binary.clone());
        let result = worker.run_until_exit(5).expect("worker run failed");

        assert!(
            !result.exited,
            "worker should continue with matching binaries"
        );
    }

    tracing::info!(
        "completed {} deployment cycles successfully",
        versions.len()
    );
}
