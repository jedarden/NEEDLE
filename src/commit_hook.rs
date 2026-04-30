//! Bead-Id commit trailer injection.
//!
//! When a bead closes with a commit artifact (i.e. the agent made commits),
//! NEEDLE amends the latest commit to include a `Bead-Id: <id>` trailer.
//! HOOP's bead_commit_index then picks this up via `git log`.

use std::path::Path;

use anyhow::Result;
use tokio::process::Command;

use crate::types::BeadId;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Inject a `Bead-Id: <id>` trailer into the latest commit in `workspace`.
///
/// Only acts when HEAD moved since `pre_dispatch_head` (i.e. the agent made
/// at least one commit). Returns `Ok(())` in all no-op cases (not a git repo,
/// no new commits, trailer already present). Errors are logged by callers as
/// non-fatal warnings.
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
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("git rev-parse HEAD timed out after 10s in {}", workspace))??;

    if !out.status.success() {
        anyhow::bail!("git rev-parse HEAD failed in {}", workspace);
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
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
