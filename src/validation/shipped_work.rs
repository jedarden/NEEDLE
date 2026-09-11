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
//! `GateResult::ExecutionError`, not `Fail`. The gate literally cannot check a
//! push there, so every closure would fail regardless of what the agent
//! produced, burning the retry counter toward quarantine and mitosis splitting
//! on unjudged work (GitHub issue #18: three correct commits rejected, parent
//! split into seven children in ~70s). A gate that cannot run is not a gate
//! that failed — the `handle_gate_error` precedent (needle-4aaa010c) releases
//! without touching the failure count.
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
/// `GateResult::ExecutionError` instead of a verdict: the gate cannot run at
/// all, and the caller releases without incrementing the failure count (see
/// `outcome::handle_gate_error`).
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
            return Ok(Some(GateResult::ExecutionError {
                command: format!("git {}", UPSTREAM_PROBE_ARGS.join(" ")),
                reason: format!(
                    "no upstream configured for branch '{}': the shipped-work gate cannot \
                         verify that the commit was pushed, so this closure is not counted as a \
                         failure. Remedy: `git push -u <remote> {}` — that publishes the branch \
                         and sets its upstream in one step. If no remote exists yet, add one \
                         first: `git remote add origin <url>` (adding a remote that already \
                         exists is an error, so check `git remote -v` first).",
                    branch, branch
                ),
            }));
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
    use crate::validation::predispatch::PreDispatch;
    use tempfile::TempDir;

    fn snapshot(head: Option<&str>, notes: Option<&str>) -> PreDispatch {
        PreDispatch {
            head_sha: head.map(|s| s.to_string()),
            notes_hash: notes.map(hash_notes),
            dirty_files: Vec::new(),
            captured_at: None,
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

    /// With no snapshot and no note — nothing the gate can judge at all — the
    /// closure fails so the failure-quarantine circuit stays reachable: the
    /// bead is reopened, released, and the failure count is incremented.
    /// After `outcome.quarantine_after_failures` consecutive failures, the
    /// bead is quarantined (set deferred) and stops retrying. This bounded
    /// loop prevents the unbounded retry loop described in GitHub issue #16
    /// (bead needle-0fbf5145 posted 18 identical GitHub comments because
    /// every closure was accepted).
    #[tokio::test]
    async fn no_snapshot_fails_closed() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        match evaluate(dir.path(), None, "", false).await.unwrap() {
            GateResult::Fail(reason) => {
                assert!(
                    reason.contains("no pre-dispatch snapshot recorded"),
                    "failure reason should mention missing snapshot"
                );
            }
            GateResult::Pass => {
                panic!("no snapshot must fail closed to prevent unbounded retry loops");
            }
            GateResult::ExecutionError { .. } => {
                panic!("no snapshot should return Fail, not ExecutionError");
            }
        }
    }

    /// The remedy the no-snapshot failure text has always printed must
    /// actually work: with no baseline the gate still judges the bead note,
    /// passing when the agent recorded one. Before needle-e4fbe47c this arm
    /// was unreachable — the missing-snapshot failure preempted every note
    /// check, so closures bounced on that advice forever (five closes on
    /// bead beadrs-5d781dc7 until an operator intervened).
    #[tokio::test]
    async fn no_snapshot_with_a_bead_note_passes_on_the_note_arm() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        assert_eq!(
            evaluate(
                dir.path(),
                None,
                "verified: work already shipped at a4f8fbe, closing",
                false
            )
            .await
            .unwrap(),
            GateResult::Pass
        );
    }

    /// A `deliverable:external` bead with no snapshot is judged the same way
    /// as one with a snapshot whose notes were unreadable: the evidence note
    /// is checkable without any baseline.
    #[tokio::test]
    async fn no_snapshot_external_with_evidence_note_passes() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        assert_eq!(
            evaluate(
                dir.path(),
                None,
                "evidence: https://example.com/proof",
                true
            )
            .await
            .unwrap(),
            GateResult::Pass
        );
    }

    /// External bead, no snapshot, and no note: fail on the missing note, not
    /// on the missing snapshot — the message should point at the remedy.
    #[tokio::test]
    async fn no_snapshot_external_without_note_fails_on_the_note() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        match evaluate(dir.path(), None, "", true).await.unwrap() {
            GateResult::Fail(reason) => {
                assert!(
                    reason.contains("without note update"),
                    "failure should name the note remedy, not the snapshot: {reason}"
                );
            }
            GateResult::Pass => panic!("external bead with no evidence must fail"),
            GateResult::ExecutionError { .. } => panic!("expected Fail, not ExecutionError"),
        }
    }

    #[tokio::test]
    async fn snapshot_without_readable_notes_accepts_a_non_empty_note() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let snap = snapshot(Some(&head), None);
        assert_eq!(
            evaluate(
                dir.path(),
                Some(&snap),
                "already implemented in 4f2a1c",
                false
            )
            .await
            .unwrap(),
            GateResult::Pass
        );
    }

    #[tokio::test]
    async fn snapshot_without_readable_notes_and_no_note_fails() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let snap = snapshot(Some(&head), None);
        assert!(matches!(
            evaluate(dir.path(), Some(&snap), "   ", false)
                .await
                .unwrap(),
            GateResult::Fail(_)
        ));
    }

    #[tokio::test]
    async fn notes_changed_during_dispatch_passes_without_a_commit() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let snap = snapshot(Some(&head), Some(""));
        let result = evaluate(
            dir.path(),
            Some(&snap),
            "checked: no code change needed",
            false,
        )
        .await
        .unwrap();
        assert_eq!(result, GateResult::Pass);
    }

    /// The hole this gate was written for and did not close: an agent that
    /// closes a bead having shipped nothing. Bead close bumps `updated_at`, so
    /// the old timestamp fallback passed here unconditionally.
    #[tokio::test]
    async fn closing_without_shipping_or_noting_anything_fails() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let snap = snapshot(Some(&head), Some("pre-existing note"));
        let result = evaluate(dir.path(), Some(&snap), "pre-existing note", false)
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
            evaluate(dir.path(), Some(&snap), "", false).await.unwrap(),
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
        match evaluate(dir.path(), Some(&snap), "", false).await.unwrap() {
            GateResult::Fail(reason) => {
                assert!(reason.contains("not been pushed"));
                // The message names which upstream it was not pushed to.
                assert!(
                    reason.contains("origin/"),
                    "failure reason should name the upstream ref: {reason}"
                );
            }
            GateResult::Pass => panic!("expected Fail for unpushed commit"),
            GateResult::ExecutionError { .. } => {
                panic!("expected Pass or Fail, got ExecutionError")
            }
        }
    }

    /// GitHub issue #18: a workspace with no remote at all (plain `git init`)
    /// has no upstream, so `@{u}` does not resolve. That is a gate that cannot
    /// run — `ExecutionError`, released without a failure increment — not a
    /// `Fail` claiming the commit "has not been pushed".
    #[tokio::test]
    async fn no_upstream_reports_a_gate_that_cannot_run() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await; // no remote, no upstream
        commit_files(dir.path(), &[("src.rs", "fn main() {}\n")], "real work");

        let snap = snapshot(Some(&head), Some(""));
        match evaluate(dir.path(), Some(&snap), "", false).await.unwrap() {
            GateResult::ExecutionError { command, reason } => {
                assert!(
                    command.contains("@{u}"),
                    "the probe that could not run should be named: {command}"
                );
                assert!(
                    reason.contains("no upstream configured"),
                    "reason should name the missing upstream: {reason}"
                );
                assert!(
                    reason.contains("git remote add") && reason.contains("git push -u"),
                    "reason should name the remedy: {reason}"
                );
                assert!(
                    !reason.contains("has not been pushed"),
                    "the missing-upstream verdict must not read as an unpushed commit: {reason}"
                );
            }
            GateResult::Fail(reason) => {
                panic!(
                    "no-upstream workspace must not fail the gate (got: {reason}) — \
                     an unverifiable push is not unshipped work"
                );
            }
            GateResult::Pass => panic!("a gate that cannot run must not pass"),
        }
    }

    /// A remote that was added but never pushed to (`git remote add` without
    /// `push -u`) leaves the branch without an upstream too — same verdict as
    /// having no remote at all.
    #[tokio::test]
    async fn remote_without_upstream_branch_also_reports_a_gate_that_cannot_run() {
        let dir = TempDir::new().unwrap();
        let bare = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        git(bare.path(), &["init", "-q", "--bare"]);
        git(
            dir.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        commit_files(dir.path(), &[("src.rs", "fn main() {}\n")], "real work");

        let branch = git_output(dir.path(), &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap();
        let snap = snapshot(Some(&head), Some(""));
        match evaluate(dir.path(), Some(&snap), "", false).await.unwrap() {
            GateResult::ExecutionError { reason, .. } => {
                assert!(
                    reason.contains("no upstream configured"),
                    "reason should name the missing upstream: {reason}"
                );
                // The message names the branch it could not check, so the
                // remedy can be copied verbatim.
                assert!(
                    reason.contains(&format!("branch '{branch}'")),
                    "reason should name the branch: {reason}"
                );
            }
            GateResult::Fail(reason) => {
                panic!("an unconfigured upstream must not read as an unpushed commit: {reason}")
            }
            GateResult::Pass => panic!("a gate that cannot run must not pass"),
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
        match evaluate(dir.path(), Some(&snap), "", false).await.unwrap() {
            GateResult::Fail(_) => {}
            GateResult::Pass => panic!("a notes-only commit must not satisfy the gate on its own"),
            GateResult::ExecutionError { .. } => {
                panic!("expected Pass or Fail, got ExecutionError")
            }
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
        match evaluate(dir.path(), Some(&snap), "", false).await.unwrap() {
            GateResult::Fail(_) => {}
            GateResult::Pass => {
                panic!("the marker file must not make a notes-only commit substantial")
            }
            GateResult::ExecutionError { .. } => {
                panic!("expected Pass or Fail, got ExecutionError")
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
            evaluate(dir.path(), Some(&snap), "", false).await.unwrap(),
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
            evaluate(dir.path(), Some(&snap), "investigated", false)
                .await
                .unwrap(),
            GateResult::Pass
        );
    }

    // ── upstream predicate ──

    /// The predicate answers the upstream question on its own, so the doctor
    /// readiness check (needle-4fcd150c) and the gate read the same probe
    /// rather than two implementations that can drift apart.
    #[tokio::test]
    async fn upstream_predicate_reports_a_configured_upstream() {
        let dir = TempDir::new().unwrap();
        let bare = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        push_upstream(dir.path(), bare.path()).await;

        let branch = git_output(dir.path(), &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap();
        assert_eq!(
            upstream_status(dir.path()).await,
            UpstreamStatus::Present(format!("origin/{branch}"))
        );
    }

    #[tokio::test]
    async fn upstream_predicate_reports_a_fresh_init_as_not_configured() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await; // no remote, no upstream
        assert_eq!(
            upstream_status(dir.path()).await,
            UpstreamStatus::NotConfigured
        );
    }

    /// `git remote add` alone leaves the branch upstreamless — same answer as
    /// having no remote at all, not an error.
    #[tokio::test]
    async fn upstream_predicate_reports_an_unpushed_remote_as_not_configured() {
        let dir = TempDir::new().unwrap();
        let bare = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        git(bare.path(), &["init", "-q", "--bare"]);
        git(
            dir.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        assert_eq!(
            upstream_status(dir.path()).await,
            UpstreamStatus::NotConfigured
        );
    }

    /// Outside a repository git cannot answer the question at all — a distinct
    /// state from "the branch has no upstream", and not to be misreported as
    /// one.
    #[tokio::test]
    async fn upstream_predicate_reports_a_git_error_outside_a_repository() {
        let dir = TempDir::new().unwrap(); // not a git repo
        match upstream_status(dir.path()).await {
            UpstreamStatus::GitError(stderr) => {
                assert!(
                    stderr.contains("not a git repository"),
                    "stderr should carry git's real cause: {stderr}"
                );
            }
            UpstreamStatus::NotConfigured => {
                panic!("an unusable workspace is not the same as a branch without an upstream")
            }
            UpstreamStatus::Present(upstream) => panic!(
                "nothing can be an upstream for a directory outside any repository: {upstream}"
            ),
        }
    }

    /// Upstream configured but unresolvable (detached HEAD): the gate still
    /// cannot run, and the cause reported is git's own — not "no upstream
    /// configured", which would send an operator down the wrong remedy.
    #[tokio::test]
    async fn detached_head_reports_gits_cause_rather_than_no_upstream() {
        let dir = TempDir::new().unwrap();
        let bare = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        push_upstream(dir.path(), bare.path()).await;
        git(dir.path(), &["checkout", "-q", "--detach", "HEAD"]);
        commit_files(dir.path(), &[("src.rs", "fn main() {}\n")], "real work");

        let snap = snapshot(Some(&head), Some(""));
        match evaluate(dir.path(), Some(&snap), "", false).await.unwrap() {
            GateResult::ExecutionError { reason, .. } => {
                assert!(
                    reason.contains("could not resolve an upstream"),
                    "the unresolvable-upstream cause should be named: {reason}"
                );
                assert!(
                    reason.contains("HEAD does not point to a branch"),
                    "git's own stderr should be carried through: {reason}"
                );
                assert!(
                    !reason.contains("no upstream configured"),
                    "a detached HEAD is not a missing-upstream config: {reason}"
                );
            }
            GateResult::Fail(reason) => {
                panic!("the gate cannot run here, so this is not a Fail: {reason}")
            }
            GateResult::Pass => panic!("a gate that cannot run must not pass"),
        }
    }

    // ── deliverable:external tests ──

    /// Labeled bead with evidence line and changed notes passes.
    #[tokio::test]
    async fn external_labeled_with_evidence_and_changed_notes_passes() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let snap = snapshot(Some(&head), Some("previous note"));
        assert_eq!(
            evaluate(
                dir.path(),
                Some(&snap),
                "evidence: https://github.com/example/repo/issues/16#comment-123",
                true
            )
            .await
            .unwrap(),
            GateResult::Pass
        );
    }

    /// Labeled bead with changed notes but no evidence: line fails.
    #[tokio::test]
    async fn external_labeled_without_evidence_line_fails() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let snap = snapshot(Some(&head), Some("previous note"));
        match evaluate(
            dir.path(),
            Some(&snap),
            "posted comment to GitHub issue #16",
            true,
        )
        .await
        .unwrap()
        {
            GateResult::Fail(reason) => {
                assert!(reason.contains("without evidence"));
            }
            GateResult::Pass => panic!("expected Fail for missing evidence: line"),
            GateResult::ExecutionError { .. } => {
                panic!("expected Pass or Fail, got ExecutionError")
            }
        }
    }

    /// Labeled bead with a real commit but no evidence: line fails.
    #[tokio::test]
    async fn external_labeled_with_commit_but_no_evidence_fails() {
        let dir = TempDir::new().unwrap();
        let bare = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        push_upstream(dir.path(), bare.path()).await;
        commit_files(dir.path(), &[("src.rs", "fn main() {}\n")], "real work");
        git(dir.path(), &["push", "-q"]);

        let snap = snapshot(Some(&head), Some(""));
        match evaluate(dir.path(), Some(&snap), "implemented the feature", true)
            .await
            .unwrap()
        {
            GateResult::Fail(reason) => {
                assert!(reason.contains("without evidence"));
            }
            GateResult::Pass => {
                panic!("expected Fail for commit without evidence: line in notes")
            }
            GateResult::ExecutionError { .. } => {
                panic!("expected Pass or Fail, got ExecutionError")
            }
        }
    }

    /// Labeled bead with snapshot hash None (notes unreadable at dispatch) and
    /// evidence: line passes.
    #[tokio::test]
    async fn external_labeled_with_no_snapshot_hash_and_evidence_passes() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let snap = snapshot(Some(&head), None); // notes_hash = None
        assert_eq!(
            evaluate(
                dir.path(),
                Some(&snap),
                "evidence: provisioning succeeded, ID: prov-abc123",
                true
            )
            .await
            .unwrap(),
            GateResult::Pass
        );
    }

    /// Labeled bead without note update fails.
    #[tokio::test]
    async fn external_labeled_without_note_change_fails() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let pre_notes = "evidence: https://example.com/provisioning/prov-abc123";
        let snap = snapshot(Some(&head), Some(pre_notes));
        match evaluate(dir.path(), Some(&snap), pre_notes, true)
            .await
            .unwrap()
        {
            GateResult::Fail(reason) => {
                assert!(reason.contains("without note update"));
            }
            GateResult::Pass => panic!("expected Fail when notes didn't change"),
            GateResult::ExecutionError { .. } => {
                panic!("expected Pass or Fail, got ExecutionError")
            }
        }
    }

    /// Unlabeled beads work exactly as before - git check still applies.
    #[tokio::test]
    async fn unlabeled_bead_git_check_still_applies() {
        let dir = TempDir::new().unwrap();
        let bare = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        push_upstream(dir.path(), bare.path()).await;
        commit_files(dir.path(), &[("src.rs", "fn main() {}\n")], "real work");
        git(dir.path(), &["push", "-q"]);

        let snap = snapshot(Some(&head), Some(""));
        // Unlabeled bead should still pass via git commit
        assert_eq!(
            evaluate(dir.path(), Some(&snap), "", false).await.unwrap(),
            GateResult::Pass
        );
    }

    /// Evidence: line is case-insensitive on "evidence" keyword.
    #[tokio::test]
    async fn external_evidence_keyword_case_insensitive() {
        let dir = TempDir::new().unwrap();
        let head = init_repo(dir.path()).await;
        let snap = snapshot(Some(&head), Some("previous note"));

        // Test various case combinations
        for notes in &[
            "EVIDENCE: https://example.com",
            "Evidence: https://example.com",
            "eViDeNcE: https://example.com",
            "evidence: https://example.com",
        ] {
            assert_eq!(
                evaluate(dir.path(), Some(&snap), notes, true)
                    .await
                    .unwrap(),
                GateResult::Pass,
                "case variation should pass: {}",
                notes
            );
        }
    }
}
