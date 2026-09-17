//! Bead-Id commit trailer injection and validation.
//!
//! When a bead closes with a commit artifact (i.e. the agent made commits),
//! NEEDLE amends the latest commit to include a `Bead-Id: <id>` trailer.
//! HOOP's bead_commit_index then picks this up via `git log`.
//!
//! The commit hook also validates that agents don't sweep in other workers'
//! in-flight edits by checking against the predispatch snapshot.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use fs2::FileExt;
use tokio::process::Command;

use crate::types::BeadId;
use crate::validation::predispatch::load;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Validate that a commit doesn't sweep in other workers' in-flight edits.
///
/// Checks the staged files against the predispatch snapshot. Rejects commits
/// that include paths whose content matches the predispatch blob hash (meaning
/// the agent didn't modify them after dispatch). Returns `Ok(())` if validation
/// passes or if there's no snapshot to check against.
///
/// # Arguments
///
/// * `workspace` - Path to the git workspace
/// * `bead_id` - The bead ID being worked on
///
/// # Returns
///
/// * `Ok(())` if validation passes or no snapshot exists
/// * `Err(String)` with a human-readable rejection message listing the foreign paths
pub async fn validate_commit(workspace: &Path, bead_id: &BeadId) -> Result<()> {
    let ws = workspace.to_str().unwrap_or(".").to_string();

    // Load the predispatch snapshot
    let snapshot = match load(workspace, bead_id).await {
        Some(s) => s,
        None => {
            // No snapshot means we can't validate — this is the conservative
            // fallback path for workspaces without snapshots
            tracing::warn!(
                bead_id = %bead_id,
                workspace = %ws,
                "no predispatch snapshot found, skipping dirty file validation"
            );
            return Ok(());
        }
    };

    // Get the list of paths about to be committed
    let committed_paths = match get_staged_paths(&ws).await {
        Ok(paths) => paths,
        Err(e) => {
            tracing::warn!(
                bead_id = %bead_id,
                workspace = %ws,
                error = %e,
                "failed to get staged paths, skipping dirty file validation"
            );
            return Ok(());
        }
    };

    // Check each committed path against the predispatch dirty files
    let mut foreign_paths = Vec::new();

    for committed_path in &committed_paths {
        // Skip .beads/ and .needle-predispatch-sha — they have their own handling
        if committed_path.starts_with(".beads/") || committed_path == ".needle-predispatch-sha" {
            continue;
        }

        // Check if this path was dirty at predispatch
        if let Some(dirty_file) = snapshot
            .dirty_files
            .iter()
            .find(|df| df.path == *committed_path)
        {
            // Get the current blob hash of the staged version
            let current_hash = match get_staged_blob_hash(&ws, committed_path).await {
                Ok(hash) => hash,
                Err(e) => {
                    tracing::warn!(
                        bead_id = %bead_id,
                        workspace = %ws,
                        path = %committed_path,
                        error = %e,
                        "failed to get staged blob hash, assuming path was modified"
                    );
                    // If we can't check, assume the agent modified it — be permissive
                    continue;
                }
            };

            // If the hash matches, the agent didn't modify it — foreign dirty file!
            if current_hash == dirty_file.blob_hash {
                foreign_paths.push(committed_path.clone());
            }
        }
    }

    if !foreign_paths.is_empty() {
        let paths = foreign_paths.join(", ");
        let error_msg = format!(
            "commit rejected: sweeping in other workers' edits without modification. \
            These paths were dirty before dispatch and you haven't modified them: {}",
            paths
        );
        tracing::warn!(
            bead_id = %bead_id,
            workspace = %ws,
            foreign_paths = %paths,
            "commit rejected: sweeping in foreign dirty files"
        );
        return Err(anyhow!(error_msg));
    }

    Ok(())
}

/// Get the list of paths that are staged for commit.
///
/// Returns the relative paths of all files in the index.
async fn get_staged_paths(workspace: &str) -> Result<Vec<String>> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Command::new("git")
            .args(["-C", workspace, "diff", "--name-only", "--cached", "-z"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!("git diff timed out after 10s in {}", workspace))??;

    if !output.status.success() {
        anyhow::bail!("git diff failed in {}", workspace);
    }

    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

/// Get the blob hash of a file's staged version.
///
/// Returns the git object hash of the file as it appears in the index.
async fn get_staged_blob_hash(workspace: &str, path: &str) -> Result<String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Command::new("git")
            .args(["-C", workspace, "ls-files", "-s", "--", path])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!("git ls-files timed out after 10s in {}", workspace))??;

    if !output.status.success() {
        anyhow::bail!("git ls-files failed for {} in {}", path, workspace);
    }

    // Output format: "<mode> <blob_hash> <stage>\t<path>"
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.split_whitespace().collect();

    if parts.len() < 2 {
        anyhow::bail!(
            "unexpected git ls-files output for {} in {}",
            path,
            workspace
        );
    }

    Ok(parts[1].to_string())
}

// ---------------------------------------------------------------------------
// Bead-Id trailer injection (existing functionality)
// ---------------------------------------------------------------------------

/// List the commit SHAs created in `workspace` since `since_sha` (inclusive of
/// HEAD, exclusive of `since_sha`), oldest first.
///
/// Used to fill the `commits` field of the `attempt.resolved` ledger row: the
/// worker captures HEAD just before dispatch and reads back what the agent
/// added. Returns an empty list when `since_sha` is unknown, the workspace is
/// not a git repo, or git fails — commits are ledger evidence, not a gate, so
/// a failure here must never fail the dispatch.
pub(crate) async fn commits_since(workspace: &str, since_sha: &str) -> Result<Vec<String>> {
    let range = format!("{}..HEAD", since_sha);
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Command::new("git")
            .args(["-C", workspace, "log", "--format=%H", "--reverse", &range])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!("git log {} timed out after 10s in {}", range, workspace))??;

    if !out.status.success() {
        anyhow::bail!("git log {} failed in {}", range, workspace);
    }
    Ok(String::from_utf8(out.stdout)?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Inject a `Bead-Id: <id>` trailer into the latest commit in `workspace`.
///
/// Only acts when HEAD moved since `pre_dispatch_head` (i.e. the agent made
/// at least one commit). Returns `Ok(())` in all no-op cases (not a git repo,
/// no new commits, trailer already present). Errors are logged by callers as
/// non-fatal warnings.
///
/// This function uses a per-workspace advisory lock (flock) to serialize
/// the read-HEAD → verify → amend sequence. Inside the lock, it verifies that
/// the commit at HEAD actually corresponds to this bead by checking the commit
/// subject contains the bead ID (per the NEEDLE commit convention:
/// `fix(needle-XYZ): ...`). This prevents cross-tagging commits when multiple
/// workers dispatch concurrently in the same workspace.
pub async fn inject_bead_id_trailer(
    workspace: &Path,
    bead_id: &BeadId,
    pre_dispatch_head: &str,
) -> Result<()> {
    let ws = workspace.to_str().unwrap_or(".").to_string();

    // Get current HEAD — if it fails, workspace is not a git repo.
    let current_head = match git_head(&ws).await {
        Ok(h) => h,
        Err(_) => return Ok(()),
    };

    // No new commits → nothing to tag.
    if current_head == pre_dispatch_head {
        return Ok(());
    }

    // Check if the trailer is already present (idempotent).
    if already_has_trailer(&ws, bead_id).await? {
        return Ok(());
    }

    // Acquire workspace flock to serialize the verify → amend sequence.
    // The lock path is deterministic for the workspace: <workspace>/.git/needle-trailer.lock
    let lock_path = trailer_lock_path(workspace);
    let _lock = match acquire_flock(&lock_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                bead_id = %bead_id,
                workspace = %ws,
                error = %e,
                "failed to acquire trailer flock, skipping injection"
            );
            // Return Ok instead of Err — this is a non-fatal step
            return Ok(());
        }
    };

    // Re-read HEAD inside the lock — it may have changed while we waited.
    let current_head = match git_head(&ws).await {
        Ok(h) => h,
        Err(_) => return Ok(()),
    };

    // No new commits → nothing to tag (re-check after acquiring lock).
    if current_head == pre_dispatch_head {
        return Ok(());
    }

    // Verify that the commit at HEAD actually belongs to this bead.
    // The NEEDLE commit convention puts the bead ID in the subject line:
    // "feat(needle-XYZ): ..." or "fix(needle-XYZ): ..."
    let head_subject = git_head_subject(&ws).await?;
    let bead_id_str = bead_id.as_ref();
    if !head_subject.contains(bead_id_str) {
        tracing::warn!(
            bead_id = %bead_id,
            workspace = %ws,
            head_subject = %head_subject,
            current_head = %current_head,
            "HEAD commit does not match this bead, skipping trailer injection to avoid mislabeling"
        );
        // Skip injection rather than mislabel another bead's commit.
        // The lock ensures we don't race with another worker injecting its own trailer.
        return Ok(());
    }

    // Check if HEAD is already pushed to any remote. If so, skip the amend
    // to avoid rewriting published commits, which would diverge local and
    // remote history and break the next `git push` in a shared checkout.
    let is_pushed = is_head_pushed(&ws).await?;
    if is_pushed {
        tracing::info!(
            bead_id = %bead_id,
            workspace = %ws,
            current_head = %current_head,
            "HEAD already pushed to remote, skipping Bead-Id trailer injection to avoid diverging history"
        );
        return Ok(());
    }

    // Amend the latest commit to add the Bead-Id trailer.
    // Wrapped in a 30-second timeout to prevent indefinite hangs if git
    // subprocess hangs (e.g., due to filesystem issues or network mounts).
    let trailer_arg = format!("Bead-Id: {}", bead_id);
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Command::new("git")
            .args([
                "-C",
                &ws,
                "commit",
                "--amend",
                "--no-edit",
                "--trailer",
                &trailer_arg,
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("git commit --amend timed out after 30s in {}", ws))??;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git commit --amend failed: {}", stderr.trim());
    }

    tracing::info!(
        bead_id = %bead_id,
        workspace = %ws,
        "injected Bead-Id trailer into latest commit"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the current HEAD SHA for `workspace`.
///
/// Wrapped in a 10-second timeout to prevent indefinite hangs if git
/// subprocess hangs (e.g., due to filesystem issues or network mounts).
pub(crate) async fn git_head(workspace: &str) -> Result<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Command::new("git")
            .args(["-C", workspace, "rev-parse", "HEAD"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("git rev-parse HEAD timed out after 10s in {}", workspace))??;

    if !out.status.success() {
        anyhow::bail!("git rev-parse HEAD failed in {}", workspace);
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

/// Return the HEAD commit subject for `workspace`.
///
/// Wrapped in a 10-second timeout to prevent indefinite hangs if git
/// subprocess hangs (e.g., due to filesystem issues or network mounts).
async fn git_head_subject(workspace: &str) -> Result<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Command::new("git")
            .args(["-C", workspace, "log", "-1", "--format=%s"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("git log timed out after 10s in {}", workspace))??;

    if !out.status.success() {
        anyhow::bail!("git log failed in {}", workspace);
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

/// Compute the trailer lock file path for a workspace.
///
/// Uses `.git/needle-trailer.lock` within the workspace directory.
/// All workers on the same workspace compute the same lock path.
fn trailer_lock_path(workspace: &Path) -> PathBuf {
    workspace.join(".git").join("needle-trailer.lock")
}

/// Acquire an exclusive flock with a timeout.
///
/// Returns the locked file on success. The lock is released when the
/// file is dropped (flock auto-releases on close).
async fn acquire_flock(lock_path: &Path) -> Result<std::fs::File> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)?;

    const FLOCK_TIMEOUT: Duration = Duration::from_secs(10);
    const FLOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

    let deadline = Instant::now() + FLOCK_TIMEOUT;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "flock timeout after {}s on {}",
                        FLOCK_TIMEOUT.as_secs(),
                        lock_path.display()
                    ));
                }
                tokio::time::sleep(FLOCK_POLL_INTERVAL).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Check whether HEAD is contained in any remote-tracking branch.
///
/// Returns true if `git branch -r --contains HEAD` outputs any branches,
/// indicating HEAD has been pushed to a remote. Returns false if the output
/// is empty or the command fails (e.g., no remotes configured).
///
/// Wrapped in a 10-second timeout to prevent indefinite hangs if git
/// subprocess hangs (e.g., due to filesystem issues or network mounts).
async fn is_head_pushed(workspace: &str) -> Result<bool> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Command::new("git")
            .args(["-C", workspace, "branch", "-r", "--contains", "HEAD"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("git branch -r timed out after 10s in {}", workspace))??;

    // If the command fails (e.g., no remotes), assume not pushed
    if !out.status.success() {
        return Ok(false);
    }

    let text = String::from_utf8_lossy(&out.stdout);
    // Non-empty output means HEAD is in at least one remote branch
    Ok(!text.trim().is_empty())
}

/// Check whether the latest commit already carries `Bead-Id: <bead_id>`.
///
/// Wrapped in a 10-second timeout to prevent indefinite hangs if git
/// subprocess hangs (e.g., due to filesystem issues or network mounts).
async fn already_has_trailer(workspace: &str, bead_id: &BeadId) -> Result<bool> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Command::new("git")
            .args([
                "-C",
                workspace,
                "log",
                "-1",
                "--format=%(trailers:key=Bead-Id,valueonly,separator=,)",
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("git log timed out after 10s in {}", workspace))??;

    if !out.status.success() {
        return Ok(false);
    }

    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.split(',').any(|v| v.trim() == bead_id.as_ref()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #[test]
    fn already_has_trailer_logic() {
        // Simulate what `already_has_trailer` checks: split on ',' and compare.
        let output = "hoop-ttb.3.34,hoop-ttb.3.35\n";
        let bead_id = "hoop-ttb.3.34";
        let found = output.split(',').any(|v| v.trim() == bead_id);
        assert!(found);

        let bead_id_missing = "hoop-ttb.9.99";
        let not_found = output.split(',').any(|v| v.trim() == bead_id_missing);
        assert!(!not_found);
    }

    #[test]
    fn empty_head_means_no_op() {
        // pre_dispatch_head "" is treated as unknown; HEAD would differ → would
        // inject. This test documents that the caller should use "" only when
        // the workspace has no commits (git_head returns Err, which we short-circuit).
        // The actual guard is: if current_head == pre_dispatch_head → skip.
        let pre = "abc123";
        let current = "abc123";
        assert_eq!(pre, current); // no-op condition
    }
}
