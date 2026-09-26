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
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path, PathBuf};

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

/// Outcome of inspecting persisted gate-health records for vanished workspaces.
///
/// Records that cannot be proved safe to remove are reported in `retained`
/// and left untouched. `needle doctor --repair` is therefore scoped to files
/// whose serialized workspace is missing and whose filename still matches the
/// workspace identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateHealthPruneReport {
    /// JSON state records inspected.
    pub scanned: usize,
    /// Records whose workspace did not exist at inspection time.
    pub missing: usize,
    /// Missing-workspace records removed when repair was requested.
    pub removed: usize,
    /// Valid records retained because their workspace still exists.
    pub existing: usize,
    /// Records retained because they could not be parsed, identified, or
    /// safely removed.
    pub retained: Vec<String>,
    /// Missing workspace paths, bounded by the caller when displayed.
    pub missing_workspaces: Vec<PathBuf>,
}

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
    ensure_recordable_workspace(workspace)?;

    let failure = VerificationFailure::new(chrono::Utc::now(), bead, gate, output);
    let summary = crate::verification_fingerprint::normalize_output(output);

    let mut state = load_state(workspace)?.unwrap_or_else(|| {
        // First recorded signal for this workspace is a verification
        // failure, not a gate execution error.
        GateHealthState::skeleton(canonical_workspace(workspace))
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
    let canonical = canonical_workspace(workspace);
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
    let id = workspace_id(workspace)?;
    Ok(crate::state_dir::gate_health_dir().join(format!("{}.json", id)))
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
    ensure_recordable_workspace(&state.workspace)?;

    let path = state_file_path(&state.workspace)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("gate health state path has no parent"))?;
    fs::create_dir_all(parent).context("failed to create gate health state directory")?;

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
    ensure_recordable_workspace(workspace)?;

    let mut state = load_state(workspace)?;

    let now_degraded = if let Some(ref mut s) = state {
        s.increment(command, reason)
    } else {
        let new_state = GateHealthState::new(canonical_workspace(workspace), command, reason);
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

/// Inspect gate-health state and optionally remove records for workspaces that
/// no longer exist.
///
/// A candidate is deleted only when its JSON parses, its filename still
/// matches the serialized workspace identity, its workspace is missing both
/// during the scan and immediately before deletion, and the file contents did
/// not change between those checks. Existing and untrusted records are always
/// retained, preserving real degradation data while workers are active.
pub fn prune_missing_workspace_records(
    needle_home: &Path,
    repair: bool,
) -> Result<GateHealthPruneReport> {
    let gate_health_dir = needle_home.join("state").join("gate-health");
    if !gate_health_dir.exists() {
        return Ok(GateHealthPruneReport::default());
    }

    struct Candidate {
        path: PathBuf,
        contents: Vec<u8>,
        workspace: PathBuf,
    }

    let mut report = GateHealthPruneReport::default();
    let mut candidates = Vec::new();
    let entries = fs::read_dir(&gate_health_dir)
        .with_context(|| format!("failed to read {}", gate_health_dir.display()))?;

    for entry in entries {
        let entry = entry.context("failed to read gate-health directory entry")?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        report.scanned += 1;

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                report.retained.push(format!(
                    "{}: cannot inspect file type: {error}",
                    path.display()
                ));
                continue;
            }
        };
        if !file_type.is_file() {
            report
                .retained
                .push(format!("{}: not a regular file", path.display()));
            continue;
        }

        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) => {
                report
                    .retained
                    .push(format!("{}: cannot read state: {error}", path.display()));
                continue;
            }
        };
        let state: GateHealthState = match serde_json::from_slice(&contents) {
            Ok(state) => state,
            Err(error) => {
                report
                    .retained
                    .push(format!("{}: cannot parse state: {error}", path.display()));
                continue;
            }
        };

        let expected_name = match workspace_id(&state.workspace) {
            Ok(id) => format!("{id}.json"),
            Err(error) => {
                report.retained.push(format!(
                    "{}: cannot identify workspace: {error}",
                    path.display()
                ));
                continue;
            }
        };
        if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
            report.retained.push(format!(
                "{}: filename does not match serialized workspace {}",
                path.display(),
                state.workspace.display()
            ));
            continue;
        }

        match state.workspace.try_exists() {
            Ok(true) => report.existing += 1,
            Ok(false) => {
                report.missing += 1;
                report.missing_workspaces.push(state.workspace.clone());
                candidates.push(Candidate {
                    path,
                    contents,
                    workspace: state.workspace,
                });
            }
            Err(error) => report.retained.push(format!(
                "{}: cannot inspect workspace {}: {error}",
                path.display(),
                state.workspace.display()
            )),
        }
    }

    if !repair {
        return Ok(report);
    }

    for candidate in candidates {
        let current = match fs::read(&candidate.path) {
            Ok(current) => current,
            Err(error) => {
                report.retained.push(format!(
                    "{}: state changed before repair: {error}",
                    candidate.path.display()
                ));
                continue;
            }
        };
        if current != candidate.contents {
            report.retained.push(format!(
                "{}: state changed during inspection",
                candidate.path.display()
            ));
            continue;
        }
        match candidate.workspace.try_exists() {
            Ok(false) => {}
            Ok(true) => {
                report.retained.push(format!(
                    "{}: workspace reappeared during inspection",
                    candidate.path.display()
                ));
                continue;
            }
            Err(error) => {
                report.retained.push(format!(
                    "{}: cannot recheck workspace: {error}",
                    candidate.path.display()
                ));
                continue;
            }
        }
        match fs::remove_file(&candidate.path) {
            Ok(()) => report.removed += 1,
            Err(error) => report.retained.push(format!(
                "{}: cannot remove stale state: {error}",
                candidate.path.display()
            )),
        }
    }

    Ok(report)
}

/// Refuse to persist an ephemeral workspace into a durable operator state
/// root. Tests that deliberately use temporary workspaces remain valid only
/// when HOME itself is isolated beneath a recognized temporary root.
fn ensure_recordable_workspace(workspace: &Path) -> Result<()> {
    let home = home_dir();
    if is_recognized_temporary_path(workspace)? && !is_recognized_temporary_path(&home)? {
        bail!(
            "refusing to record gate health for temporary workspace {} in durable HOME {}",
            workspace.display(),
            home.display()
        );
    }
    Ok(())
}

fn canonical_workspace(workspace: &Path) -> PathBuf {
    fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf())
}

fn is_recognized_temporary_path(path: &Path) -> Result<bool> {
    let lexical = lexical_absolute(path)?;
    let canonical = fs::canonicalize(path).ok();
    let filesystem_root = PathBuf::from(std::path::MAIN_SEPARATOR_STR);

    for root in [
        std::env::temp_dir(),
        filesystem_root.join("tmp"),
        filesystem_root.join("var").join("tmp"),
    ] {
        let lexical_root = lexical_absolute(&root)?;
        if lexical.starts_with(&lexical_root)
            || canonical
                .as_ref()
                .is_some_and(|resolved| resolved.starts_with(&lexical_root))
        {
            return Ok(true);
        }
        if let Ok(canonical_root) = fs::canonicalize(&root) {
            if lexical.starts_with(&canonical_root)
                || canonical
                    .as_ref()
                    .is_some_and(|resolved| resolved.starts_with(&canonical_root))
            {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn lexical_absolute(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir | Component::Normal(_) => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
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
        isolated_home_in(&std::env::temp_dir())
    }

    fn isolated_home_in(parent: &Path) -> (crate::util::test_env::EnvGuard, TempDir) {
        let parent = parent.to_path_buf();
        isolated_home_from(|| parent)
    }

    fn isolated_home_from(
        parent: impl FnOnce() -> PathBuf,
    ) -> (crate::util::test_env::EnvGuard, TempDir) {
        let env_guard = crate::util::test_env::isolate_env();
        let home = TempDir::new_in(parent()).unwrap();
        std::env::set_var("HOME", home.path());
        (env_guard, home)
    }

    /// Create a test HOME below the operator's durable NEEDLE state root.
    ///
    /// This test must distinguish a durable HOME from the temporary workspace
    /// it rejects. The repository's clean-archive verification runs from a
    /// temporary extraction, so using the current directory as the parent
    /// would accidentally make both paths temporary and disable the guard.
    fn isolated_durable_home() -> (crate::util::test_env::EnvGuard, TempDir) {
        isolated_home_from(|| {
            let parent = home_dir().join(".needle");
            fs::create_dir_all(&parent).unwrap();
            parent
        })
    }

    #[test]
    fn durable_home_refuses_all_temporary_workspace_recording() {
        let (_env_guard, home) = isolated_durable_home();
        let workspace = TempDir::new().unwrap();

        let gate_error = record_error(
            workspace.path(),
            "missing-gate".to_string(),
            "ENOENT".to_string(),
        )
        .unwrap_err();
        assert!(gate_error.to_string().contains("temporary workspace"));

        let verification_error = record_verification_failure(
            workspace.path(),
            "bead-test",
            "definition-of-done",
            "failed",
            &DetectorConfig::default(),
        )
        .unwrap_err();
        assert!(verification_error
            .to_string()
            .contains("temporary workspace"));

        let direct_save = save_state(&GateHealthState::new(
            workspace.path().to_path_buf(),
            "missing-gate".to_string(),
            "ENOENT".to_string(),
        ))
        .unwrap_err();
        assert!(direct_save.to_string().contains("temporary workspace"));
        assert!(
            !home.path().join(".needle/state/gate-health").exists(),
            "refused recordings must not create durable state"
        );
    }

    #[test]
    fn prune_removes_only_proven_missing_workspace_records() {
        let (_env_guard, home) = isolated_home();
        let workspaces = TempDir::new().unwrap();
        let existing = workspaces.path().join("existing");
        let missing = workspaces.path().join("missing");
        fs::create_dir_all(&existing).unwrap();
        fs::create_dir_all(&missing).unwrap();

        record_error(
            &existing,
            "real-gate".to_string(),
            "real failure".to_string(),
        )
        .unwrap();
        record_error(
            &missing,
            "test-gate".to_string(),
            "fixture failure".to_string(),
        )
        .unwrap();
        let existing_path = state_file_path(&existing).unwrap();
        let missing_path = state_file_path(&missing).unwrap();
        let existing_bytes = fs::read(&existing_path).unwrap();
        fs::remove_dir(&missing).unwrap();
        let gate_health_dir = home.path().join(".needle/state/gate-health");
        let malformed_path = gate_health_dir.join("untrusted.json");
        fs::write(&malformed_path, b"not-json").unwrap();

        let inspected =
            prune_missing_workspace_records(&home.path().join(".needle"), false).unwrap();
        assert_eq!(inspected.scanned, 3);
        assert_eq!(inspected.missing, 1);
        assert_eq!(inspected.removed, 0);
        assert_eq!(inspected.existing, 1);
        assert_eq!(inspected.retained.len(), 1);
        assert!(missing_path.exists(), "inspection is read-only");

        let repaired = prune_missing_workspace_records(&home.path().join(".needle"), true).unwrap();
        assert_eq!(repaired.removed, 1);
        assert!(!missing_path.exists());
        assert_eq!(fs::read(&existing_path).unwrap(), existing_bytes);
        assert!(malformed_path.exists(), "untrusted records fail closed");
    }

    #[test]
    fn test_state_increment_no_degradation() {
        let (_env_guard, home) = isolated_home();
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();
        let state_dir = home.path().join(".needle/state/gate-health");

        assert!(load_state(workspace).unwrap().is_none());
        assert!(
            !state_dir.exists(),
            "a read-only health check must not mutate inherited HOME"
        );

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
        let path = state_file_path(workspace).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, legacy).unwrap();

        let state = load_state(workspace).unwrap().expect("legacy state loads");
        assert!(state.fingerprint_window.is_empty());
        assert!(state.degraded_fingerprint.is_none());
    }
}
