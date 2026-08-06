//! Shipped-work verification: did a bead's closure correspond to real,
//! durable output?
//!
//! Gated by `worker.enforce_shipped_work` (default `true`). Runs only when an
//! agent has already closed the bead (see `outcome::handle_success`). Passes
//! if either:
//! - a commit was made since dispatch started (per the `validation::predispatch`
//!   snapshot) that touches at least one file outside `notes/`/`.beads/`, and
//!   that commit has been pushed to the upstream remote; or
//! - the bead's own `notes` field changed during this dispatch — i.e. the agent
//!   ran `bf update --notes` to record why no code change was needed.
//!
//! Deliberately does NOT accept a commit touching only `notes/`/`.beads/` as
//! sufficient on its own: a prior incident (see docs/notes on ARMOR's
//! commit-storm) showed a worker stuck retrying an uncompletable bead will
//! happily satisfy a bare "must have a commit" rule by committing a trivial
//! "still blocked" doc file every cycle, each one triggering paired CI
//! version-bump commits. Recording that kind of status belongs on the bead
//! (via `bf update`/`bf comments add`), not in git history.
//!
//! # Why not `updated_at`
//!
//! The original fallback compared `post.updated_at > pre.updated_at`. That is
//! unsound: `bf close` *is* an update, so the timestamp always advances on the
//! exact path the gate exists to judge — an agent that closed a bead having
//! shipped nothing. Combined with a missing snapshot writer, this made the gate
//! inert from the day it shipped (2026-07-30) until this change: no closure was
//! ever rejected, and no bead ever received the `verification-failed` label.
//! Comparing the `notes` field instead keys on something only a deliberate
//! `bf update --notes` changes.
//!
//! Depends on: `types`, `validation::predispatch`.

use std::path::Path;

use anyhow::Result;

use crate::types::Bead;
use crate::validation::predispatch::{self, hash_notes};
use crate::validation::GateResult;

/// Paths that don't count as "substantial" on their own. A commit touching
/// only these is treated the same as no commit at all — see module docs.
///
/// `.needle-predispatch-sha` is listed because stale, git-tracked copies of that
/// file linger in several workspaces from an earlier NEEDLE build. An agent's
/// `git commit -a` sweeps the file in, which would otherwise launder a
/// notes-only commit into one touching a "substantial" path.
const TRIVIAL_PATH_PREFIXES: &[&str] = &["notes/", ".beads/", ".needle-predispatch-sha"];

/// Verify that a bead's closure corresponds to shipped (committed + pushed)
/// work, or an explicit bead note recording why none was needed.
///
/// `post` is the freshly fetched bead at closure-check time. `workspace` is the
/// bead's workspace directory (`bead.workspace` / `source_repo`). The
/// pre-dispatch baseline comes from the `predispatch` snapshot recorded by the
/// worker before the agent ran.
pub async fn verify_shipped_work(post: &Bead, workspace: &Path) -> Result<GateResult> {
    let snapshot = predispatch::load(workspace, &post.id).await;
    // `Bead` does not carry `notes`, so read the current value the same way the
    // snapshot did.
    let post_notes = predispatch::current_notes(workspace, &post.id)
        .await
        .unwrap_or_default();

    evaluate(workspace, snapshot.as_ref(), &post_notes).await
}

/// Gate logic with all external state passed in, so tests can exercise every
/// branch without touching `HOME` or requiring a `bf` workspace.
async fn evaluate(
    workspace: &Path,
    snapshot: Option<&predispatch::PreDispatch>,
    post_notes: &str,
) -> Result<GateResult> {
    if let Some(result) = check_commit(workspace, snapshot).await? {
        return Ok(result);
    }

    // Fallback: the agent recorded an explanation on the bead itself.
    match snapshot.and_then(|s| s.notes_hash.as_deref()) {
        Some(pre_hash) => {
            if hash_notes(post_notes) != pre_hash {
                return Ok(GateResult::Pass);
            }
        }
        None => {
            // No baseline to compare against (snapshot missing or notes
            // unreadable). Accept a non-empty note rather than failing a bead
            // the gate cannot actually judge.
            if !post_notes.trim().is_empty() {
                return Ok(GateResult::Pass);
            }
        }
    }

    Ok(GateResult::Fail(
        "no substantial pushed commit and no bead note recorded for this dispatch — \
         commit real work, or run `bf update --notes \"...\"` explaining why none was needed"
            .to_string(),
    ))
}

/// Checks the git side. Returns `Ok(None)` to mean "no verdict from git,
/// check the fallback" (no snapshot, no new commit, or only trivial paths
/// changed) rather than a hard pass/fail.
async fn check_commit(
    workspace: &Path,
    snapshot: Option<&predispatch::PreDispatch>,
) -> Result<Option<GateResult>> {
    // No baseline — can't determine what this dispatch changed; fall through.
    let pre_sha = match snapshot.and_then(|s| s.head_sha.as_deref()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Ok(None),
    };

    let head = match git_output(workspace, &["rev-parse", "HEAD"]).await {
        Ok(h) => h,
        Err(_) => return Ok(None),
    };
    if head == pre_sha {
        return Ok(None); // No new commit.
    }

    let changed = git_output(workspace, &["diff", "--name-only", &pre_sha, &head])
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
    use crate::validation::predispatch::PreDispatch;
    use tempfile::TempDir;

    fn snapshot(head: Option<&str>, notes: Option<&str>) -> PreDispatch {
        PreDispatch {
            head_sha: head.map(|s| s.to_string()),
            notes_hash: notes.map(hash_notes),
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
    }

    async fn init_repo(dir: &Path) -> String {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "test"]);
        std::fs::write(dir.join("README.md"), "init\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "init"]);
        git_output(dir, &["rev-parse", "HEAD"]).await.unwrap()
    }

    /// Give `dir` a bare upstream and push, so `@{u}` resolves.
    async fn push_upstream(dir: &Path, bare: &Path) {
        git(bare, &["init", "-q", "--bare"]);
        git(dir, &["remote", "add", "origin", bare.to_str().unwrap()]);
        let branch = git_output(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap();
        git(dir, &["push", "-q", "-u", "origin", &branch]);
    }

    fn commit_files(dir: &Path, files: &[(&str, &str)], msg: &str) {
        for (path, contents) in files {
            let full = dir.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full, contents).unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", msg]);
    }

    // ── fallback: bead notes ──

    #[tokio::test]
    async fn no_snapshot_and_no_notes_fails() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let result = evaluate(dir.path(), None, "").await.unwrap();
        assert!(matches!(result, GateResult::Fail(_)));
    }

    #[tokio::test]
    async fn no_snapshot_but_a_recorded_note_passes() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let result = evaluate(dir.path(), None, "already implemented in 4f2a1c")
            .await
            .unwrap();
        assert_eq!(result, GateResult::Pass);
    }

    #[tokio::test]
    async fn notes_changed_during_dispatch_passes_without_a_commit() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let snap = snapshot(Some(&head), Some(""));
        let result = evaluate(dir.path(), Some(&snap), "checked: no code change needed")
            .await
            .unwrap();
        assert_eq!(result, GateResult::Pass);
    }

    /// The hole this gate was written for and did not close: an agent that
    /// closes a bead having shipped nothing. `bf close` bumps `updated_at`, so
    /// the old timestamp fallback passed here unconditionally.
    #[tokio::test]
    async fn closing_without_shipping_or_noting_anything_fails() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let snap = snapshot(Some(&head), Some("pre-existing note"));
        let result = evaluate(dir.path(), Some(&snap), "pre-existing note")
            .await
            .unwrap();
        assert!(
            matches!(result, GateResult::Fail(_)),
            "unchanged notes and no commit must not satisfy the gate"
        );
    }

    // ── git side ──

    #[tokio::test]
    async fn substantial_pushed_commit_passes() {
        let dir = TempDir::new().unwrap();
        let bare = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        push_upstream(dir.path(), bare.path()).await;
        commit_files(dir.path(), &[("src.rs", "fn main() {}\n")], "real work");
        git(dir.path(), &["push", "-q"]);

        let snap = snapshot(Some(&head), Some(""));
        assert_eq!(
            evaluate(dir.path(), Some(&snap), "").await.unwrap(),
            GateResult::Pass
        );
    }

    #[tokio::test]
    async fn substantial_unpushed_commit_fails() {
        let dir = TempDir::new().unwrap();
        let bare = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        push_upstream(dir.path(), bare.path()).await;
        commit_files(dir.path(), &[("src.rs", "fn main() {}\n")], "unpushed");

        let snap = snapshot(Some(&head), Some(""));
        match evaluate(dir.path(), Some(&snap), "").await.unwrap() {
            GateResult::Fail(reason) => assert!(reason.contains("not been pushed")),
            GateResult::Pass => panic!("expected Fail for unpushed commit"),
        }
    }

    #[tokio::test]
    async fn notes_only_commit_is_treated_as_trivial() {
        let dir = TempDir::new().unwrap();
        let bare = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        push_upstream(dir.path(), bare.path()).await;
        commit_files(
            dir.path(),
            &[("notes/bf-test.md", "attempted again, still blocked\n")],
            "docs(bf-test): document attempt",
        );
        git(dir.path(), &["push", "-q"]);

        // Notes unchanged on the bead too — the exact ARMOR commit-storm shape.
        let snap = snapshot(Some(&head), Some(""));
        match evaluate(dir.path(), Some(&snap), "").await.unwrap() {
            GateResult::Fail(_) => {}
            GateResult::Pass => panic!("a notes-only commit must not satisfy the gate on its own"),
        }
    }

    /// Stale `.needle-predispatch-sha` files are git-tracked in several live
    /// workspaces. Sweeping one into a notes-only commit must not launder it
    /// into "substantial" work.
    #[tokio::test]
    async fn predispatch_marker_does_not_launder_a_notes_only_commit() {
        let dir = TempDir::new().unwrap();
        let bare = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        push_upstream(dir.path(), bare.path()).await;
        commit_files(
            dir.path(),
            &[
                ("notes/bf-test.md", "still blocked\n"),
                (
                    ".needle-predispatch-sha",
                    "0000000000000000000000000000000000000000\n",
                ),
            ],
            "docs(bf-test): document attempt",
        );
        git(dir.path(), &["push", "-q"]);

        let snap = snapshot(Some(&head), Some(""));
        match evaluate(dir.path(), Some(&snap), "").await.unwrap() {
            GateResult::Fail(_) => {}
            GateResult::Pass => {
                panic!("the marker file must not make a notes-only commit substantial")
            }
        }
    }

    #[tokio::test]
    async fn beads_only_commit_is_treated_as_trivial() {
        let dir = TempDir::new().unwrap();
        let bare = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        push_upstream(dir.path(), bare.path()).await;
        commit_files(
            dir.path(),
            &[(".beads/issues.jsonl", "{\"id\":\"bf-test\"}\n")],
            "chore: flush beads",
        );
        git(dir.path(), &["push", "-q"]);

        let snap = snapshot(Some(&head), Some(""));
        assert!(matches!(
            evaluate(dir.path(), Some(&snap), "").await.unwrap(),
            GateResult::Fail(_)
        ));
    }

    #[tokio::test]
    async fn no_new_commit_falls_through_to_the_notes_check() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let snap = snapshot(Some(&head), Some(""));
        // HEAD == snapshot, notes changed -> pass via fallback.
        assert_eq!(
            evaluate(dir.path(), Some(&snap), "investigated")
                .await
                .unwrap(),
            GateResult::Pass
        );
    }
}
