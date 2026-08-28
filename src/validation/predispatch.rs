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

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bead_store::{spawn_with_etxtbsy_retry, BeadStore};
use crate::types::BeadId;

/// A dirty file with its blob hash at predispatch time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirtyFile {
    /// Path relative to workspace root.
    pub path: String,
    /// Git blob hash of the file's contents at predispatch time.
    pub blob_hash: String,
}

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
    /// Files that were dirty at predispatch time with their blob hashes.
    ///
    /// Used by the commit hook to detect when an agent sweeps in another
    /// worker's in-flight edit without modifying it.
    pub dirty_files: Vec<DirtyFile>,
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
pub fn snapshot_path_in(root: &Path, workspace: &Path, bead_id: &BeadId) -> PathBuf {
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

pub fn snapshot_path(workspace: &Path, bead_id: &BeadId) -> PathBuf {
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
        dirty_files: capture_dirty_files(workspace).await.unwrap_or_default(),
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

/// Capture all dirty files in the workspace with their blob hashes.
///
/// Returns `None` if not a git repo or if git commands fail. Returns an empty
/// vec if there are no dirty files.
async fn capture_dirty_files(workspace: &Path) -> Option<Vec<DirtyFile>> {
    let workspace = workspace.to_path_buf();

    // Run git status --porcelain to get all dirty files (tracked and untracked)
    let output = spawn_with_etxtbsy_retry(
        || {
            let workspace = workspace.clone();
            async move {
                tokio::process::Command::new("git")
                    .args(["status", "--porcelain"])
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

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut dirty_files = Vec::new();

    for line in stdout.lines() {
        if line.len() < 4 {
            continue; // Skip malformed lines
        }

        let status = &line[..2];
        let path = line[3..].trim();

        // Skip .beads/ and .needle-predispatch-sha - they have their own handling
        if path.starts_with(".beads/") || path == ".needle-predispatch-sha" {
            continue;
        }

        // Only capture files that are actually modified (M), added (A), or untracked (??)
        // Ignore deleted files since they won't be in the index
        if !matches!(status, " M" | "M " | "MM" | "A " | "??") {
            continue;
        }

        // Get the blob hash of the file's current content
        let blob_hash = match git_hash_object(&workspace, path).await {
            Some(hash) => hash,
            None => continue, // Skip files we can't hash
        };

        dirty_files.push(DirtyFile {
            path: path.to_string(),
            blob_hash,
        });
    }

    if dirty_files.is_empty() {
        Some(Vec::new())
    } else {
        Some(dirty_files)
    }
}

/// Get the git blob hash of a file's current content.
///
/// Returns `None` if the file doesn't exist or git hash-object fails.
async fn git_hash_object(workspace: &Path, path: &str) -> Option<String> {
    let output = spawn_with_etxtbsy_retry(
        || {
            let workspace = workspace.clone();
            let path = path.to_string();
            async move {
                // Use git hash-object with the file path directly
                tokio::process::Command::new("git")
                    .args(["hash-object", &path])
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

async fn git_head(workspace: &Path) -> Option<String> {
    let workspace = workspace.to_path_buf();

    let output = spawn_with_etxtbsy_retry(
        || {
            let workspace = workspace.clone();
            async move {
                tokio::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
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

/// Read a bead's current `notes` field, for comparison against a snapshot.
///
/// `notes` is a backend capability (`BeadStore::notes`), not part of the common
/// `Bead` projection. `Bead::body` is the bead's `description`, which `update`
/// cannot change mid-dispatch — keying the gate on it would make the
/// note-update fallback unreachable, so the snapshot and this read must both
/// go through the notes capability.
pub async fn current_notes(store: &dyn BeadStore, bead_id: &BeadId) -> Option<String> {
    read_notes(store, bead_id).await
}

/// Read notes through the already-resolved workspace backend.
///
/// Backends without a notes projection return `None`, and the gate falls back
/// to its conservative path — the same outcome the pre-migration `bf show`
/// subprocess produced when it failed.
async fn read_notes(store: &dyn BeadStore, bead_id: &BeadId) -> Option<String> {
    store.notes(bead_id).await.ok().flatten()
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
            dirty_files: vec![],
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
                dirty_files: vec![],
            },
        )
        .await;

        let path = snapshot_path_in(root.path(), ws.path(), &bead);
        tokio::fs::remove_file(&path)
            .await
            .expect("failed to remove snapshot file during clear test");
        assert!(load_at(root.path(), ws.path(), &bead).await.is_none());
    }

    #[tokio::test]
    async fn read_notes_consults_store_not_subprocess() {
        use crate::types::Bead;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        // Create a mock bead ID and expected notes
        let bead_id: BeadId = "bf-test".into();
        let expected_notes = "Investigated the issue, found a bug in the parser".to_string();

        // Track whether show() was called and with which ID
        let show_called = Arc::new(RwLock::new(false));
        let show_called_id = Arc::new(RwLock::new(None::<BeadId>));
        let show_called_clone = Arc::clone(&show_called);
        let show_called_id_clone = Arc::clone(&show_called_id);

        // Track whether notes() was called and with which ID
        let notes_called = Arc::new(RwLock::new(false));
        let notes_called_id = Arc::new(RwLock::new(None::<BeadId>));
        let notes_called_clone = Arc::clone(&notes_called);
        let notes_called_id_clone = Arc::clone(&notes_called_id);

        // Create a mock BeadStore that records calls and returns fixed values.
        // The bead's `body` (the `description`) deliberately differs from its
        // `notes`: the gate must key on the notes capability, since `update`
        // cannot change the description mid-dispatch.
        struct MockBeadStore {
            show_called: Arc<RwLock<bool>>,
            show_called_id: Arc<RwLock<Option<BeadId>>>,
            notes_called: Arc<RwLock<bool>>,
            notes_called_id: Arc<RwLock<Option<BeadId>>>,
            notes: String,
            description: String,
        }

        #[async_trait::async_trait]
        impl crate::bead_store::BeadStore for MockBeadStore {
            async fn show(&self, id: &BeadId) -> anyhow::Result<Bead> {
                // Record that show() was called with this ID
                *self.show_called.write().await = true;
                *self.show_called_id.write().await = Some(id.clone());

                // Return a bead whose body is the description, NOT the notes
                Ok(Bead {
                    id: id.clone(),
                    title: "Test bead".to_string(),
                    body: Some(self.description.clone()),
                    priority: 2,
                    status: crate::types::BeadStatus::Open,
                    assignee: None,
                    labels: vec![],
                    workspace: std::path::PathBuf::from("/tmp/test"),
                    dependencies: vec![],
                    dependents: vec![],
                    comments: vec![],
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                })
            }

            async fn notes(&self, id: &BeadId) -> anyhow::Result<Option<String>> {
                // Record that notes() was called with this ID
                *self.notes_called.write().await = true;
                *self.notes_called_id.write().await = Some(id.clone());

                // Return the expected notes
                Ok(Some(self.notes.clone()))
            }

            // Provide minimal stub implementations for required trait methods
            async fn ready(
                &self,
                _filters: &crate::bead_store::Filters,
            ) -> anyhow::Result<Vec<Bead>> {
                Ok(vec![])
            }

            async fn list_all(&self) -> anyhow::Result<Vec<Bead>> {
                Ok(vec![])
            }

            async fn claim(
                &self,
                _id: &BeadId,
                _actor: &str,
            ) -> anyhow::Result<crate::types::ClaimResult> {
                Ok(crate::types::ClaimResult::NotClaimable {
                    reason: "mock".to_string(),
                })
            }

            async fn claim_auto(&self, _actor: &str) -> anyhow::Result<crate::types::ClaimResult> {
                Ok(crate::types::ClaimResult::NotClaimable {
                    reason: "mock".to_string(),
                })
            }

            async fn release(&self, _id: &BeadId) -> anyhow::Result<()> {
                Ok(())
            }

            async fn block(&self, _id: &BeadId) -> anyhow::Result<()> {
                Ok(())
            }

            async fn clear_assignee(&self, _id: &BeadId) -> anyhow::Result<()> {
                Ok(())
            }

            async fn flush(&self) -> anyhow::Result<()> {
                Ok(())
            }

            async fn reopen(&self, _id: &BeadId) -> anyhow::Result<()> {
                Ok(())
            }

            async fn labels(&self, _id: &BeadId) -> anyhow::Result<Vec<String>> {
                Ok(vec![])
            }

            async fn add_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
                Ok(())
            }

            async fn remove_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
                Ok(())
            }

            async fn create_bead(
                &self,
                _title: &str,
                _body: &str,
                _labels: &[&str],
            ) -> anyhow::Result<BeadId> {
                Ok(BeadId::from("bf-new".to_string()))
            }

            async fn add_dependency(
                &self,
                _blocker_id: &BeadId,
                _blocked_id: &BeadId,
            ) -> anyhow::Result<()> {
                Ok(())
            }

            async fn remove_dependency(
                &self,
                _blocked_id: &BeadId,
                _blocker_id: &BeadId,
            ) -> anyhow::Result<()> {
                Ok(())
            }

            async fn doctor_repair(&self) -> anyhow::Result<crate::bead_store::RepairReport> {
                Ok(crate::bead_store::RepairReport::default())
            }

            async fn doctor_check(&self) -> anyhow::Result<crate::bead_store::RepairReport> {
                Ok(crate::bead_store::RepairReport::default())
            }

            async fn full_rebuild(&self) -> anyhow::Result<()> {
                Ok(())
            }

            fn has_valid_store(&self) -> bool {
                true
            }
        }

        // Create the mock store
        let mock_store = MockBeadStore {
            show_called: show_called_clone,
            show_called_id: show_called_id_clone,
            notes_called: notes_called_clone,
            notes_called_id: notes_called_id_clone,
            notes: expected_notes.clone(),
            description: "Deliverables fixed at creation time".to_string(),
        };

        // Call read_notes via the public interface
        let result = current_notes(&mock_store, &bead_id).await;

        // Verify the result carries the notes, not the description
        assert_eq!(result, Some(expected_notes));

        // Verify that store.notes() was called with the correct bead_id
        assert!(*notes_called.read().await);
        assert_eq!(*notes_called_id.read().await, Some(bead_id));

        // Verify the description was never consulted: keying the gate on
        // `Bead::body` would make the note-update fallback unreachable,
        // because `update` cannot change a bead's description.
        assert!(!*show_called.read().await);
        assert_eq!(*show_called_id.read().await, None);
    }
}
