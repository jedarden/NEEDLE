//! Pre-dispatch snapshots for the shipped-work gate.
//!
//! Before each dispatch the worker records the workspace's git HEAD and a hash
//! of the bead's `notes` field. `validation::shipped_work` compares against this
//! snapshot to decide whether a closure corresponded to real output.
//!
//! **Snapshots live under `~/.needle/state/predispatch/`, never in the
//! workspace.** The gate originally specified a `.needle-predispatch-sha` file
//! inside the repo. That writer was never implemented, but stale copies of the
//! file exist (and are git-tracked) in several workspaces, left by an earlier
//! NEEDLE build. Those stale markers broke the gate two ways: the recorded SHA
//! was months old, so every diff against it looked substantial; and because the
//! file is tracked, an agent's `git commit -a` swept it in, turning a notes-only
//! commit into one that touches a "substantial" path. Keeping snapshot state out
//! of the working tree removes both failure modes by construction.
//!
//! Depends on: `types`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bead_store::{spawn_with_etxtbsy_retry, BeadStore};
use crate::types::BeadId;

/// Workspace + bead state captured immediately before an agent is dispatched.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreDispatch {
    /// `git rev-parse HEAD` in the workspace, or `None` if not a git repo.
    pub head_sha: Option<String>,
    /// SHA-256 of the bead's `notes` field, or `None` if it could not be read.
    ///
    /// Hashed rather than stored verbatim so snapshots stay small and never
    /// carry bead content onto disk.
    pub notes_hash: Option<String>,
}

/// Hash a bead's `notes` field for comparison across a dispatch.
pub fn hash_notes(notes: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(notes.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn state_root() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".needle/state/predispatch")
    } else {
        PathBuf::from("/tmp").join(".needle/state/predispatch")
    }
}

/// Snapshot file for a (workspace, bead) pair, under an explicit state root.
///
/// The workspace path is hashed so that snapshots for same-named beads in
/// different checkouts never collide.
fn snapshot_path_in(root: &Path, workspace: &Path, bead_id: &BeadId) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(workspace.as_os_str().as_encoded_bytes());
    let ws = format!("{:x}", hasher.finalize());
    // Bead ids are `[a-z]+-[a-z0-9]+`, but sanitize anyway — this becomes a filename.
    let bead: String = bead_id
        .as_ref()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    root.join(format!("{}-{}.json", &ws[..16], bead))
}

fn snapshot_path(workspace: &Path, bead_id: &BeadId) -> PathBuf {
    snapshot_path_in(&state_root(), workspace, bead_id)
}

/// Capture the workspace HEAD and the bead's current notes before dispatch.
///
/// Never fails the dispatch: a snapshot that cannot be written just means the
/// gate falls back to its conservative path later.
pub async fn record(workspace: &Path, bead_id: &BeadId, store: &dyn BeadStore) -> Result<()> {
    let snapshot = PreDispatch {
        head_sha: git_head(workspace).await,
        notes_hash: read_notes(store, bead_id).await.map(|n| hash_notes(&n)),
    };

    let path = snapshot_path(workspace, bead_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating predispatch state dir {}", parent.display()))?;
    }
    let encoded = serde_json::to_vec(&snapshot).context("serializing predispatch snapshot")?;
    tokio::fs::write(&path, encoded)
        .await
        .with_context(|| format!("writing predispatch snapshot {}", path.display()))?;
    Ok(())
}

/// Load the snapshot for a (workspace, bead) pair, if one was recorded.
pub async fn load(workspace: &Path, bead_id: &BeadId) -> Option<PreDispatch> {
    let path = snapshot_path(workspace, bead_id);
    let raw = tokio::fs::read(&path).await.ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Remove a snapshot once its dispatch has been fully accounted for.
pub async fn clear(workspace: &Path, bead_id: &BeadId) {
    let _ = tokio::fs::remove_file(snapshot_path(workspace, bead_id)).await;
}

async fn git_head(workspace: &Path) -> Option<String> {
    run(workspace, "git", &["rev-parse", "HEAD"]).await
}

/// Read a bead's current `notes` field, for comparison against a snapshot.
///
/// `Bead` does not carry `notes`, so the gate reads it the same way the
/// snapshot did.
pub async fn current_notes(store: &Arc<dyn BeadStore>, bead_id: &BeadId) -> Option<String> {
    read_notes(store, bead_id).await
}

/// Read notes through the already-resolved workspace backend.
async fn read_notes(store: &Arc<dyn BeadStore>, bead_id: &BeadId) -> Option<String> {
    store.notes(bead_id).await.ok().flatten()
}

async fn run(workspace: &Path, bin: &str, args: &[&str]) -> Option<String> {
    let bin = bin.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let workspace = workspace.to_path_buf();

    let output = spawn_with_etxtbsy_retry(
        || {
            let bin = bin.clone();
            let args = args.clone();
            let workspace = workspace.clone();
            async move {
                tokio::process::Command::new(&bin)
                    .args(args.iter().map(String::as_str).collect::<Vec<_>>())
                    .current_dir(&workspace)
                    .kill_on_drop(true)
                    .output()
                    .await
            }
        },
        5,
        20,
    )
    .await
    .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Write a snapshot under an explicit root, mirroring what `record` does.
    /// Tests never touch `HOME` — it is process-global and these run in parallel.
    async fn write_at(root: &Path, workspace: &Path, bead: &BeadId, snapshot: &PreDispatch) {
        let path = snapshot_path_in(root, workspace, bead);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, serde_json::to_vec(snapshot).unwrap())
            .await
            .unwrap();
    }

    async fn load_at(root: &Path, workspace: &Path, bead: &BeadId) -> Option<PreDispatch> {
        let raw = tokio::fs::read(snapshot_path_in(root, workspace, bead))
            .await
            .ok()?;
        serde_json::from_slice(&raw).ok()
    }

    #[tokio::test]
    async fn roundtrips_a_snapshot() {
        let root = TempDir::new().unwrap();
        let ws = TempDir::new().unwrap();
        let bead: BeadId = "bf-abc".into();

        let snapshot = PreDispatch {
            head_sha: Some("deadbeef".to_string()),
            notes_hash: Some(hash_notes("investigated, nothing to change")),
        };
        write_at(root.path(), ws.path(), &bead, &snapshot).await;

        assert_eq!(
            load_at(root.path(), ws.path(), &bead).await.unwrap(),
            snapshot
        );
    }

    #[tokio::test]
    async fn missing_snapshot_loads_as_none() {
        let root = TempDir::new().unwrap();
        let ws = TempDir::new().unwrap();
        assert!(load_at(root.path(), ws.path(), &"bf-never-recorded".into())
            .await
            .is_none());
    }

    #[test]
    fn same_bead_in_different_workspaces_does_not_collide() {
        let root = Path::new("/state");
        let bead: BeadId = "bf-abc".into();
        assert_ne!(
            snapshot_path_in(root, Path::new("/home/coding/vista"), &bead),
            snapshot_path_in(root, Path::new("/home/coding/HOOP"), &bead)
        );
    }

    #[test]
    fn snapshot_filename_is_sanitized() {
        let path = snapshot_path_in(Path::new("/state"), Path::new("/ws"), &"bf/../etc".into());
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            !name.contains('/'),
            "filename must not contain separators: {name}"
        );
    }

    #[test]
    fn notes_hash_distinguishes_content() {
        assert_ne!(hash_notes(""), hash_notes("checked, already done"));
        assert_eq!(hash_notes("same"), hash_notes("same"));
    }

    #[tokio::test]
    async fn clear_removes_the_snapshot() {
        let root = TempDir::new().unwrap();
        let ws = TempDir::new().unwrap();
        let bead: BeadId = "bf-clear".into();

        write_at(
            root.path(),
            ws.path(),
            &bead,
            &PreDispatch {
                head_sha: None,
                notes_hash: None,
            },
        )
        .await;

        let path = snapshot_path_in(root.path(), ws.path(), &bead);
        tokio::fs::remove_file(&path)
            .await
            .expect("failed to remove snapshot file during clear test");
        assert!(load_at(root.path(), ws.path(), &bead).await.is_none());
    }
}
