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
//!   recorded a bead note explaining why no code change was needed.
//!
//! Every closure that fails the shipped-work check increments the failure
//! count, and after the threshold the bead is quarantined (set deferred)
//! rather than reopened. This prevents beads that close without shipped work
//! (e.g., GitHub comments, external API calls) from looping forever. See
//! GitHub issue #16 (bead needle-0fbf5145 cycled 14 times posting identical
//! comments).
//!
//! Deliberately does NOT accept a commit touching only `notes/`/`.beads/` as
//! sufficient on its own: a prior incident (see docs/notes on ARMOR's
//! commit-storm) showed a worker stuck retrying an uncompletable bead will
//! happily satisfy a bare "must have a commit" rule by committing a trivial
//! "still blocked" doc file every cycle, each one triggering paired CI
//! version-bump commits. Recording that kind of status belongs on the bead
//! through the workspace's configured backend, not in git history.
//!
//! # No upstream configured
//!
//! A workspace with no upstream for its branch (`git rev-parse @{u}` does not
//! resolve — e.g. plain `git init`, or a remote added without `push -u`) gets
//! `GateResult::Unsatisfiable`, not `Fail`. The gate literally cannot check a
//! push there, so every closure would fail regardless of what the agent
//! produced, burning the retry counter toward quarantine and mitosis splitting
//! on unjudged work (GitHub issue #18: three correct commits rejected, parent
//! split into seven children in ~70s). An unsatisfiable gate is not a work
//! failure — it releases without touching the failure count and is classified
//! as `Outcome::GateUnsatisfiable` for downstream attribution.
//!
//! # Why not `updated_at`
//!
//! The original fallback compared `post.updated_at > pre.updated_at`. That is
//! unsound: bead close *is* an update, so the timestamp always advances on the
//! exact path the gate exists to judge — an agent that closed a bead having
//! shipped nothing. Combined with a missing snapshot writer, this made the gate
//! inert from the day it shipped (2026-07-30) until this change: no closure was
//! ever rejected, and no bead ever received the `verification-failed` label.
//! Comparing the `notes` field instead keys on a deliberate note update.
//!
//! # Missing snapshot
//!
//! The baseline normally comes from the on-disk snapshot, but that file is
//! single-slot per (workspace, bead): when duplicate dispatches land on one
//! bead, the first to finish cleared it and every surviving dispatch
//! permanently bounced with "no pre-dispatch snapshot recorded" regardless of
//! evidence (bead needle-e4fbe47c: work shipped and verified four times, five
//! closes bounced until an operator intervened). Two defenses:
//!
//! - `verify_shipped_work` accepts a `fallback` baseline — the dispatch's
//!   own in-memory pre-dispatch HEAD, threaded through the attempt context —
//!   substituted whenever the file is gone. Same evidence quality, because
//!   the worker captured it itself right before the agent ran.
//! - With no baseline of any kind, the gate judges what it still can: an
//!   explicit bead note passes (the same epistemics as notes that were
//!   unreadable at dispatch — accept rather than fail a comparison the gate
//!   could not make, and the only remedy the failure text has ever offered).
//!   A closure with neither a commit nor a note still fails, keeping the
//!   failure-quarantine circuit reachable (GitHub issue #16).
//!
//! Depends on: `types`, `validation::predispatch`.

use std::path::Path;

use anyhow::Result;

use crate::bead_store::BeadStore;
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
///
/// This function always runs against the git repository's committed state, not
/// uncommitted working tree changes. The git commands it uses (`git rev-parse`,
/// `git diff`, `git merge-base`) operate on commits only and ignore the
/// working tree.
///
/// For beads labeled `deliverable:external`, the git check is skipped and
/// the gate requires machine-checkable evidence: notes must have changed
/// during the dispatch AND contain a line beginning with `evidence:`.
///
/// `fallback` substitutes for the on-disk snapshot when that file is missing
/// — most commonly because a completing twin dispatch cleared it. It is the
/// dispatch's own in-memory pre-dispatch HEAD (attempt context
/// `bead_revision_start`), captured by the same worker right before the agent
/// ran, so it judges the closure exactly as the file snapshot would have.
pub async fn verify_shipped_work(
    post: &Bead,
    workspace: &Path,
    store: &dyn BeadStore,
    fallback: Option<&predispatch::PreDispatch>,
) -> Result<GateResult> {
    let snapshot = match predispatch::load(workspace, &post.id).await {
        Some(s) => Some(s),
        None => fallback.cloned(),
    };
    // `Bead` does not carry `notes`, so read the current value the same way the
    // snapshot did.
    let post_notes = predispatch::current_notes(store, &post.id)
        .await
        .unwrap_or_default();

    evaluate(
        workspace,
        snapshot.as_ref(),
        &post_notes,
        post.labels.contains(&"deliverable:external".to_string()),
    )
    .await
}

/// Gate logic with all external state passed in, so tests can exercise every
/// branch without touching `HOME` or requiring a `bf` workspace.
async fn evaluate(
    workspace: &Path,
    snapshot: Option<&predispatch::PreDispatch>,
    post_notes: &str,
    is_external: bool,
) -> Result<GateResult> {
    // No baseline at all means the gate cannot attribute a commit to this
    // dispatch. It still honors the one remedy it can judge without a
    // baseline — an explicit bead note — by substituting an all-unknown
    // baseline (the same epistemics as notes that were unreadable at
    // dispatch: accept rather than fail a comparison the gate could not
    // make). A closure with neither note nor commit still FAILs, so the
    // failure-quarantine circuit stays reachable (GitHub issue #16: bead
    // needle-0fbf5145 posted 18 identical comments because every closure
    // reset the count).
    let fallback_snapshot;
    let snapshot = match snapshot {
        Some(s) => s,
        None => {
            tracing::debug!(
                workspace = %workspace.display(),
                "no pre-dispatch snapshot — judging the closure on its note alone"
            );
            fallback_snapshot = predispatch::PreDispatch {
                head_sha: None,
                notes_hash: None,
                dirty_files: Vec::new(),
                captured_at: None,
            };
            &fallback_snapshot
        }
    };
    let had_usable_baseline = snapshot.captured_at.is_some() || snapshot.head_sha.is_some();

    // For beads with deliverable:external, skip git check entirely and
    // verify only that notes changed with an evidence: line.
    if is_external {
        return evaluate_external(snapshot, post_notes);
    }

    if let Some(result) = check_commit(workspace, Some(snapshot)).await? {
        return Ok(result);
    }

    // Fallback: the agent recorded an explanation on the bead itself.
    match snapshot.notes_hash.as_deref() {
        Some(pre_hash) => {
            if hash_notes(post_notes) != pre_hash {
                return Ok(GateResult::Pass);
            }
        }
        None => {
            // Notes were unreadable at dispatch (or no baseline was recorded
            // at all), so there is nothing to diff against. Accept a
            // non-empty note rather than failing a bead on a comparison the
            // gate could not make.
            if !post_notes.trim().is_empty() {
                return Ok(GateResult::Pass);
            }
        }
    }

    Ok(GateResult::Fail(if had_usable_baseline {
        "no substantial pushed commit and no bead note recorded for this dispatch — \
         commit real work, or record an explanatory note with the configured bead backend"
            .to_string()
    } else {
        "no pre-dispatch snapshot recorded and no evidence of shipped work: no substantial \
         pushed commit and no bead note. This closure will be treated as a failure and \
         increment the retry counter. Record an explicit bead note explaining why no work \
         was shipped, and ensure the worker is recording predispatch snapshots."
            .to_string()
    }))
}

/// Gate logic for beads labeled with deliverable:external.
///
/// These beads have non-commit deliverables (GitHub comments, external API calls,
/// provisioning steps, verification tasks). The gate skips git verification
/// and requires machine-checkable evidence in the bead notes.
///
/// PASS conditions:
/// - Notes changed during dispatch (hash differs, or non-empty when snapshot hash is None)
/// - Notes contain a line matching `^\s*evidence:\s*\S` (case-insensitive on "evidence")
///
/// FAIL otherwise, with a distinct failure reason for telemetry.
fn evaluate_external(snapshot: &predispatch::PreDispatch, post_notes: &str) -> Result<GateResult> {
    // Check if notes changed during the dispatch
    let notes_changed = match snapshot.notes_hash.as_deref() {
        Some(pre_hash) => hash_notes(post_notes) != pre_hash,
        None => !post_notes.trim().is_empty(),
    };

    if !notes_changed {
        return Ok(GateResult::Fail(
            "deliverable:external bead closed without note update — \
             run: <bead_cli> update <id> --notes \"evidence: <url or identifier>\" then close; \
             close --reason alone does not count"
                .to_string(),
        ));
    }

    // Check for evidence: line (case-insensitive on "evidence")
    let has_evidence = post_notes.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.to_lowercase().starts_with("evidence:") {
            // Check there's non-whitespace after the colon
            let after_colon = trimmed
                .trim_start_matches("evidence:")
                .trim_start_matches("EVIDENCE:");
            !after_colon.is_empty() && after_colon.chars().any(|c| !c.is_whitespace())
        } else {
            false
        }
    });

    if !has_evidence {
        return Ok(GateResult::Fail(
            "deliverable:external bead closed without evidence — \
             run: <bead_cli> update <id> --notes \"evidence: <url or identifier>\" then close; \
             close --reason alone does not count"
                .to_string(),
        ));
    }

    Ok(GateResult::Pass)
}

/// Probe that resolves the current branch's upstream ref, run on its own
/// before the ancestry test. Failing here means "no upstream to compare
/// against" (GitHub issue #18), which is a different verdict from "the commit
/// is unpushed" and must not be reported as the latter.
const UPSTREAM_PROBE_ARGS: &[&str] = &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"];

/// Answer to "does this workspace have an upstream configured for its current
/// branch?" — a separate question from "is the commit pushed", which the
/// ancestry test answers and which can only be asked once an upstream exists
/// (GitHub issue #18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamStatus {
    /// `@{u}` resolved. The string is the upstream's abbreviated symbolic
    /// name, e.g. `origin/main`.
    Present(String),
    /// Git ran, but the branch has no upstream to compare against: plain
    /// `git init`, or a remote added without `push -u`. Git says
    /// `no upstream configured for branch '...'`.
    NotConfigured,
    /// Git could not answer at all — not a repository, detached HEAD, missing
    /// binary, upstream configured but unresolvable. The string carries git's
    /// own stderr so callers report the real cause rather than a guess.
    GitError(String),
}

/// Whether the workspace's current branch has an upstream, judged by
/// `git rev-parse --abbrev-ref --symbolic-full-name @{u}` alone: no commits
/// are read and the working tree is untouched.
///
/// Extracted from the shipped-work gate so callers that only need the
/// upstream question (the `needle doctor` shipped-work readiness check,
/// needle-4fcd150c) consume this predicate rather than reimplementing the
/// probe and drifting from how the gate classifies its outcomes.
pub async fn upstream_status(workspace: &Path) -> UpstreamStatus {
    match git_output(workspace, UPSTREAM_PROBE_ARGS).await {
        Ok(u) if !u.trim().is_empty() => UpstreamStatus::Present(u),
        // A zero exit with no name cannot happen for a resolved `@{u}`; treat
        // it as "nothing to compare against" rather than inventing an upstream.
        Ok(_) => UpstreamStatus::NotConfigured,
        Err(e) if e.to_string().contains("no upstream configured for branch") => {
            UpstreamStatus::NotConfigured
        }
        Err(e) => UpstreamStatus::GitError(e.to_string()),
    }
}

/// Checks the git side. Returns `Ok(None)` to mean "no verdict from git,
/// check the fallback" (no snapshot, no new commit, or only trivial paths
/// changed) rather than a hard pass/fail.
///
/// A workspace with no upstream configured returns
/// `GateResult::Unsatisfiable` instead of an ordinary failure: every commit
/// would be judged the same way until the repository gets an upstream or
/// shipped-work enforcement is disabled.
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

    // With no upstream configured, `@{u}` does not resolve and the ancestry
    // test below would fail for ANY work: a workspace created with plain
    // `git init` could never pass the gate, and every correct closure would be
    // reported as "has not been pushed" (GitHub issue #18). Probe the upstream
    // by name first and keep that outcome distinct — a gate that cannot run is
    // not a gate that failed (needle-4aaa010c precedent).
    let upstream = match upstream_status(workspace).await {
        UpstreamStatus::Present(upstream) => upstream,
        UpstreamStatus::NotConfigured => {
            let branch = git_output(workspace, &["rev-parse", "--abbrev-ref", "HEAD"])
                .await
                .unwrap_or_else(|_| "HEAD".to_string());
            return Ok(Some(GateResult::Unsatisfiable(format!(
                "no upstream configured for branch '{}': the shipped-work gate cannot verify \
                 that the commit was pushed, so this closure is not counted as a failure. \
                 Remedy: `git push -u <remote> {}` — that publishes the branch and sets its \
                 upstream in one step. If no remote exists yet, add one first: `git remote add \
                 origin <url>` (adding a remote that already exists is an error, so check `git \
                 remote -v` first). For a local-only repository, set \
                 `worker.enforce_shipped_work: false`.",
                branch, branch
            ))));
        }
        UpstreamStatus::GitError(stderr) => {
            // Git could not answer the upstream question at all — the ancestry
            // test is equally unrunnable. Same verdict shape as the
            // not-configured case, but the real cause is reported instead of
            // claiming the branch lacks an upstream.
            return Ok(Some(GateResult::ExecutionError {
                command: format!("git {}", UPSTREAM_PROBE_ARGS.join(" ")),
                reason: format!(
                    "the shipped-work gate could not resolve an upstream for this branch \
                     (git said: {stderr}). It cannot verify that the commit was pushed, so \
                     this closure is not counted as a failure. If the branch should have an \
                     upstream, `git push -u <remote> <branch>` sets one."
                ),
            }));
        }
    };

    let pushed = git_output(
        workspace,
        &["merge-base", "--is-ancestor", &head, &upstream],
    )
    .await
    .is_ok();
    if pushed {
        Ok(Some(GateResult::Pass))
    } else {
        Ok(Some(GateResult::Fail(format!(
            "commit {} has substantial changes but has not been pushed to its upstream {}",
            &head[..head.len().min(7)],
            upstream
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
    use chrono::Utc;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn git_ok(workspace: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(workspace)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout(workspace: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(workspace)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git fixture output is UTF-8")
            .trim()
            .to_string()
    }

    fn git_repo() -> TempDir {
        let repo = tempfile::tempdir().expect("create Git fixture");
        git_ok(repo.path(), &["init", "-q"]);
        git_ok(
            repo.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git_ok(repo.path(), &["config", "user.name", "Needle Test"]);
        fs::write(repo.path().join("README.md"), "seed\n").expect("write fixture seed");
        git_ok(repo.path(), &["add", "README.md"]);
        git_ok(repo.path(), &["commit", "-q", "-m", "seed"]);
        repo
    }

    fn commit_work(repo: &TempDir) {
        fs::write(repo.path().join("work.rs"), "fn work() {}\n").expect("write fixture work");
        git_ok(repo.path(), &["add", "work.rs"]);
        git_ok(repo.path(), &["commit", "-q", "-m", "ship work"]);
    }

    fn snapshot_at(head_sha: String) -> predispatch::PreDispatch {
        predispatch::PreDispatch {
            head_sha: Some(head_sha),
            notes_hash: Some(predispatch::hash_notes("")),
            dirty_files: Vec::new(),
            captured_at: Some(Utc::now()),
        }
    }

    fn bare_upstream(repo: &TempDir) -> TempDir {
        let remote = tempfile::tempdir().expect("create bare upstream fixture");
        git_ok(remote.path(), &["init", "-q", "--bare"]);
        git_ok(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                remote.path().to_str().expect("upstream path is UTF-8"),
            ],
        );
        git_ok(repo.path(), &["push", "-q", "-u", "origin", "HEAD"]);
        remote
    }

    async fn verdict(repo: &TempDir, snapshot: &predispatch::PreDispatch) -> GateResult {
        check_commit(repo.path(), Some(snapshot))
            .await
            .expect("shipped-work check should run")
            .expect("substantial committed work should produce a verdict")
    }

    #[tokio::test]
    async fn no_upstream_is_unsatisfiable_and_not_a_failure_verdict() {
        let repo = git_repo();
        let before = git_stdout(repo.path(), &["rev-parse", "HEAD"]);
        commit_work(&repo);

        let result = verdict(&repo, &snapshot_at(before)).await;
        assert!(matches!(result, GateResult::Unsatisfiable(_)));
        assert!(result.is_unsatisfiable());
        assert!(
            result.failure_reason().is_none(),
            "an unsatisfiable precondition must not enter failure accounting"
        );
    }

    #[tokio::test]
    async fn configured_upstream_with_unpushed_head_is_a_work_failure() {
        let repo = git_repo();
        let before = git_stdout(repo.path(), &["rev-parse", "HEAD"]);
        let _remote = bare_upstream(&repo);
        commit_work(&repo);

        let result = verdict(&repo, &snapshot_at(before)).await;
        assert!(
            matches!(result, GateResult::Fail(ref reason) if reason.contains("has not been pushed"))
        );
        assert!(!result.is_unsatisfiable());
    }

    #[tokio::test]
    async fn configured_upstream_with_pushed_head_passes() {
        let repo = git_repo();
        let before = git_stdout(repo.path(), &["rev-parse", "HEAD"]);
        let _remote = bare_upstream(&repo);
        commit_work(&repo);
        git_ok(repo.path(), &["push", "-q"]);

        let result = verdict(&repo, &snapshot_at(before)).await;
        assert_eq!(result, GateResult::Pass);
    }
}
