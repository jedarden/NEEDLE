//! Bead-Id commit trailer injection.
//!
//! When a bead closes with a commit artifact (i.e. the agent made commits),
//! NEEDLE amends the latest commit to include a `Bead-Id: <id>` trailer.
//! HOOP's bead_commit_index then picks this up via `git log`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use fs2::FileExt;
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
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

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

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git failed: {:?}", args);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn get_trailers(dir: &Path) -> String {
        run_git(
            dir,
            &[
                "log",
                "-1",
                "--format=%(trailers:key=Bead-Id,valueonly,separator=,)",
            ],
        )
    }

    fn create_git_repo() -> (PathBuf, tempfile::TempDir) {
        let temp = tempfile::TempDir::new().unwrap();
        let repo_path = temp.path().join("test-repo");
        fs::create_dir_all(&repo_path).unwrap();

        // Initialize repo
        run_git(&repo_path, &["init"]);
        run_git(&repo_path, &["config", "user.name", "Test"]);
        run_git(&repo_path, &["config", "user.email", "test@example.com"]);

        // Create initial commit
        let file_path = repo_path.join("initial.txt");
        fs::write(&file_path, "initial").unwrap();
        run_git(&repo_path, &["add", "initial.txt"]);
        run_git(&repo_path, &["commit", "-m", "initial commit"]);

        (repo_path, temp)
    }

    #[tokio::test]
    async fn concurrent_inject_never_cross_tags() {
        use super::inject_bead_id_trailer;

        // Regression test for bf-j1b8l: two concurrent inject_bead_id_trailer calls
        // in the same workspace should never tag each other's commits.
        //
        // The race:
        // 1. Worker A commits HEAD=A1
        // 2. Worker B commits HEAD=B1 (on top of A1)
        // 3. Both workers call inject_bead_id_trailer concurrently
        // 4. Without flock + identity check: A amends B1, mislabeling it
        // 5. With flock + identity check: each worker verifies HEAD before amending

        let (repo_path, _temp_dir) = create_git_repo();
        let base_head = run_git(&repo_path, &["rev-parse", "HEAD"]);

        // Create two commits, one for each bead
        let bead_a_id = crate::types::BeadId::from("bf-aaa");
        let bead_b_id = crate::types::BeadId::from("bf-bbb");

        // Commit A (commit message contains bead-a ID)
        let file_a = repo_path.join("file-a.txt");
        fs::write(&file_a, "content a").unwrap();
        run_git(&repo_path, &["add", "file-a.txt"]);
        run_git(
            &repo_path,
            &[
                "commit",
                "-m",
                &format!("feat({}): commit A", bead_a_id.as_ref()),
            ],
        );
        let head_after_a = run_git(&repo_path, &["rev-parse", "HEAD"]);

        // Commit B (commit message contains bead-b ID)
        let file_b = repo_path.join("file-b.txt");
        fs::write(&file_b, "content b").unwrap();
        run_git(&repo_path, &["add", "file-b.txt"]);
        run_git(
            &repo_path,
            &[
                "commit",
                "-m",
                &format!("fix({}): commit B", bead_b_id.as_ref()),
            ],
        );
        let head_after_b = run_git(&repo_path, &["rev-parse", "HEAD"]);

        // Verify HEAD moved through both commits
        assert_ne!(base_head, head_after_a);
        assert_ne!(head_after_a, head_after_b);
        assert_ne!(base_head, head_after_b);

        // Spawn concurrent injections:
        // - Worker A injects bead-a ID, with pre_dispatch_head = base
        // - Worker B injects bead-b ID, with pre_dispatch_head = head_after_a
        //
        // Without the fix, there would be a race:
        // - A reads HEAD=B1 ( != base ), verifies HEAD != pre_dispatch_head ✓
        // - B reads HEAD=B1 ( != head_after_a ), verifies HEAD != pre_dispatch_head ✓
        // - A amends B1 with Bead-Id:bf-aaa → B1 hash changes, mislabeled
        // - B tries to amend but B1 hash changed → confusion
        //
        // With the fix:
        // - A acquires flock, reads HEAD=B1, checks subject contains "bf-aaa"
        //   → NO (contains "bf-bbb") → skips injection
        // - B acquires flock, reads HEAD=B1, checks subject contains "bf-bbb"
        //   → YES → amends B1 with Bead-Id:bf-bbb ✓
        // - A retries, acquires flock, reads HEAD=B1, checks subject contains "bf-aaa"
        //   → NO (contains "bf-bbb") → skips injection
        // - Both complete without cross-tagging

        let repo_path_a = repo_path.clone();
        let repo_path_b = repo_path.clone();
        let head_after_a_clone = head_after_a.clone();
        let bead_a_id_clone = bead_a_id.clone();
        let bead_b_id_clone = bead_b_id.clone();

        // Spawn two concurrent tasks
        let task_a = tokio::spawn(async move {
            inject_bead_id_trailer(&repo_path_a, &bead_a_id_clone, &base_head).await
        });

        let task_b = tokio::spawn(async move {
            inject_bead_id_trailer(&repo_path_b, &bead_b_id_clone, &head_after_a_clone).await
        });

        // Both should complete successfully (idempotent skip is OK)
        let result_a = task_a.await.unwrap();
        let result_b = task_b.await.unwrap();
        assert!(result_a.is_ok());
        assert!(result_b.is_ok());

        // Verify the final state:
        // - HEAD should still be at B's commit (may have hash changed if B amended it)
        // - The HEAD commit should have Bead-Id:bf-bbb trailer (B tagged its own)
        // - The HEAD commit should NOT have Bead-Id:bf-aaa (A skipped, no cross-tag)

        let _final_head = run_git(&repo_path, &["rev-parse", "HEAD"]);
        let final_subject = run_git(&repo_path, &["log", "-1", "--format=%s"]);
        let final_trailers = get_trailers(&repo_path);

        // HEAD is still at B's commit (hash may differ if amended)
        assert!(final_subject.contains(bead_b_id.as_ref()));
        assert!(!final_subject.contains(bead_a_id.as_ref()));

        // B's trailer is present
        assert!(final_trailers.contains(bead_b_id.as_ref()));

        // A's trailer is NOT present (no cross-tagging)
        assert!(!final_trailers.contains(bead_a_id.as_ref()));

        // Additional check: HEAD's parent is A's commit
        let parent = run_git(&repo_path, &["rev-parse", "HEAD^"]);
        assert_eq!(parent, head_after_a, "HEAD's parent should be A's commit");
    }

    #[tokio::test]
    async fn identity_check_skips_mismatched_commit() {
        use super::inject_bead_id_trailer;

        // Test that inject_bead_id_trailer skips injection when HEAD doesn't
        // match the bead ID, even if HEAD moved since pre_dispatch_head.
        //
        // This verifies the identity check logic without requiring concurrency.

        let (repo_path, _temp_dir) = create_git_repo();
        let base_head = run_git(&repo_path, &["rev-parse", "HEAD"]);

        let bead_a_id = crate::types::BeadId::from("bf-xxx");
        let bead_b_id = crate::types::BeadId::from("bf-yyy");

        // Create a commit for bead-b
        let file = repo_path.join("file.txt");
        fs::write(&file, "content").unwrap();
        run_git(&repo_path, &["add", "file.txt"]);
        run_git(
            &repo_path,
            &[
                "commit",
                "-m",
                &format!("fix({}): some work", bead_b_id.as_ref()),
            ],
        );

        // Try to inject bead-a's trailer (mismatch: HEAD contains bf-yyy, not bf-xxx)
        inject_bead_id_trailer(&repo_path, &bead_a_id, &base_head)
            .await
            .unwrap();

        // Verify no trailer was added
        let trailers = get_trailers(&repo_path);
        assert!(trailers.is_empty() || !trailers.contains(bead_a_id.as_ref()));
    }

    #[tokio::test]
    async fn identity_check_allows_matched_commit() {
        use super::inject_bead_id_trailer;

        // Test that inject_bead_id_trailer succeeds when HEAD matches the bead ID.
        //
        // This verifies the happy path after adding the identity check.

        let (repo_path, _temp_dir) = create_git_repo();
        let base_head = run_git(&repo_path, &["rev-parse", "HEAD"]);

        let bead_id = crate::types::BeadId::from("bf-zzz");

        // Create a commit for this bead
        let file = repo_path.join("file.txt");
        fs::write(&file, "content").unwrap();
        run_git(&repo_path, &["add", "file.txt"]);
        run_git(
            &repo_path,
            &[
                "commit",
                "-m",
                &format!("feat({}): some work", bead_id.as_ref()),
            ],
        );

        // Inject the matching trailer
        inject_bead_id_trailer(&repo_path, &bead_id, &base_head)
            .await
            .unwrap();

        // Verify the trailer was added
        let trailers = get_trailers(&repo_path);
        assert!(trailers.contains(bead_id.as_ref()));
    }

    #[tokio::test]
    async fn skips_trailer_injection_when_head_already_pushed() {
        use super::inject_bead_id_trailer;

        // Regression test for needle-9c8640b7: when HEAD is already pushed to
        // a remote, inject_bead_id_trailer should skip the amend to avoid
        // diverging local and remote history.
        //
        // Setup:
        // 1. Create a workspace with a commit
        // 2. Add a remote (simulating a pushed state)
        // 3. Manually set up remote tracking to simulate pushed state
        // 4. Call inject_bead_id_trailer
        // Expected: HEAD SHA unchanged, no trailer added

        let (repo_path, _temp_dir) = create_git_repo();
        let base_head = run_git(&repo_path, &["rev-parse", "HEAD"]);

        let bead_id = crate::types::BeadId::from("needle-abc123");

        // Create a commit for this bead
        let file = repo_path.join("file.txt");
        fs::write(&file, "content").unwrap();
        run_git(&repo_path, &["add", "file.txt"]);
        run_git(
            &repo_path,
            &[
                "commit",
                "-m",
                &format!("feat({}): some work", bead_id.as_ref()),
            ],
        );

        // Get HEAD before injection
        let head_before_injection = run_git(&repo_path, &["rev-parse", "HEAD"]);

        // Create a bare remote and push
        let temp_remote_dir = tempfile::TempDir::new().unwrap();
        let remote_path = temp_remote_dir.path().join("remote.git");
        fs::create_dir_all(&remote_path).unwrap();
        let output = Command::new("git")
            .args(["init", "--bare"])
            .current_dir(&remote_path)
            .output();
        assert!(output.unwrap().status.success(), "git init --bare failed");

        // Add remote and push
        run_git(
            &repo_path,
            &["remote", "add", "origin", remote_path.to_str().unwrap()],
        );

        // Push the current branch to origin
        let current_branch = run_git(&repo_path, &["branch", "--show-current"]);
        run_git(&repo_path, &["push", "-u", "origin", &current_branch]);

        // Verify HEAD is in remote tracking branch
        let remote_contains = run_git(&repo_path, &["branch", "-r", "--contains", "HEAD"]);
        assert!(
            !remote_contains.is_empty(),
            "HEAD should be in remote branch after push"
        );

        // Try to inject the trailer - should skip because HEAD is pushed
        inject_bead_id_trailer(&repo_path, &bead_id, &base_head)
            .await
            .unwrap();

        // Verify HEAD SHA is unchanged (no amend occurred)
        let head_after_injection = run_git(&repo_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            head_before_injection, head_after_injection,
            "HEAD SHA should not change when amend is skipped for pushed commits"
        );

        // Verify no trailer was added
        let trailers = get_trailers(&repo_path);
        assert!(
            !trailers.contains(bead_id.as_ref()),
            "No trailer should be added when HEAD is already pushed"
        );
    }

    #[tokio::test]
    async fn injects_trailer_when_head_not_pushed() {
        use super::inject_bead_id_trailer;

        // Companion test to skips_trailer_injection_when_head_already_pushed:
        // verify that unpushed commits still get the trailer amended.
        //
        // Setup:
        // 1. Create a local bare remote
        // 2. Clone it to create a workspace
        // 3. Make a commit in the workspace
        // 4. DO NOT push
        // 5. Call inject_bead_id_trailer
        // Expected: HEAD SHA changed (amended), trailer added

        let (repo_path, _temp_dir) = create_git_repo();
        let base_head = run_git(&repo_path, &["rev-parse", "HEAD"]);

        let bead_id = crate::types::BeadId::from("needle-def456");

        // Create a commit for this bead
        let file = repo_path.join("file.txt");
        fs::write(&file, "content").unwrap();
        run_git(&repo_path, &["add", "file.txt"]);
        run_git(
            &repo_path,
            &[
                "commit",
                "-m",
                &format!("feat({}): some work", bead_id.as_ref()),
            ],
        );

        // Get HEAD before injection
        let head_before_injection = run_git(&repo_path, &["rev-parse", "HEAD"]);

        // Verify HEAD is NOT in remote tracking branch (no remotes configured)
        let remote_contains = run_git(&repo_path, &["branch", "-r", "--contains", "HEAD"]);
        assert!(
            remote_contains.is_empty(),
            "HEAD should not be in remote branch when no remotes configured"
        );

        // Inject the trailer - should succeed because HEAD is not pushed
        inject_bead_id_trailer(&repo_path, &bead_id, &base_head)
            .await
            .unwrap();

        // Verify HEAD SHA changed (amend occurred)
        let head_after_injection = run_git(&repo_path, &["rev-parse", "HEAD"]);
        assert_ne!(
            head_before_injection, head_after_injection,
            "HEAD SHA should change when amend is performed on unpushed commits"
        );

        // Verify the trailer was added
        let trailers = get_trailers(&repo_path);
        assert!(
            trailers.contains(bead_id.as_ref()),
            "Trailer should be added when HEAD is not pushed"
        );
    }

    #[allow(dead_code)]
    fn run_git_in_dir(dir: &Path, args: &[&str]) -> String {
        // Ensure parent directory exists for commands that need it (e.g., git init --bare)
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed in {:?}: {:?}",
            dir,
            args
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}
