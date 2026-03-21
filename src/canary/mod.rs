//! Canary testing for self-modification pipeline.
//!
//! The canary runner tests a :testing binary against a controlled workspace
//! with known test beads and expected outcomes. If all tests pass, the
//! :testing binary can be promoted to :stable.
//!
//! ## Release Channels
//!
//! - `needle-testing` — Newly compiled binary awaiting canary validation
//! - `needle-stable` — Current production binary
//! - `needle-stable.prev` — Previous stable binary (backup for rollback)
//! - `needle` → symlink to `needle-stable`
//!
//! ## Canary Test Workspace
//!
//! The canary workspace (`~/.needle/canary/`) contains:
//! - `.beads/` — Bead database with test beads
//! - `expected/` — Expected outcome files for each test bead
//!
//! ## Test Scenarios
//!
//! - Happy path: Agent completes successfully, bead closed
//! - Failure path: Agent fails, bead released with failure label
//! - Timeout: Agent times out, bead deferred
//! - State machine integrity: Worker state transitions are valid

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// CanaryTestResult
// ──────────────────────────────────────────────────────────────────────────────

/// Outcome of a single canary test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CanaryTestResult {
    /// Test passed — actual outcome matches expected.
    Passed {
        /// Test bead ID.
        bead_id: String,
        /// Expected outcome.
        expected: ExpectedOutcome,
        /// Actual outcome.
        actual: ActualOutcome,
    },
    /// Test failed — actual outcome does not match expected.
    Failed {
        /// Test bead ID.
        bead_id: String,
        /// Expected outcome.
        expected: ExpectedOutcome,
        /// Actual outcome.
        actual: ActualOutcome,
        /// Reason for failure.
        reason: String,
    },
    /// Test timed out before completion.
    TimedOut {
        /// Test bead ID.
        bead_id: String,
        /// Elapsed time before timeout.
        elapsed_secs: u64,
    },
    /// Test could not run (e.g., binary not found, workspace error).
    Error {
        /// Test bead ID.
        bead_id: String,
        /// Error message.
        message: String,
    },
}

impl CanaryTestResult {
    /// Returns true if this result represents a pass.
    pub fn is_pass(&self) -> bool {
        matches!(self, CanaryTestResult::Passed { .. })
    }

    /// Returns the bead ID for this test result.
    pub fn bead_id(&self) -> &str {
        match self {
            CanaryTestResult::Passed { bead_id, .. } => bead_id,
            CanaryTestResult::Failed { bead_id, .. } => bead_id,
            CanaryTestResult::TimedOut { bead_id, .. } => bead_id,
            CanaryTestResult::Error { bead_id, .. } => bead_id,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ExpectedOutcome
// ──────────────────────────────────────────────────────────────────────────────

/// Expected outcome for a canary test bead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExpectedOutcome {
    /// Agent should complete successfully (exit 0).
    Success {
        /// Expected final bead status (default: "done").
        #[serde(default = "ExpectedOutcome::default_final_status")]
        final_status: String,
        /// Expected labels to be present on the bead.
        #[serde(default)]
        labels: Vec<String>,
    },
    /// Agent should fail (non-zero exit).
    Failure {
        /// Expected final bead status.
        #[serde(default = "ExpectedOutcome::default_failure_status")]
        final_status: String,
        /// Expected failure labels (e.g., "failure-count:1").
        #[serde(default)]
        labels: Vec<String>,
    },
    /// Agent should timeout.
    Timeout {
        /// Expected final bead status.
        #[serde(default = "ExpectedOutcome::default_timeout_status")]
        final_status: String,
    },
    /// Worker state machine should transition correctly.
    StateMachine {
        /// Expected state transitions (e.g., ["BOOTING", "SELECTING", "CLAIMING", ...]).
        transitions: Vec<String>,
    },
}

impl ExpectedOutcome {
    fn default_final_status() -> String {
        "done".to_string()
    }
    fn default_failure_status() -> String {
        "open".to_string()
    }
    fn default_timeout_status() -> String {
        "open".to_string()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ActualOutcome
// ──────────────────────────────────────────────────────────────────────────────

/// Actual outcome observed from running a canary test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActualOutcome {
    /// Agent exit code (if applicable).
    pub exit_code: Option<i32>,
    /// Final bead status.
    pub final_status: String,
    /// Labels present on the bead after execution.
    pub labels: Vec<String>,
    /// Worker state transitions observed (if tracked).
    #[serde(default)]
    pub state_transitions: Vec<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// CanaryReport
// ──────────────────────────────────────────────────────────────────────────────

/// Summary report from running the full canary test suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryReport {
    /// Path to the testing binary that was tested.
    pub testing_binary: PathBuf,
    /// Canary workspace path.
    pub workspace: PathBuf,
    /// Total number of tests run.
    pub total_tests: usize,
    /// Number of tests that passed.
    pub passed: usize,
    /// Number of tests that failed.
    pub failed: usize,
    /// Number of tests that timed out.
    pub timed_out: usize,
    /// Number of tests that had errors.
    pub errors: usize,
    /// Total duration of the canary run.
    pub duration_secs: u64,
    /// Individual test results.
    pub results: Vec<CanaryTestResult>,
    /// Whether the canary suite passed (all tests passed).
    pub suite_passed: bool,
}

impl CanaryReport {
    /// Returns true if all tests passed.
    pub fn can_promote(&self) -> bool {
        self.suite_passed
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// CanaryRunner
// ──────────────────────────────────────────────────────────────────────────────

/// Runs canary tests against a :testing binary.
pub struct CanaryRunner {
    /// Path to the needle home directory (~/.needle).
    needle_home: PathBuf,
    /// Path to the canary workspace.
    canary_workspace: PathBuf,
    /// Timeout for individual tests (seconds).
    test_timeout: u64,
    /// Path to the br CLI.
    br_path: PathBuf,
}

impl CanaryRunner {
    /// Create a new canary runner.
    pub fn new(needle_home: PathBuf, canary_workspace: PathBuf, test_timeout: u64) -> Self {
        let br_path = dirs_or_home(".local/bin/br");
        CanaryRunner {
            needle_home,
            canary_workspace,
            test_timeout,
            br_path,
        }
    }

    /// Path to the testing binary.
    pub fn testing_binary(&self) -> PathBuf {
        self.needle_home.join("bin/needle-testing")
    }

    /// Path to the stable binary.
    pub fn stable_binary(&self) -> PathBuf {
        self.needle_home.join("bin/needle-stable")
    }

    /// Path to the previous stable binary (backup).
    pub fn prev_binary(&self) -> PathBuf {
        self.needle_home.join("bin/needle-stable.prev")
    }

    /// Path to the needle symlink.
    pub fn symlink_path(&self) -> PathBuf {
        self.needle_home.join("bin/needle")
    }

    /// Run the full canary test suite against the :testing binary.
    ///
    /// Returns a report with pass/fail status for each test.
    pub fn run(&self) -> Result<CanaryReport> {
        let start = Instant::now();
        let testing_binary = self.testing_binary();

        // Verify testing binary exists.
        if !testing_binary.exists() {
            bail!("testing binary not found: {}", testing_binary.display());
        }

        // Verify canary workspace exists.
        if !self.canary_workspace.exists() {
            bail!(
                "canary workspace not found: {}",
                self.canary_workspace.display()
            );
        }

        // Discover test beads in the canary workspace.
        let test_beads = self.discover_test_beads()?;
        if test_beads.is_empty() {
            bail!("no test beads found in canary workspace");
        }

        let mut results = Vec::new();
        let mut passed = 0;
        let mut failed = 0;
        let mut timed_out = 0;
        let mut errors = 0;

        for bead_id in &test_beads {
            // Load expected outcome for this test.
            let expected = match self.load_expected_outcome(bead_id) {
                Ok(e) => e,
                Err(e) => {
                    results.push(CanaryTestResult::Error {
                        bead_id: bead_id.clone(),
                        message: format!("failed to load expected outcome: {e}"),
                    });
                    errors += 1;
                    continue;
                }
            };

            // Run the test.
            let result = self.run_test(bead_id, &expected, &testing_binary);
            match &result {
                CanaryTestResult::Passed { .. } => passed += 1,
                CanaryTestResult::Failed { .. } => failed += 1,
                CanaryTestResult::TimedOut { .. } => timed_out += 1,
                CanaryTestResult::Error { .. } => errors += 1,
            }
            results.push(result);
        }

        let suite_passed = failed == 0 && timed_out == 0 && errors == 0;
        let total_tests = test_beads.len();

        Ok(CanaryReport {
            testing_binary,
            workspace: self.canary_workspace.clone(),
            total_tests,
            passed,
            failed,
            timed_out,
            errors,
            duration_secs: start.elapsed().as_secs(),
            results,
            suite_passed,
        })
    }

    /// Discover test bead IDs in the canary workspace.
    fn discover_test_beads(&self) -> Result<Vec<String>> {
        let expected_dir = self.canary_workspace.join("expected");
        if !expected_dir.exists() {
            bail!(
                "expected outcomes directory not found: {}",
                expected_dir.display()
            );
        }

        let mut bead_ids = Vec::new();
        for entry in std::fs::read_dir(&expected_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    bead_ids.push(stem.to_string());
                }
            }
        }

        Ok(bead_ids)
    }

    /// Load the expected outcome for a test bead.
    fn load_expected_outcome(&self, bead_id: &str) -> Result<ExpectedOutcome> {
        let path = self
            .canary_workspace
            .join("expected")
            .join(format!("{bead_id}.yaml"));
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read expected outcome: {}", path.display()))?;
        serde_yaml::from_str(&content)
            .with_context(|| format!("invalid expected outcome YAML: {}", path.display()))
    }

    /// Run a single canary test.
    fn run_test(
        &self,
        bead_id: &str,
        expected: &ExpectedOutcome,
        testing_binary: &Path,
    ) -> CanaryTestResult {
        let start = Instant::now();

        // Run the testing binary against the canary workspace.
        let output = Command::new(testing_binary)
            .args([
                "run",
                "--workspace",
                &self.canary_workspace.display().to_string(),
                "--identifier",
                &format!("canary-{bead_id}"),
                "--count",
                "1",
            ])
            .output();

        match output {
            Ok(output) => {
                let exit_code = output.status.code();
                let actual = match self.get_actual_outcome(bead_id, exit_code) {
                    Ok(a) => a,
                    Err(e) => {
                        return CanaryTestResult::Error {
                            bead_id: bead_id.to_string(),
                            message: format!("failed to get actual outcome: {e}"),
                        };
                    }
                };

                // Compare actual vs expected.
                if self.outcomes_match(expected, &actual) {
                    CanaryTestResult::Passed {
                        bead_id: bead_id.to_string(),
                        expected: expected.clone(),
                        actual,
                    }
                } else {
                    let reason = self.mismatch_reason(expected, &actual);
                    CanaryTestResult::Failed {
                        bead_id: bead_id.to_string(),
                        expected: expected.clone(),
                        actual,
                        reason,
                    }
                }
            }
            Err(e) => {
                let elapsed = start.elapsed().as_secs();
                if elapsed >= self.test_timeout {
                    CanaryTestResult::TimedOut {
                        bead_id: bead_id.to_string(),
                        elapsed_secs: elapsed,
                    }
                } else {
                    CanaryTestResult::Error {
                        bead_id: bead_id.to_string(),
                        message: format!("failed to execute testing binary: {e}"),
                    }
                }
            }
        }
    }

    /// Get the actual outcome for a test bead by querying the bead store.
    fn get_actual_outcome(&self, bead_id: &str, exit_code: Option<i32>) -> Result<ActualOutcome> {
        // Use br to query the bead status.
        let output = Command::new(&self.br_path)
            .args(["show", bead_id, "--json"])
            .current_dir(&self.canary_workspace)
            .output()
            .context("failed to run br show")?;

        if !output.status.success() {
            bail!("br show failed for bead {}", bead_id);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let bead: serde_json::Value =
            serde_json::from_str(&stdout).context("failed to parse br show output")?;

        let final_status = bead["status"].as_str().unwrap_or("unknown").to_string();
        let labels = bead["labels"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(ActualOutcome {
            exit_code,
            final_status,
            labels,
            state_transitions: Vec::new(), // Not tracked in basic canary tests
        })
    }

    /// Check if actual outcome matches expected outcome.
    fn outcomes_match(&self, expected: &ExpectedOutcome, actual: &ActualOutcome) -> bool {
        match expected {
            ExpectedOutcome::Success {
                final_status,
                labels,
            } => {
                actual.final_status == *final_status
                    && labels.iter().all(|l| actual.labels.contains(l))
                    && actual.exit_code == Some(0)
            }
            ExpectedOutcome::Failure {
                final_status,
                labels,
            } => {
                actual.final_status == *final_status
                    && labels.iter().all(|l| actual.labels.contains(l))
                    && actual.exit_code.is_some_and(|c| c != 0)
            }
            ExpectedOutcome::Timeout { final_status } => {
                // Timeout is harder to detect from exit code alone.
                // We check that the bead was not closed (still open/in_progress).
                actual.final_status == *final_status
            }
            ExpectedOutcome::StateMachine { transitions } => {
                // Check that all expected transitions occurred.
                transitions
                    .iter()
                    .all(|t| actual.state_transitions.contains(t))
            }
        }
    }

    /// Generate a human-readable reason for mismatch.
    fn mismatch_reason(&self, expected: &ExpectedOutcome, actual: &ActualOutcome) -> String {
        match expected {
            ExpectedOutcome::Success {
                final_status,
                labels,
            } => {
                let mut reasons = Vec::new();
                if actual.final_status != *final_status {
                    reasons.push(format!(
                        "status mismatch: expected '{}', got '{}'",
                        final_status, actual.final_status
                    ));
                }
                for label in labels {
                    if !actual.labels.contains(label) {
                        reasons.push(format!("missing label: '{}'", label));
                    }
                }
                if actual.exit_code != Some(0) {
                    reasons.push(format!("expected exit 0, got {:?}", actual.exit_code));
                }
                reasons.join("; ")
            }
            ExpectedOutcome::Failure {
                final_status,
                labels,
            } => {
                let mut reasons = Vec::new();
                if actual.final_status != *final_status {
                    reasons.push(format!(
                        "status mismatch: expected '{}', got '{}'",
                        final_status, actual.final_status
                    ));
                }
                for label in labels {
                    if !actual.labels.contains(label) {
                        reasons.push(format!("missing label: '{}'", label));
                    }
                }
                if actual.exit_code == Some(0) {
                    reasons.push("expected non-zero exit, got 0".to_string());
                }
                reasons.join("; ")
            }
            ExpectedOutcome::Timeout { final_status } => {
                format!(
                    "status mismatch: expected '{}', got '{}'",
                    final_status, actual.final_status
                )
            }
            ExpectedOutcome::StateMachine { transitions } => {
                let missing: Vec<_> = transitions
                    .iter()
                    .filter(|t| !actual.state_transitions.contains(*t))
                    .collect();
                format!("missing state transitions: {:?}", missing)
            }
        }
    }

    /// Promote :testing to :stable, backing up the current :stable.
    ///
    /// This operation is atomic at the filesystem level:
    /// 1. Move :stable to :stable.prev
    /// 2. Move :testing to :stable
    /// 3. Update symlink
    pub fn promote(&self) -> Result<()> {
        let testing = self.testing_binary();
        let stable = self.stable_binary();
        let prev = self.prev_binary();
        let symlink = self.symlink_path();

        if !testing.exists() {
            bail!("testing binary not found: {}", testing.display());
        }

        // Ensure bin directory exists.
        let bin_dir = self.needle_home.join("bin");
        std::fs::create_dir_all(&bin_dir).context("failed to create bin directory")?;

        // Step 1: Remove old backup if exists.
        if prev.exists() {
            std::fs::remove_file(&prev).context("failed to remove old backup")?;
        }

        // Step 2: Move current stable to backup (if exists).
        if stable.exists() {
            std::fs::rename(&stable, &prev).context("failed to backup stable binary")?;
        }

        // Step 3: Move testing to stable.
        std::fs::rename(&testing, &stable).context("failed to promote testing to stable")?;

        // Step 4: Update symlink.
        if symlink.exists() || symlink.is_symlink() {
            std::fs::remove_file(&symlink).context("failed to remove old symlink")?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&stable, &symlink).context("failed to create symlink")?;
        #[cfg(not(unix))]
        {
            // Fallback: copy instead of symlink on non-Unix.
            std::fs::copy(&stable, &symlink).context("failed to copy stable to symlink")?;
        }

        tracing::info!(
            testing = %testing.display(),
            stable = %stable.display(),
            prev = %prev.display(),
            symlink = %symlink.display(),
            "promoted testing to stable"
        );

        Ok(())
    }

    /// Reject :testing (discard without promoting).
    ///
    /// The fleet remains on :stable unchanged.
    pub fn reject(&self) -> Result<()> {
        let testing = self.testing_binary();

        if testing.exists() {
            std::fs::remove_file(&testing).context("failed to remove testing binary")?;
            tracing::info!(path = %testing.display(), "rejected testing binary");
        } else {
            tracing::warn!("no testing binary to reject");
        }

        Ok(())
    }

    /// Rollback to the previous stable binary.
    ///
    /// Restores :stable.prev as :stable.
    pub fn rollback(&self) -> Result<()> {
        let stable = self.stable_binary();
        let prev = self.prev_binary();
        let symlink = self.symlink_path();

        if !prev.exists() {
            bail!("no previous stable binary to rollback to");
        }

        // Move current stable aside (if exists) - it becomes the new backup.
        if stable.exists() {
            // Remove the old backup first since we're about to replace it.
            let old_backup = self.needle_home.join("bin/needle-stable.rollback");
            if old_backup.exists() {
                std::fs::remove_file(&old_backup)?;
            }
            std::fs::rename(&stable, &old_backup)?;
        }

        // Restore previous stable.
        std::fs::rename(&prev, &stable).context("failed to restore previous stable")?;

        // Update symlink.
        if symlink.exists() || symlink.is_symlink() {
            std::fs::remove_file(&symlink)?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&stable, &symlink).context("failed to update symlink")?;

        tracing::info!(
            stable = %stable.display(),
            "rolled back to previous stable"
        );

        Ok(())
    }

    /// Check the current release channel status.
    pub fn status(&self) -> Result<ChannelStatus> {
        let testing = self.testing_binary();
        let stable = self.stable_binary();
        let prev = self.prev_binary();
        let symlink = self.symlink_path();

        let testing_exists = testing.exists();
        let stable_exists = stable.exists();
        let prev_exists = prev.exists();

        let symlink_target = if symlink.is_symlink() {
            std::fs::read_link(&symlink).ok()
        } else {
            None
        };

        Ok(ChannelStatus {
            testing_exists,
            stable_exists,
            prev_exists,
            symlink_target,
            testing_path: testing,
            stable_path: stable,
            prev_path: prev,
            symlink_path: symlink,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ChannelStatus
// ──────────────────────────────────────────────────────────────────────────────

/// Status of the release channels.
#[derive(Debug, Clone)]
pub struct ChannelStatus {
    /// Whether :testing binary exists.
    pub testing_exists: bool,
    /// Whether :stable binary exists.
    pub stable_exists: bool,
    /// Whether :stable.prev binary exists.
    pub prev_exists: bool,
    /// Target of the needle symlink (if it exists).
    pub symlink_target: Option<PathBuf>,
    /// Path to testing binary.
    pub testing_path: PathBuf,
    /// Path to stable binary.
    pub stable_path: PathBuf,
    /// Path to previous stable binary.
    pub prev_path: PathBuf,
    /// Path to symlink.
    pub symlink_path: PathBuf,
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Resolve a path relative to the user's home directory.
fn dirs_or_home(relative: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(relative)
    } else {
        PathBuf::from("/tmp").join(relative)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canary_test_result_is_pass() {
        let passed = CanaryTestResult::Passed {
            bead_id: "test-001".to_string(),
            expected: ExpectedOutcome::Success {
                final_status: "done".to_string(),
                labels: vec![],
            },
            actual: ActualOutcome {
                exit_code: Some(0),
                final_status: "done".to_string(),
                labels: vec![],
                state_transitions: vec![],
            },
        };
        assert!(passed.is_pass());

        let failed = CanaryTestResult::Failed {
            bead_id: "test-001".to_string(),
            expected: ExpectedOutcome::Success {
                final_status: "done".to_string(),
                labels: vec![],
            },
            actual: ActualOutcome {
                exit_code: Some(1),
                final_status: "open".to_string(),
                labels: vec![],
                state_transitions: vec![],
            },
            reason: "status mismatch".to_string(),
        };
        assert!(!failed.is_pass());
    }

    #[test]
    fn canary_test_result_bead_id() {
        let result = CanaryTestResult::TimedOut {
            bead_id: "test-timeout".to_string(),
            elapsed_secs: 300,
        };
        assert_eq!(result.bead_id(), "test-timeout");
    }

    #[test]
    fn expected_outcome_default_status() {
        assert_eq!(ExpectedOutcome::default_final_status(), "done");
        assert_eq!(ExpectedOutcome::default_failure_status(), "open");
        assert_eq!(ExpectedOutcome::default_timeout_status(), "open");
    }

    #[test]
    fn canary_report_can_promote() {
        let report = CanaryReport {
            testing_binary: PathBuf::from("/tmp/needle-testing"),
            workspace: PathBuf::from("/tmp/canary"),
            total_tests: 4,
            passed: 4,
            failed: 0,
            timed_out: 0,
            errors: 0,
            duration_secs: 60,
            results: vec![],
            suite_passed: true,
        };
        assert!(report.can_promote());

        let failed_report = CanaryReport {
            suite_passed: false,
            ..report
        };
        assert!(!failed_report.can_promote());
    }

    #[test]
    fn outcomes_match_success() {
        let runner = CanaryRunner::new(
            PathBuf::from("/tmp/.needle"),
            PathBuf::from("/tmp/canary"),
            300,
        );

        let expected = ExpectedOutcome::Success {
            final_status: "done".to_string(),
            labels: vec!["verified".to_string()],
        };

        let actual = ActualOutcome {
            exit_code: Some(0),
            final_status: "done".to_string(),
            labels: vec!["verified".to_string()],
            state_transitions: vec![],
        };

        assert!(runner.outcomes_match(&expected, &actual));

        // Mismatch on status
        let actual_wrong_status = ActualOutcome {
            final_status: "open".to_string(),
            ..actual.clone()
        };
        assert!(!runner.outcomes_match(&expected, &actual_wrong_status));

        // Missing label
        let actual_missing_label = ActualOutcome {
            labels: vec![],
            ..actual.clone()
        };
        assert!(!runner.outcomes_match(&expected, &actual_missing_label));

        // Wrong exit code
        let actual_wrong_exit = ActualOutcome {
            exit_code: Some(1),
            ..actual
        };
        assert!(!runner.outcomes_match(&expected, &actual_wrong_exit));
    }

    #[test]
    fn outcomes_match_failure() {
        let runner = CanaryRunner::new(
            PathBuf::from("/tmp/.needle"),
            PathBuf::from("/tmp/canary"),
            300,
        );

        let expected = ExpectedOutcome::Failure {
            final_status: "open".to_string(),
            labels: vec!["failure-count:1".to_string()],
        };

        let actual = ActualOutcome {
            exit_code: Some(1),
            final_status: "open".to_string(),
            labels: vec!["failure-count:1".to_string()],
            state_transitions: vec![],
        };

        assert!(runner.outcomes_match(&expected, &actual));

        // Success exit code should not match failure expectation
        let actual_success = ActualOutcome {
            exit_code: Some(0),
            ..actual
        };
        assert!(!runner.outcomes_match(&expected, &actual_success));
    }

    #[test]
    fn outcomes_match_state_machine() {
        let runner = CanaryRunner::new(
            PathBuf::from("/tmp/.needle"),
            PathBuf::from("/tmp/canary"),
            300,
        );

        let expected = ExpectedOutcome::StateMachine {
            transitions: vec![
                "BOOTING".to_string(),
                "SELECTING".to_string(),
                "CLAIMING".to_string(),
            ],
        };

        let actual = ActualOutcome {
            exit_code: Some(0),
            final_status: "done".to_string(),
            labels: vec![],
            state_transitions: vec![
                "BOOTING".to_string(),
                "SELECTING".to_string(),
                "CLAIMING".to_string(),
                "DISPATCHING".to_string(),
            ],
        };

        assert!(runner.outcomes_match(&expected, &actual));

        // Missing transition
        let actual_missing = ActualOutcome {
            state_transitions: vec!["BOOTING".to_string(), "SELECTING".to_string()],
            ..actual
        };
        assert!(!runner.outcomes_match(&expected, &actual_missing));
    }

    #[test]
    fn mismatch_reason_success() {
        let runner = CanaryRunner::new(
            PathBuf::from("/tmp/.needle"),
            PathBuf::from("/tmp/canary"),
            300,
        );

        let expected = ExpectedOutcome::Success {
            final_status: "done".to_string(),
            labels: vec!["verified".to_string()],
        };

        let actual = ActualOutcome {
            exit_code: Some(1),
            final_status: "open".to_string(),
            labels: vec![],
            state_transitions: vec![],
        };

        let reason = runner.mismatch_reason(&expected, &actual);
        assert!(reason.contains("status mismatch"));
        assert!(reason.contains("missing label"));
        assert!(reason.contains("expected exit 0"));
    }
}
