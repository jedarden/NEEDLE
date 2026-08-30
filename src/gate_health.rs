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

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Get the home directory, falling back to /tmp if HOME is not set.
fn home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
    } else {
        PathBuf::from("/tmp")
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
    fn isolated_home() -> (
        std::sync::MutexGuard<'static, ()>,
        crate::util::test_env::EnvGuard,
        TempDir,
    ) {
        let (lock, env_guard) = crate::util::test_env::isolate_env();
        let home = TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());
        (lock, env_guard, home)
    }

    #[test]
    fn test_state_increment_no_degradation() {
        let (_lock, _env_guard, _home) = isolated_home();
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
        let (_lock, _env_guard, _home) = isolated_home();
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
        let (_lock, _env_guard, _home) = isolated_home();
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
}
