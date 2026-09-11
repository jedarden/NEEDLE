//! Gate health tracking for workspace degradation.
//!
//! When a gate command fails to execute (ENOENT, EACCES, timeout, etc.),
//! NEEDLE tracks consecutive errors per workspace. After 3 consecutive
//! execution errors, the workspace is marked as "gate-degraded" and:
//!
//! - Pluck and Explore strands skip the workspace for ordinary dispatch
//! - A single "Gate broken" bead is created with fingerprint deduplication
//! - The workspace remains claimable (fixing a gate is verified by running it)
//!
//! On the next successful gate run in that workspace:
//! - The state file is cleared
//! - workspace.gate_restored telemetry is emitted
//! - The "Gate broken" bead is closed with a reason
//!
//! A second path into the same degradation exists for failures the gate
//! *did* produce (N-T22): when a single verification-failure fingerprint —
//! gate name plus normalized output — dominates a workspace's recent
//! failures across several distinct beads, no bead is at fault and the same
//! degradation follows. Both paths share this state file, so `is_degraded`
//! covers both and every consumer of it (Pluck, Explore, `needle status`)
//! skips a fingerprint-degraded workspace without knowing which path
//! degraded it.

use crate::verification_fingerprint::{DetectorConfig, FingerprintTracker, VerificationFailure};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Get the home directory, falling back to the process temp dir if HOME is not set.
fn home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
    } else {
        std::env::temp_dir()
    }
}

/// Consecutive gate errors threshold for degradation.
const DEGRADATION_THRESHOLD: u32 = 3;

/// Gate health state for a single workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateHealthState {
    /// Workspace path (canonicalized).
    pub workspace: PathBuf,
    /// Consecutive gate execution errors.
    pub consecutive_errors: u32,
    /// Last error timestamp (ISO 8601).
    pub last_error_at: String,
    /// Last gate command that failed.
    pub last_command: String,
    /// Last error reason.
    pub last_reason: String,
    /// Whether workspace is degraded (errors >= threshold).
    #[serde(default)]
    pub degraded: bool,
    /// Verification-failure window behind the fingerprint detector (N-T22),
    /// oldest first. Persisted so the window survives a worker restart and
    /// every worker's detector sees the same failures.
    #[serde(default)]
    pub fingerprint_window: Vec<VerificationFailure>,
    /// The verification fingerprint that degraded the workspace, when the
    /// fingerprint path (rather than consecutive errors) did it.
    #[serde(default)]
    pub degraded_fingerprint: Option<String>,
    /// The gate whose failures produced [`GateHealthState::degraded_fingerprint`].
    #[serde(default)]
    pub degraded_gate: Option<String>,
    /// Normalized summary of the failure behind the tripped fingerprint —
    /// the human-readable half of the "Gate broken" bead title.
    #[serde(default)]
    pub degraded_summary: Option<String>,
}

impl GateHealthState {
    /// Create a new gate health state record.
    fn new(workspace: PathBuf, command: String, reason: String) -> Self {
        Self {
            workspace,
            consecutive_errors: 1,
            last_error_at: chrono::Utc::now().to_rfc3339(),
            last_command: command,
            last_reason: reason,
            degraded: false,
            fingerprint_window: Vec::new(),
            degraded_fingerprint: None,
            degraded_gate: None,
            degraded_summary: None,
        }
    }

    /// An empty record for a workspace whose first recorded signal is a
    /// verification failure rather than a gate execution error.
    fn skeleton(workspace: PathBuf) -> Self {
        Self {
            workspace,
            consecutive_errors: 0,
            last_error_at: chrono::Utc::now().to_rfc3339(),
            last_command: String::new(),
            last_reason: String::new(),
            degraded: false,
            fingerprint_window: Vec::new(),
            degraded_fingerprint: None,
            degraded_gate: None,
            degraded_summary: None,
        }
    }

    /// Increment error count and check if degraded.
    fn increment(&mut self, command: String, reason: String) -> bool {
        self.consecutive_errors += 1;
        self.last_error_at = chrono::Utc::now().to_rfc3339();
        self.last_command = command;
        self.last_reason = reason;

        if self.consecutive_errors >= DEGRADATION_THRESHOLD {
            self.degraded = true;
            true
        } else {
            false
        }
    }

    /// Clear errors on successful gate run.
    #[allow(dead_code)]
    fn clear(&mut self) {
        self.consecutive_errors = 0;
        self.degraded = false;
        self.last_command = String::new();
        self.last_reason = String::new();
    }

    /// The fingerprint this workspace is degraded for, if the fingerprint
    /// path degraded it.
    pub fn degraded_fingerprint(&self) -> Option<&str> {
        self.degraded_fingerprint.as_deref()
    }
}

/// What recording one verification failure decided.
///
/// The distinction the outcome handler acts on: a failure carrying the
/// fingerprint the workspace is already degraded for (or that completes the
/// pattern) is infrastructure and must not penalise its bead, while a failure
/// with any other fingerprint is still a bead's own verification result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationRecording {
    /// The workspace is already degraded for this fingerprint — the failure
    /// adds nothing and must not increment the bead's failure count.
    DegradedForThisFingerprint {
        /// The shared fingerprint.
        fingerprint: String,
    },
    /// The workspace is degraded for a different fingerprint (or by
    /// consecutive gate errors). This failure is judged normally.
    DegradedForOther {
        /// The fingerprint of this failure.
        fingerprint: String,
        /// What the workspace is degraded for instead, for the log.
        degraded: String,
    },
    /// Not degraded; the window did not trip.
    Window {
        /// The fingerprint of this failure.
        fingerprint: String,
        /// Failures now in the window.
        failures: usize,
        /// Distinct beads behind those failures.
        distinct_beads: usize,
    },
    /// This failure completed the pattern — the workspace is now degraded.
    Tripped {
        /// The fingerprint that dominates the window.
        fingerprint: String,
        /// How many of the window's failures carry it.
        failures: usize,
        /// Distinct beads behind those failures.
        distinct_beads: usize,
        /// Normalized failure text, for the "Gate broken" bead.
        summary: String,
    },
}

impl VerificationRecording {
    /// Whether this failure must be released without touching the bead's
    /// failure count.
    pub fn is_infra(&self) -> bool {
        matches!(
            self,
            VerificationRecording::DegradedForThisFingerprint { .. }
                | VerificationRecording::Tripped { .. }
        )
    }

    /// The fingerprint of the failure just recorded.
    pub fn fingerprint(&self) -> &str {
        match self {
            VerificationRecording::DegradedForThisFingerprint { fingerprint }
            | VerificationRecording::DegradedForOther { fingerprint, .. }
            | VerificationRecording::Window { fingerprint, .. }
            | VerificationRecording::Tripped { fingerprint, .. } => fingerprint,
        }
    }
}

/// Record a verification failure against a workspace's fingerprint window
/// and decide what it means.
///
/// `output` is the failing gate's output; it is fingerprinted here so the
/// caller never has to normalize anything itself. The window persists in the
/// workspace's gate-health state file, so the pattern survives a worker
/// restart and is visible to every worker.
pub fn record_verification_failure(
    workspace: &Path,
    bead: &str,
    gate: &str,
    output: &str,
    config: &DetectorConfig,
) -> Result<VerificationRecording> {
    let failure = VerificationFailure::new(chrono::Utc::now(), bead, gate, output);
    let summary = crate::verification_fingerprint::normalize_output(output);

    let mut state = load_state(workspace)?.unwrap_or_else(|| {
        // First recorded signal for this workspace is a verification
        // failure, not a gate execution error.
        GateHealthState::skeleton(workspace.to_path_buf())
    });

    let degraded_for = state.degraded_fingerprint().map(str::to_string);
    let mut tracker = FingerprintTracker::new(state.fingerprint_window.clone(), *config);
    let outcome = tracker.record(failure);
    state.fingerprint_window = tracker.window();

    let recording = match degraded_for {
        Some(degraded) if degraded == outcome.fingerprint => {
            VerificationRecording::DegradedForThisFingerprint {
                fingerprint: outcome.fingerprint,
            }
        }
        Some(degraded) => VerificationRecording::DegradedForOther {
            fingerprint: outcome.fingerprint,
            degraded,
        },
        None => match outcome.decision {
            crate::verification_fingerprint::Decision::Tripped {
                fingerprint,
                failures,
                distinct_beads,
            } => {
                state.degraded = true;
                state.degraded_fingerprint = Some(fingerprint.clone());
                state.degraded_gate = Some(gate.to_string());
                state.degraded_summary = Some(summary.clone());
                VerificationRecording::Tripped {
                    fingerprint,
                    failures,
                    distinct_beads,
                    summary,
                }
            }
            crate::verification_fingerprint::Decision::Window {
                failures,
                distinct_beads,
            } => VerificationRecording::Window {
                fingerprint: outcome.fingerprint,
                failures,
                distinct_beads,
            },
        },
    };

    save_state(&state)?;
    Ok(recording)
}

/// Generate a stable workspace ID from its path.
///
/// The ID is the first 12 hex characters of the SHA-256 hash of the
/// canonical workspace path. This provides collision resistance while
/// keeping filenames short.
pub fn workspace_id(workspace: &Path) -> Result<String> {
    let canonical = fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let path_str = canonical
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("workspace path is not valid UTF-8"))?;

    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let hash = hasher.finalize();

    // Take first 12 hex characters (not padding)
    Ok(format!("{:x}", hash)[..12].to_string())
}

/// Get the gate health state file path for a workspace.
pub fn state_file_path(workspace: &Path) -> Result<PathBuf> {
    let mut base = home_dir();

    base.push(".needle");
    base.push("state");
    base.push("gate-health");

    // Create directory if it doesn't exist
    fs::create_dir_all(&base).context("failed to create gate health state directory")?;

    let id = workspace_id(workspace)?;
    base.push(format!("{}.json", id));

    Ok(base)
}

/// Load gate health state for a workspace.
///
/// Returns None if no state file exists (no errors yet).
pub fn load_state(workspace: &Path) -> Result<Option<GateHealthState>> {
    let path = state_file_path(workspace)?;

    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path).context("failed to read gate health state file")?;

    let state: GateHealthState =
        serde_json::from_str(&content).context("failed to parse gate health state")?;

    Ok(Some(state))
}

/// Save gate health state for a workspace.
pub fn save_state(state: &GateHealthState) -> Result<()> {
    let path = state_file_path(&state.workspace)?;

    let content =
        serde_json::to_string_pretty(state).context("failed to serialize gate health state")?;

    fs::write(&path, content).context("failed to write gate health state file")?;

    Ok(())
}

/// Record a gate execution error for a workspace.
///
/// Returns (previous_state, now_degraded).
pub fn record_error(
    workspace: &Path,
    command: String,
    reason: String,
) -> Result<(Option<GateHealthState>, bool)> {
    let mut state = load_state(workspace)?;

    let now_degraded = if let Some(ref mut s) = state {
        s.increment(command, reason)
    } else {
        let new_state = GateHealthState::new(workspace.to_path_buf(), command, reason);
        let degraded = false;
        save_state(&new_state)?;
        state = Some(new_state);
        degraded
    };

    if let Some(ref s) = state {
        save_state(s)?;
    }

    Ok((state, now_degraded))
}

/// Check if a workspace is currently degraded.
pub fn is_degraded(workspace: &Path) -> Result<bool> {
    match load_state(workspace)? {
        Some(state) => Ok(state.degraded),
        None => Ok(false),
    }
}

/// Clear gate health state for a workspace (restoration).
///
/// Returns the previous state if it existed.
pub fn clear_state(workspace: &Path) -> Result<Option<GateHealthState>> {
    let path = state_file_path(workspace)?;

    if !path.exists() {
        return Ok(None);
    }

    let previous = load_state(workspace)?;

    // Remove the state file
    fs::remove_file(&path).context("failed to remove gate health state file")?;

    Ok(previous)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_workspace_id_stable() {
        let path1 = PathBuf::from("/home/user/test");
        let path2 = PathBuf::from("/home/user/test");
        let path3 = PathBuf::from("/home/user/other");

        let id1 = workspace_id(&path1).unwrap();
        let id2 = workspace_id(&path2).unwrap();
        let id3 = workspace_id(&path3).unwrap();

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
        assert_eq!(id1.len(), 12);
    }

    /// Isolate `$HOME` for tests that touch the on-disk gate-health state.
    ///
    /// `state_path()` resolves under `$HOME/.needle`, so a test that does not
    /// pin HOME reads and writes the real fleet's state — and races every
    /// other test that swaps HOME (observed: test_is_degraded and
    /// test_state_clear failing only under parallel execution).
    fn isolated_home() -> (crate::util::test_env::EnvGuard, TempDir) {
        let env_guard = crate::util::test_env::isolate_env();
        let home = TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());
        (env_guard, home)
    }

    #[test]
    fn test_state_increment_no_degradation() {
        let (_env_guard, _home) = isolated_home();
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        let (state, degraded) = record_error(
            workspace,
            "test-command".to_string(),
            "test-reason".to_string(),
        )
        .unwrap();

        assert!(state.is_some());
        assert!(!degraded);
        assert_eq!(state.unwrap().consecutive_errors, 1);
    }

    #[test]
    fn test_state_clear() {
        let (_env_guard, _home) = isolated_home();
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // Record some errors
        for _ in 0..2 {
            record_error(
                workspace,
                "test-command".to_string(),
                "test-reason".to_string(),
            )
            .unwrap();
        }

        assert!(load_state(workspace).unwrap().is_some());

        // Clear state
        let previous = clear_state(workspace).unwrap();
        assert!(previous.is_some());

        // State is gone
        assert!(load_state(workspace).unwrap().is_none());
    }

    #[test]
    fn test_is_degraded() {
        let (_env_guard, _home) = isolated_home();
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // No errors yet
        assert!(!is_degraded(workspace).unwrap());

        // Record errors until degraded
        for i in 0..DEGRADATION_THRESHOLD {
            let (_, degraded) =
                record_error(workspace, format!("command-{}", i), format!("reason-{}", i)).unwrap();

            if i < DEGRADATION_THRESHOLD - 1 {
                assert!(!degraded);
                assert!(!is_degraded(workspace).unwrap());
            } else {
                assert!(degraded);
                assert!(is_degraded(workspace).unwrap());
            }
        }
    }

    /// The 2026-09-01 incident failure, verbatim.
    const INCIDENT: &str = "command 'scripts/definition-of-done.sh --fast' failed: \
fatal: not a git repository (or any of the parent directories): .git";

    fn record_verification(workspace: &Path, bead: &str) -> VerificationRecording {
        record_verification_failure(
            workspace,
            bead,
            "gate_1",
            INCIDENT,
            &DetectorConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn five_shared_failures_across_beads_trip_degradation() {
        let (_env_guard, _home) = isolated_home();
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // The same broken gate on three distinct beads: the first four
        // failures only fill the window.
        for bead in ["bead-a", "bead-a", "bead-b", "bead-b", "bead-c"] {
            let recording = record_verification(workspace, bead);
            if bead == "bead-c" {
                assert!(
                    recording.is_infra(),
                    "the fifth failure trips: {recording:?}"
                );
                assert!(matches!(
                    recording,
                    VerificationRecording::Tripped {
                        distinct_beads: 3,
                        failures: 5,
                        ..
                    }
                ));
            } else {
                assert!(!recording.is_infra());
            }
        }

        let state = load_state(workspace).unwrap().expect("state persisted");
        assert!(
            is_degraded(workspace).unwrap(),
            "degradation is fleet-visible"
        );
        assert_eq!(
            state.degraded_fingerprint(),
            Some(crate::verification_fingerprint::fingerprint("gate_1", INCIDENT).as_str()),
            "the persisted fingerprint is the detector's, so the alert bead and the window agree"
        );
        assert_eq!(state.degraded_gate.as_deref(), Some("gate_1"));
        assert!(
            state
                .degraded_summary
                .as_deref()
                .unwrap_or_default()
                .contains("not a git repository"),
            "the summary names the failure"
        );
    }

    #[test]
    fn degraded_workspace_stops_penalising_that_fingerprint() {
        let (_env_guard, _home) = isolated_home();
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        for bead in ["bead-a", "bead-b", "bead-c", "bead-d", "bead-e"] {
            record_verification(workspace, bead);
        }
        assert!(is_degraded(workspace).unwrap());

        // Further failures with the same fingerprint are infrastructure.
        let recording = record_verification(workspace, "bead-f");
        assert_eq!(
            recording,
            VerificationRecording::DegradedForThisFingerprint {
                fingerprint: recording.fingerprint().to_string()
            }
        );
        assert!(recording.is_infra());
    }

    #[test]
    fn a_different_failure_is_still_judged_normally_while_degraded() {
        let (_env_guard, _home) = isolated_home();
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        for bead in ["bead-a", "bead-b", "bead-c", "bead-d", "bead-e"] {
            record_verification(workspace, bead);
        }

        let recording = record_verification_failure(
            workspace,
            "bead-f",
            "shipped_work",
            "no substantial pushed commit and no bead note recorded for this dispatch",
            &DetectorConfig::default(),
        )
        .unwrap();

        match &recording {
            VerificationRecording::DegradedForOther { degraded, .. } => {
                assert!(
                    !degraded.is_empty(),
                    "the log names what the workspace is degraded for"
                );
            }
            other => panic!("expected DegradedForOther, got {other:?}"),
        }
        assert!(!recording.is_infra());
    }

    #[test]
    fn clear_state_also_clears_the_fingerprint_degradation() {
        let (_env_guard, _home) = isolated_home();
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        for bead in ["bead-a", "bead-b", "bead-c", "bead-d", "bead-e"] {
            record_verification(workspace, bead);
        }
        assert!(is_degraded(workspace).unwrap());

        clear_state(workspace).unwrap();
        assert!(!is_degraded(workspace).unwrap());
        // And the window is gone with it — the next degradation needs a
        // fresh pattern, not a resurrected one.
        let recording = record_verification(workspace, "bead-a");
        assert!(!recording.is_infra());
    }

    #[test]
    fn state_files_written_by_older_builds_still_load() {
        let (_env_guard, _home) = isolated_home();
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        // A pre-fingerprint state file: consecutive-error fields only.
        let legacy = format!(
            "{{\"workspace\":\"{}\",\"consecutive_errors\":2,\
              \"last_error_at\":\"2026-09-01T12:00:00Z\",\"last_command\":\"x\",\
              \"last_reason\":\"y\",\"degraded\":false}}",
            workspace.display()
        );
        std::fs::write(state_file_path(workspace).unwrap(), legacy).unwrap();

        let state = load_state(workspace).unwrap().expect("legacy state loads");
        assert!(state.fingerprint_window.is_empty());
        assert!(state.degraded_fingerprint.is_none());
    }
}
