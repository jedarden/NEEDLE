//! Shipped-work verification: did a bead's closure correspond to real,
//! durable output?
//!
//! Gated by `worker.enforce_shipped_work` (default `true`). Runs only when an
//! agent has already closed the bead (see `outcome::handle_success`). Passes
//! if either:
//! - a commit was made since dispatch started (per `.needle-predispatch-sha`,
//!   written by the agent adapter's `invoke_template` before each dispatch)
//!   that touches at least one file outside `notes/`/`.beads/`, and that
//!   commit has been pushed to the upstream remote; or
//! - the bead itself was updated during this dispatch (`bead.updated_at`
//!   advanced past the pre-dispatch snapshot) — e.g. `bf update --notes` to
//!   record why no code change was needed.
//!
//! Deliberately does NOT accept a commit touching only `notes/`/`.beads/` as
//! sufficient on its own: a prior incident (see docs/notes on ARMOR's
//! commit-storm) showed a worker stuck retrying an uncompletable bead will
//! happily satisfy a bare "must have a commit" rule by committing a trivial
//! "still blocked" doc file every cycle, each one triggering paired CI
//! version-bump commits. Recording that kind of status belongs on the bead
//! (via `bf update`/`bf comments add`), not in git history.

use std::path::Path;

use anyhow::Result;

use crate::types::Bead;
use crate::validation::GateResult;

/// Paths that don't count as "substantial" on their own. A commit touching
/// only these is treated the same as no commit at all — see module docs.
const TRIVIAL_PATH_PREFIXES: &[&str] = &["notes/", ".beads/"];

/// Verify that a bead's closure corresponds to shipped (committed + pushed)
/// work, or an explicit bead update recording why none was needed.
///
/// `pre` is the bead as claimed, before dispatch. `post` is the freshly
/// fetched bead at closure-check time. `workspace` is the bead's workspace
/// directory (`bead.workspace` / `source_repo`).
pub async fn verify_shipped_work(pre: &Bead, post: &Bead, workspace: &Path) -> Result<GateResult> {
    if let Some(result) = check_commit(workspace).await? {
        return Ok(result);
    }

    // Fallback: the bead itself was explicitly updated during this dispatch.
    if post.updated_at > pre.updated_at {
        return Ok(GateResult::Pass);
    }

    Ok(GateResult::Fail(
        "no substantial pushed commit and no bead update recorded for this dispatch — \
         commit real work, or run `bf update --notes \"...\"` explaining why none was needed"
            .to_string(),
    ))
}

/// Checks the git side. Returns `Ok(None)` to mean "no verdict from git,
/// check the fallback" (no predispatch marker, no new commit, or only
/// trivial paths changed) rather than a hard pass/fail.
async fn check_commit(workspace: &Path) -> Result<Option<GateResult>> {
    let sha_file = workspace.join(".needle-predispatch-sha");
    let pre_sha_raw = match tokio::fs::read_to_string(&sha_file).await {
        Ok(s) => s,
        Err(_) => return Ok(None), // No marker — can't determine a baseline; fail open to fallback.
    };
    let pre_sha = pre_sha_raw.trim();
    if pre_sha.is_empty() {
        return Ok(None);
    }

    let head = match git_output(workspace, &["rev-parse", "HEAD"]).await {
        Ok(h) => h,
        Err(_) => return Ok(None),
    };
    if head == pre_sha {
        return Ok(None); // No new commit.
    }

    let changed = git_output(workspace, &["diff", "--name-only", pre_sha, &head])
        .await
        .unwrap_or_default();
    let substantial = changed
        .lines()
        .any(|f| !TRIVIAL_PATH_PREFIXES.iter().any(|p| f.starts_with(p)));
    if !substantial {
        return Ok(None); // Only notes/.beads touched — treat like no commit.
    }

    let pushed = git_output(workspace, &["merge-base", "--is-ancestor", &head, "@{u}"])
        .await
        .is_ok();
    if pushed {
        Ok(Some(GateResult::Pass))
    } else {
        Ok(Some(GateResult::Fail(format!(
            "commit {} has substantial changes but has not been pushed to the remote",
            &head[..head.len().min(7)]
        ))))
    }
}

async fn git_output(workspace: &Path, args: &[&str]) -> Result<String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BeadStatus;
    use chrono::{Duration, Utc};
    use tempfile::TempDir;

    fn test_bead(updated_at: chrono::DateTime<Utc>) -> Bead {
        Bead {
            id: "bf-test".into(),
            title: "test".to_string(),
            body: None,
            priority: 2,
            status: BeadStatus::Done,
            assignee: None,
            labels: vec![],
            workspace: std::path::PathBuf::new(),
            dependencies: vec![],
            dependents: vec![],
            created_at: updated_at,
            updated_at,
        }
    }

    async fn init_repo(dir: &Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "test"]);
        std::fs::write(dir.join("README.md"), "init\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
    }

    #[tokio::test]
    async fn no_predispatch_marker_falls_back_to_bead_update_check() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let t0 = Utc::now();
        let pre = test_bead(t0);
        let post = test_bead(t0); // unchanged
        let result = verify_shipped_work(&pre, &post, dir.path()).await.unwrap();
        assert_eq!(
            result,
            GateResult::Fail(
                "no substantial pushed commit and no bead update recorded for this dispatch — \
             commit real work, or run `bf update --notes \"...\"` explaining why none was needed"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn bead_update_since_dispatch_passes_without_a_commit() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let t0 = Utc::now();
        let pre = test_bead(t0);
        let post = test_bead(t0 + Duration::seconds(5));
        let result = verify_shipped_work(&pre, &post, dir.path()).await.unwrap();
        assert_eq!(result, GateResult::Pass);
    }

    #[tokio::test]
    async fn substantial_pushed_commit_passes() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let head = git_output(dir.path(), &["rev-parse", "HEAD"])
            .await
            .unwrap();
        tokio::fs::write(dir.path().join(".needle-predispatch-sha"), &head)
            .await
            .unwrap();

        // Make a real change and commit it.
        std::fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-q", "-m", "real work"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Fake "pushed" by pointing a remote-tracking-shaped ref at HEAD via
        // a bare upstream so `@{u}` resolves.
        let bare = TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q", "--bare"])
            .current_dir(bare.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["remote", "add", "origin", bare.path().to_str().unwrap()])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let branch = git_output(dir.path(), &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "-q", "-u", "origin", &branch])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let t0 = Utc::now();
        let pre = test_bead(t0);
        let post = test_bead(t0);
        let result = verify_shipped_work(&pre, &post, dir.path()).await.unwrap();
        assert_eq!(result, GateResult::Pass);
    }

    #[tokio::test]
    async fn substantial_unpushed_commit_fails() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let head = git_output(dir.path(), &["rev-parse", "HEAD"])
            .await
            .unwrap();
        tokio::fs::write(dir.path().join(".needle-predispatch-sha"), &head)
            .await
            .unwrap();

        std::fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-q", "-m", "real work, unpushed"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let t0 = Utc::now();
        let pre = test_bead(t0);
        let post = test_bead(t0);
        let result = verify_shipped_work(&pre, &post, dir.path()).await.unwrap();
        match result {
            GateResult::Fail(reason) => assert!(reason.contains("not been pushed")),
            GateResult::Pass => panic!("expected Fail for unpushed commit"),
        }
    }

    #[tokio::test]
    async fn notes_only_commit_is_treated_as_trivial() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let head = git_output(dir.path(), &["rev-parse", "HEAD"])
            .await
            .unwrap();
        tokio::fs::write(dir.path().join(".needle-predispatch-sha"), &head)
            .await
            .unwrap();

        std::fs::create_dir_all(dir.path().join("notes")).unwrap();
        std::fs::write(
            dir.path().join("notes/bf-test.md"),
            "attempted again, still blocked\n",
        )
        .unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-q", "-m", "docs(bf-test): document attempt"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // No bead update either — this is the exact ARMOR commit-storm shape.
        let t0 = Utc::now();
        let pre = test_bead(t0);
        let post = test_bead(t0);
        let result = verify_shipped_work(&pre, &post, dir.path()).await.unwrap();
        match result {
            GateResult::Fail(_) => {}
            GateResult::Pass => panic!("a notes-only commit must not satisfy the gate on its own"),
        }
    }
}
