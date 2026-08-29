//! Binary freshness detection for supervisor.
//!
//! Monitors the worker binary (typically needle-stable) for changes and
//! notifies the supervisor when a new version is detected, triggering
//! graceful worker rotation.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Result of checking binary freshness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshnessCheck {
    /// No change detected — binary is still fresh.
    Unchanged {
        /// Path to the binary being monitored.
        binary_path: PathBuf,
        /// Current hash of the binary.
        current_hash: String,
    },
    /// New binary detected — hash has changed.
    NewBinary {
        /// Path to the new binary.
        binary_path: PathBuf,
        /// Hash of the old binary.
        old_hash: String,
        /// Hash of the new binary.
        new_hash: String,
    },
    /// Binary no longer exists at the expected path.
    BinaryMissing {
        /// Path where the binary was expected.
        binary_path: PathBuf,
    },
    /// Check failed due to an error.
    CheckFailed {
        /// Path that was checked.
        binary_path: PathBuf,
        /// Error message.
        error: String,
    },
}

/// Binary freshness checker.
///
/// Monitors a binary path for changes by computing and comparing SHA256 hashes.
/// This detects both file modifications and replacements (e.g., when an upgrade
/// writes a new binary to the same path).
pub struct BinaryFreshnessChecker {
    /// Path to the binary to monitor.
    binary_path: PathBuf,
    /// Last computed hash (if any).
    last_hash: Option<String>,
    /// Last check time (for rate limiting).
    last_check: Option<Instant>,
    /// Minimum interval between checks.
    check_interval: Duration,
}

impl BinaryFreshnessChecker {
    /// Create a new freshness checker for the given binary path.
    ///
    /// # Arguments
    /// * `binary_path` - Path to the binary to monitor
    /// * `check_interval_secs` - Minimum seconds between checks
    ///
    /// # Returns
    /// * `Self` - New checker instance
    pub fn new(binary_path: PathBuf, check_interval_secs: u64) -> Self {
        Self {
            binary_path,
            last_hash: None,
            last_check: None,
            check_interval: Duration::from_secs(check_interval_secs.max(1)),
        }
    }

    /// Create a checker that monitors needle-stable in the given workspace.
    ///
    /// # Arguments
    /// * `needle_home` - Path to the needle workspace (containing bin/needle-stable)
    /// * `check_interval_secs` - Minimum seconds between checks
    ///
    /// # Returns
    /// * `Self` - New checker instance
    pub fn for_needle_stable(needle_home: &Path, check_interval_secs: u64) -> Self {
        let stable_path = needle_home.join("bin").join("needle-stable");
        Self::new(stable_path, check_interval_secs)
    }

    /// Check binary freshness using the current monotonic time.
    ///
    /// Returns `None` if the check was skipped due to the minimum interval.
    /// Returns `Some(FreshnessCheck)` if a check was performed.
    ///
    /// # Returns
    /// * `Ok(Some(FreshnessCheck))` - Check was performed
    /// * `Ok(None)` - Check was skipped (too soon since last check)
    /// * `Err(anyhow::Error)` - If the check itself failed
    pub fn poll(&mut self) -> Result<Option<FreshnessCheck>> {
        self.poll_at(Instant::now())
    }

    /// Check binary freshness at an explicit monotonic time.
    ///
    /// This keeps interval behavior deterministic in tests without sleeping.
    ///
    /// # Arguments
    /// * `now` - Current monotonic time
    ///
    /// # Returns
    /// * `Ok(Some(FreshnessCheck))` - Check was performed
    /// * `Ok(None)` - Check was skipped (too soon since last check)
    /// * `Err(anyhow::Error)` - If the check itself failed
    pub fn poll_at(&mut self, now: Instant) -> Result<Option<FreshnessCheck>> {
        // Enforce minimum check interval
        if self
            .last_check
            .is_some_and(|last| now.duration_since(last) < self.check_interval)
        {
            return Ok(None);
        }

        self.last_check = Some(now);
        let check_result = self.check_freshness()?;

        // Update last_hash only on successful checks
        if let Some(FreshnessCheck::Unchanged { current_hash, .. }) = &check_result {
            self.last_hash = Some(current_hash.clone());
        } else if let Some(FreshnessCheck::NewBinary { new_hash, .. }) = &check_result {
            self.last_hash = Some(new_hash.clone());
        }

        Ok(check_result)
    }

    /// Perform the actual freshness check without interval enforcement.
    ///
    /// Computes the current binary hash and compares it to the last known hash.
    ///
    /// # Returns
    /// * `Ok(Some(FreshnessCheck))` - Check result
    /// * `Ok(None)` - Not applicable
    fn check_freshness(&self) -> Result<Option<FreshnessCheck>> {
        // Check if binary exists
        if !self.binary_path.exists() {
            return Ok(Some(FreshnessCheck::BinaryMissing {
                binary_path: self.binary_path.clone(),
            }));
        }

        // Compute current hash - return CheckFailed if hashing fails
        let current_hash = match compute_binary_hash(&self.binary_path) {
            Ok(hash) => hash,
            Err(e) => {
                return Ok(Some(FreshnessCheck::CheckFailed {
                    binary_path: self.binary_path.clone(),
                    error: e.to_string(),
                }));
            }
        };

        // Compare with last known hash
        match &self.last_hash {
            None => {
                // First check — record current hash, report no change
                Ok(Some(FreshnessCheck::Unchanged {
                    binary_path: self.binary_path.clone(),
                    current_hash,
                }))
            }
            Some(last_hash) => {
                if current_hash == *last_hash {
                    Ok(Some(FreshnessCheck::Unchanged {
                        binary_path: self.binary_path.clone(),
                        current_hash,
                    }))
                } else {
                    Ok(Some(FreshnessCheck::NewBinary {
                        binary_path: self.binary_path.clone(),
                        old_hash: last_hash.clone(),
                        new_hash: current_hash,
                    }))
                }
            }
        }
    }

    /// Get the binary path being monitored.
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    /// Get the last computed hash (if any).
    pub fn last_hash(&self) -> Option<&str> {
        self.last_hash.as_deref()
    }
}

/// Compute SHA256 hash of a binary file.
///
/// # Arguments
/// * `path` - Path to the binary file
///
/// # Returns
/// * `Ok(String)` - Hex-encoded SHA256 hash
/// * `Err(anyhow::Error)` - If reading or hashing failed
fn compute_binary_hash(path: &Path) -> Result<String> {
    let contents = fs::read(path).with_context(|| {
        format!(
            "failed to read binary for hash computation: {}",
            path.display()
        )
    })?;

    let mut hasher = Sha256::new();
    hasher.update(&contents);
    let result = hasher.finalize();

    Ok(format!("{:x}", result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn checker_detects_binary_change() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        // Create initial binary
        fs::write(&binary_path, b"v1").expect("failed to write initial binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
        // Wall-clock poll() is rate-limited by check_interval, so two calls in
        // the same second return None on the second — drive the interval with
        // poll_at (the deterministic API) like checker_respects_minimum_interval.
        let now = Instant::now();

        // First check should record hash
        let result = checker.poll_at(now).expect("first check failed");
        assert!(result.is_some());

        match result.unwrap() {
            FreshnessCheck::Unchanged { current_hash, .. } => {
                assert!(!current_hash.is_empty());
            }
            other => panic!("expected Unchanged, got {:?}", other),
        }

        // Update binary
        fs::write(&binary_path, b"v2").expect("failed to update binary");

        // Second check (past the interval) should detect change
        let result = checker
            .poll_at(now + Duration::from_secs(2))
            .expect("second check failed");
        assert!(result.is_some());

        match result.unwrap() {
            FreshnessCheck::NewBinary {
                old_hash, new_hash, ..
            } => {
                assert!(!old_hash.is_empty());
                assert!(!new_hash.is_empty());
                assert_ne!(old_hash, new_hash);
            }
            other => panic!("expected NewBinary, got {:?}", other),
        }
    }

    #[test]
    fn checker_respects_minimum_interval() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"v1").expect("failed to write binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path, 10);
        let now = Instant::now();

        // First check should succeed
        let result = checker.poll_at(now).expect("first check failed");
        assert!(result.is_some());

        // Immediate second check should be skipped
        let result = checker.poll_at(now).expect("second check failed");
        assert!(result.is_none());

        // Check after interval should succeed
        let later = now + Duration::from_secs(10);
        let result = checker.poll_at(later).expect("third check failed");
        assert!(result.is_some());
    }

    #[test]
    fn checker_handles_missing_binary() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("nonexistent-binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);

        let result = checker.poll().expect("check failed");
        assert!(result.is_some());

        match result.unwrap() {
            FreshnessCheck::BinaryMissing { binary_path: path } => {
                assert_eq!(path, binary_path);
            }
            other => panic!("expected BinaryMissing, got {:?}", other),
        }
    }

    #[test]
    fn for_needle_stable_creates_correct_path() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");

        let checker = BinaryFreshnessChecker::for_needle_stable(temp_dir.path(), 60);

        let expected_path = temp_dir.path().join("bin").join("needle-stable");
        assert_eq!(checker.binary_path(), expected_path);
    }

    #[test]
    fn hash_is_consistent_for_same_binary() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"consistent content").expect("failed to write binary");

        let hash1 = compute_binary_hash(&binary_path).expect("first hash failed");
        let hash2 = compute_binary_hash(&binary_path).expect("second hash failed");

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn hash_differs_for_different_binaries() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"content A").expect("failed to write binary");
        let hash_a = compute_binary_hash(&binary_path).expect("hash A failed");

        fs::write(&binary_path, b"content B").expect("failed to update binary");
        let hash_b = compute_binary_hash(&binary_path).expect("hash B failed");

        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn checker_first_poll_records_initial_hash() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"initial content").expect("failed to write binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
        let now = Instant::now();

        // First poll should record initial hash and report unchanged
        let result = checker.poll_at(now).expect("first poll failed");
        assert!(result.is_some());

        match result.unwrap() {
            FreshnessCheck::Unchanged { current_hash, .. } => {
                assert!(!current_hash.is_empty());
                assert_eq!(checker.last_hash(), Some(current_hash.as_str()));
            }
            other => panic!("expected Unchanged on first poll, got {:?}", other),
        }
    }

    #[test]
    fn checker_multiple_sequential_polls() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"v1").expect("failed to write binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 5);
        let now = Instant::now();

        // First poll at t=0
        let result = checker.poll_at(now).expect("poll 1 failed");
        assert!(result.is_some());
        assert_eq!(checker.last_hash().unwrap().len(), 64); // SHA256 hex length

        // Polls at t=1, t=3 should be skipped (before interval)
        assert!(checker
            .poll_at(now + Duration::from_secs(1))
            .expect("poll 2 failed")
            .is_none());
        assert!(checker
            .poll_at(now + Duration::from_secs(3))
            .expect("poll 3 failed")
            .is_none());

        // Poll at t=5 should execute (at interval)
        let result = checker
            .poll_at(now + Duration::from_secs(5))
            .expect("poll 4 failed");
        assert!(result.is_some());

        // Update binary
        fs::write(&binary_path, b"v2").expect("failed to update binary");

        // Poll at t=10 should detect change
        let result = checker
            .poll_at(now + Duration::from_secs(10))
            .expect("poll 5 failed");
        assert!(result.is_some());

        match result.unwrap() {
            FreshnessCheck::NewBinary {
                old_hash, new_hash, ..
            } => {
                assert_ne!(old_hash, new_hash);
            }
            other => panic!("expected NewBinary, got {:?}", other),
        }
    }

    #[test]
    fn checker_boundary_conditions() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"boundary test").expect("failed to write binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path, 2);
        let now = Instant::now();

        // First poll at t=0
        assert!(checker.poll_at(now).expect("poll 0 failed").is_some());

        // Poll at t=1 should be skipped
        assert!(checker
            .poll_at(now + Duration::from_secs(1))
            .expect("poll 1 failed")
            .is_none());

        // Poll at exactly t=2 (interval boundary) should execute
        assert!(checker
            .poll_at(now + Duration::from_secs(2))
            .expect("poll 2 failed")
            .is_some());

        // Poll at t=4 (next boundary) should execute
        assert!(checker
            .poll_at(now + Duration::from_secs(4))
            .expect("poll 4 failed")
            .is_some());
    }

    #[test]
    fn checker_skipped_poll_does_not_update_state() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"initial").expect("failed to write binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 10);
        let now = Instant::now();

        // First poll
        let result = checker.poll_at(now).expect("first poll failed").unwrap();
        let initial_hash = match result {
            FreshnessCheck::Unchanged {
                ref current_hash, ..
            } => current_hash.clone(),
            _ => panic!("expected Unchanged"),
        };

        // Update binary immediately after first poll
        fs::write(&binary_path, b"updated").expect("failed to update binary");

        // Poll before interval should be skipped and NOT update hash
        let skipped = checker
            .poll_at(now + Duration::from_secs(5))
            .expect("skipped poll failed");
        assert!(skipped.is_none());

        // Hash should still be the initial hash (not updated)
        assert_eq!(checker.last_hash(), Some(initial_hash.as_str()));

        // Poll after interval should detect the change
        let result = checker
            .poll_at(now + Duration::from_secs(10))
            .expect("interval poll failed")
            .unwrap();
        match result {
            FreshnessCheck::NewBinary {
                old_hash, new_hash, ..
            } => {
                assert_eq!(old_hash, initial_hash);
                assert_ne!(new_hash, initial_hash);
            }
            other => panic!("expected NewBinary, got {:?}", other),
        }
    }

    #[test]
    fn checker_immediate_second_poll_is_skipped() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"test").expect("failed to write binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path, 60);
        let now = Instant::now();

        // First poll should execute
        assert!(checker.poll_at(now).expect("first poll failed").is_some());

        // Immediate second poll should be skipped
        assert!(checker.poll_at(now).expect("second poll failed").is_none());

        // Poll 1 nanosecond later should still be skipped
        assert!(checker
            .poll_at(now + Duration::from_nanos(1))
            .expect("nanosecond poll failed")
            .is_none());
    }

    #[test]
    fn checker_persists_last_hash_across_polls() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"persistent").expect("failed to write binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
        let now = Instant::now();

        // First poll records hash
        let result = checker.poll_at(now).expect("first poll failed").unwrap();
        let first_hash = match result {
            FreshnessCheck::Unchanged {
                ref current_hash, ..
            } => current_hash.clone(),
            _ => panic!("expected Unchanged"),
        };

        // Multiple subsequent polls should report the same hash
        for i in 1..5 {
            let poll_time = now + Duration::from_secs(i as u64);
            let result = checker
                .poll_at(poll_time)
                .expect("subsequent poll failed")
                .unwrap();
            match result {
                FreshnessCheck::Unchanged {
                    ref current_hash, ..
                } => {
                    assert_eq!(
                        current_hash, &first_hash,
                        "hash should remain consistent across poll {}",
                        i
                    );
                }
                other => panic!("expected Unchanged on poll {}, got {:?}", i, other),
            }
        }

        // Verify last_hash is still the original
        assert_eq!(checker.last_hash(), Some(first_hash.as_str()));
    }

    #[test]
    fn checker_detects_change_after_multiple_unchanged_polls() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"original").expect("failed to write binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path.clone(), 1);
        let now = Instant::now();

        // Perform several unchanged polls
        for i in 0..3 {
            let poll_time = now + Duration::from_secs(i as u64);
            let result = checker.poll_at(poll_time).expect("poll {} failed").unwrap();
            match result {
                FreshnessCheck::Unchanged { .. } => {}
                other => panic!("expected Unchanged on poll {}, got {:?}", i, other),
            }
        }

        // Now change the binary
        fs::write(&binary_path, b"changed").expect("failed to update binary");

        // Next poll should detect the change
        let result = checker
            .poll_at(now + Duration::from_secs(3))
            .expect("change detection poll failed")
            .unwrap();
        match result {
            FreshnessCheck::NewBinary {
                old_hash, new_hash, ..
            } => {
                assert!(!old_hash.is_empty());
                assert!(!new_hash.is_empty());
                assert_ne!(old_hash, new_hash);
            }
            other => panic!("expected NewBinary, got {:?}", other),
        }
    }

    #[test]
    fn checker_one_second_interval_works() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"1-second test").expect("failed to write binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path, 1);
        let now = Instant::now();

        // Test with 1-second interval (minimum practical interval)
        assert!(checker.poll_at(now).expect("poll at 0s failed").is_some());
        assert!(checker
            .poll_at(now + Duration::from_millis(999))
            .expect("poll at 999ms failed")
            .is_none());
        assert!(checker
            .poll_at(now + Duration::from_secs(1))
            .expect("poll at 1s failed")
            .is_some());
        assert!(checker
            .poll_at(now + Duration::from_millis(1999))
            .expect("poll at 1.999s failed")
            .is_none());
        assert!(checker
            .poll_at(now + Duration::from_secs(2))
            .expect("poll at 2s failed")
            .is_some());
    }

    #[test]
    fn checker_returns_none_for_skipped_checks() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let binary_path = temp_dir.path().join("test-binary");

        fs::write(&binary_path, b"test content").expect("failed to write binary");

        let mut checker = BinaryFreshnessChecker::new(binary_path, 10);
        let now = Instant::now();

        // First poll should return Some
        let result = checker.poll_at(now).expect("first poll failed");
        assert!(
            result.is_some(),
            "first poll should return Some(FreshnessCheck)"
        );

        // Immediate poll should return None (skipped)
        let result = checker.poll_at(now).expect("immediate poll failed");
        assert!(
            result.is_none(),
            "immediate poll should return None (skipped)"
        );

        // Poll before interval should return None
        let result = checker
            .poll_at(now + Duration::from_secs(5))
            .expect("early poll failed");
        assert!(
            result.is_none(),
            "poll before interval should return None (skipped)"
        );

        // Poll after interval should return Some
        let result = checker
            .poll_at(now + Duration::from_secs(10))
            .expect("interval poll failed");
        assert!(
            result.is_some(),
            "poll after interval should return Some(FreshnessCheck)"
        );
    }
}
