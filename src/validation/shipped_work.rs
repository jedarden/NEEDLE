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
//! # Shared checkouts: judge the dispatch's commits, not the shared HEAD
//!
//! Several workers commit in one checkout, so `HEAD` at judging time is often
//! a concurrent worker's commit made seconds earlier and not yet pushed. The
//! gate used to fail every such closure on the ancestry of that foreign tip:
//! 9 of the 14 `gate:shipped_work` failures sampled on 2026-10-16/17 named
//! another bead's commit, and 5 of those dispatches had already pushed their
//! own work (needle-9798bd7e). When `HEAD` is not on the upstream, each
//! commit the dispatch's range holds that the upstream lacks is classified:
//!
//! - **landed under a new SHA** — its `git patch-id --stable` over substantial
//!   paths matches one of the newest [`UPSTREAM_PATCH_WINDOW`] upstream
//!   commits (a concurrent `git pull --rebase` or an amend rewrote it);
//! - **another bead's** — its conventional-commit scope names a different
//!   bead of this workspace and the message never names this one;
//! - **trivial** — it touches only [`TRIVIAL_PATH_PREFIXES`];
//! - otherwise **unshipped**, which fails the closure naming that commit.
//!
//! With nothing unshipped, the verdict rests on this dispatch's own
//! substantial commits (another bead's commits are not credited):
//! `GateResult::Pass` when they are on the upstream as made,
//! `GateResult::PassRewritten` when one counted only by patch-equivalence,
//! and the note fallback when there are none. Every ambiguity resolves
//! toward the strict verdict: an unattributable commit is judged as this
//! dispatch's, and a classification that cannot run reports the plain
//! "has not been pushed" failure.
//!
//! Depends on: `types`, `validation::predispatch`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;

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

/// How many of the upstream's newest commits (those `HEAD` lacks) are searched
/// for a patch-equivalent twin of an unpushed commit. A twin lands after the
/// dispatch's own commit, so it is among the newest; the bound keeps a stale
/// checkout from hashing the upstream's whole history.
const UPSTREAM_PATCH_WINDOW: usize = 200;

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
        &post.id,
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
    bead_id: &str,
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

    if let Some(result) = check_commit(workspace, bead_id, Some(snapshot)).await? {
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
/// check the fallback" (no snapshot, no new commit, only trivial paths
/// changed, or nothing substantial of the dispatch's own beneath a concurrent
/// worker's unpushed commit) rather than a hard pass/fail.
///
/// A workspace with no upstream configured returns
/// `GateResult::Unsatisfiable` instead of an ordinary failure: every commit
/// would be judged the same way until the repository gets an upstream or
/// shipped-work enforcement is disabled.
///
/// `bead_id` attributes commits in a shared checkout; see the module docs.
async fn check_commit(
    workspace: &Path,
    bead_id: &str,
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

    let changed = git_output(
        workspace,
        &[
            "diff",
            "--name-only",
            &pre_sha,
            &head,
            "--",
            ".",
            crate::file_contention::git_safety::EXCLUDE_PATHSPEC,
        ],
    )
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
        return Ok(Some(GateResult::Pass));
    }

    // HEAD is not on the upstream. In a shared checkout that is often a
    // concurrent worker's fresh commit, or this dispatch's own commit already
    // landed under a new SHA, so judge the dispatch's commits one by one.
    match judge_unpushed_range(workspace, bead_id, &pre_sha, &head, &upstream).await {
        Ok(verdict) => Ok(verdict),
        Err(e) => {
            tracing::warn!(
                workspace = %workspace.display(),
                bead_id,
                error = %e,
                "shipped-work gate could not classify the unpushed commits — judging HEAD alone"
            );
            Ok(Some(unpushed_failure(&head, &upstream)))
        }
    }
}

/// The failure for substantial work that is not on the upstream in any form.
/// Every unpushed verdict keeps this one wording, so the verification
/// fingerprint (which normalizes away the SHA) still groups them as a single
/// failure shape.
fn unpushed_failure(sha: &str, upstream: &str) -> GateResult {
    GateResult::Fail(format!(
        "commit {} has substantial changes but has not been pushed to its upstream {}",
        short_sha(sha),
        upstream
    ))
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// Where one commit of the dispatch's range stands relative to the upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Landing {
    /// Reachable from the upstream: pushed as made.
    Pushed,
    /// Not reachable, but the upstream holds a commit with the same
    /// `git patch-id --stable` over substantial paths; the field is that
    /// upstream commit.
    Rewritten(String),
    /// On the upstream in no form.
    Unshipped,
}

/// One commit in `pre_sha..HEAD`, classified for the shipped-work verdict.
#[derive(Debug, Clone)]
struct RangeCommit {
    sha: String,
    /// Attributed to another bead of this workspace ([`names_other_bead`]).
    foreign: bool,
    /// A merge. Never credited as the dispatch's work, and never matched by
    /// patch id; when unpushed it is attributed by its message like any other
    /// commit, so a plain `Merge ...` subject blocks.
    merge: bool,
    /// Touches a path outside [`TRIVIAL_PATH_PREFIXES`]. Always true for a
    /// merge, whose changed paths `git log --name-only` does not list.
    substantial: bool,
    landing: Landing,
}

impl RangeCommit {
    /// Content this dispatch has not shipped: its own (or unattributable)
    /// substantial commit that the upstream holds in no form.
    fn blocks(&self) -> bool {
        self.landing == Landing::Unshipped && !self.foreign && self.substantial
    }

    /// Substantial work this dispatch can be credited with.
    fn own_work(&self) -> bool {
        !self.foreign && !self.merge && self.substantial
    }
}

/// Verdict for a range whose HEAD is not on the upstream: fail on the newest
/// commit the dispatch has not shipped; otherwise credit only the dispatch's
/// own substantial commits — on the upstream as made (`Pass`) or only by
/// patch-equivalence (`PassRewritten`) — and defer to the note fallback
/// (`None`) when it has none. Commits of other beads are neither blockers nor
/// credit.
async fn judge_unpushed_range(
    workspace: &Path,
    bead_id: &str,
    pre_sha: &str,
    head: &str,
    upstream: &str,
) -> Result<Option<GateResult>> {
    let upstream_sha = git_output(workspace, &["rev-parse", "--verify", "--quiet", upstream])
        .await
        .with_context(|| format!("resolve upstream {upstream} to a commit"))?;
    let commits = classify_range(workspace, bead_id, pre_sha, head, &upstream_sha).await?;
    Ok(range_verdict(bead_id, head, upstream, &commits))
}

/// The pure decision over an already classified range (newest first).
fn range_verdict(
    bead_id: &str,
    head: &str,
    upstream: &str,
    commits: &[RangeCommit],
) -> Option<GateResult> {
    let foreign_unpushed: Vec<&str> = commits
        .iter()
        .filter(|c| c.foreign && c.landing != Landing::Pushed)
        .map(|c| short_sha(&c.sha))
        .collect();

    if let Some(blocker) = commits.iter().find(|c| c.blocks()) {
        tracing::info!(
            bead_id,
            head = short_sha(head),
            commit = short_sha(&blocker.sha),
            other_beads_unpushed = ?foreign_unpushed,
            "shipped-work gate: this dispatch's commit is not on the upstream in any form"
        );
        return Some(unpushed_failure(&blocker.sha, upstream));
    }

    let own: Vec<&RangeCommit> = commits.iter().filter(|c| c.own_work()).collect();
    if own.is_empty() {
        tracing::info!(
            bead_id,
            head = short_sha(head),
            other_beads_unpushed = ?foreign_unpushed,
            "shipped-work gate: the unpushed commits belong to other beads or are trivial, \
             and this dispatch committed nothing substantial of its own — judging its note"
        );
        return None;
    }

    let rewritten: Vec<String> = own
        .iter()
        .filter_map(|c| match &c.landing {
            Landing::Rewritten(twin) => Some(format!(
                "{} landed as {}",
                short_sha(&c.sha),
                short_sha(twin)
            )),
            Landing::Pushed | Landing::Unshipped => None,
        })
        .collect();
    if rewritten.is_empty() {
        tracing::info!(
            bead_id,
            head = short_sha(head),
            other_beads_unpushed = ?foreign_unpushed,
            "shipped-work gate: this dispatch's commits are on the upstream; the unpushed \
             HEAD is other beads' or trivial commits"
        );
        return Some(GateResult::Pass);
    }
    Some(GateResult::PassRewritten(format!(
        "HEAD {} is not on {}, but this dispatch's work is, under rewritten commits \
         (identical git patch-id --stable): {}",
        short_sha(head),
        upstream,
        rewritten.join(", ")
    )))
}

/// Classify every commit in `pre_sha..head`, newest first.
async fn classify_range(
    workspace: &Path,
    bead_id: &str,
    pre_sha: &str,
    head: &str,
    upstream_sha: &str,
) -> Result<Vec<RangeCommit>> {
    let not_pre = format!("^{pre_sha}");
    let not_upstream = format!("^{upstream_sha}");
    let unpushed: HashSet<String> =
        git_output(workspace, &["rev-list", head, &not_pre, &not_upstream])
            .await?
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

    // One record per commit: \x1e sha \x1f parents \x1f message \x1f names.
    let log = git_output(
        workspace,
        &[
            "-c",
            "log.showSignature=false",
            "log",
            "--no-color",
            "--no-renames",
            "--name-only",
            "--format=%x1e%H%x1f%P%x1f%B%x1f",
            head,
            &not_pre,
        ],
    )
    .await?;

    let mut commits = Vec::new();
    for record in log.split('\x1e').filter(|r| !r.trim().is_empty()) {
        let mut fields = record.splitn(4, '\x1f');
        let (Some(sha), Some(parents), Some(message)) =
            (fields.next(), fields.next(), fields.next())
        else {
            anyhow::bail!("unparseable git log record in the dispatch range");
        };
        let sha = sha.trim().to_string();
        let names = fields.next().unwrap_or("");
        let merge = parents.split_whitespace().count() > 1;
        let substantial = merge
            || names
                .lines()
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .filter(|f| !crate::file_contention::git_safety::is_marker_path(f))
                .any(|f| !TRIVIAL_PATH_PREFIXES.iter().any(|p| f.starts_with(p)));
        let landing = if unpushed.contains(&sha) {
            Landing::Unshipped
        } else {
            Landing::Pushed
        };
        commits.push(RangeCommit {
            foreign: names_other_bead(message, bead_id),
            merge,
            substantial,
            landing,
            sha,
        });
    }

    // Only commits that would otherwise block are worth hashing: this
    // dispatch's own (or unattributable) substantial non-merge commits.
    let candidates: Vec<String> = commits
        .iter()
        .filter(|c| c.blocks() && !c.merge)
        .map(|c| c.sha.clone())
        .collect();
    if candidates.is_empty() {
        return Ok(commits);
    }

    let mut candidate_args: Vec<String> = vec!["--no-walk=unsorted".to_string()];
    candidate_args.extend(candidates.iter().cloned());
    let candidate_ids = patch_ids(workspace, &candidate_args).await?;

    let window_args = vec![
        format!("--max-count={UPSTREAM_PATCH_WINDOW}"),
        upstream_sha.to_string(),
        format!("^{head}"),
    ];
    let landed: HashMap<String, String> = patch_ids(workspace, &window_args)
        .await?
        .into_iter()
        .map(|(sha, id)| (id, sha))
        .collect();

    for commit in commits.iter_mut() {
        if !candidates.contains(&commit.sha) {
            continue;
        }
        if let Some(twin) = candidate_ids.get(&commit.sha).and_then(|id| landed.get(id)) {
            commit.landing = Landing::Rewritten(twin.clone());
        }
    }
    Ok(commits)
}

/// `commit sha -> git patch-id --stable` for the commits `revs` selects,
/// hashing only substantial paths so a rewrite that folded in (or dropped) a
/// `.beads/` checkpoint still matches. Merges and commits without a
/// substantial diff produce no id.
async fn patch_ids(workspace: &Path, revs: &[String]) -> Result<HashMap<String, String>> {
    let mut args: Vec<String> = [
        "-c",
        "log.showSignature=false",
        "log",
        "-p",
        "--no-merges",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--format=commit %H",
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();
    args.extend(revs.iter().cloned());
    args.push("--".to_string());
    args.push(".".to_string());
    args.extend(
        TRIVIAL_PATH_PREFIXES
            .iter()
            .map(|p| format!(":(exclude){p}")),
    );
    args.push(crate::file_contention::git_safety::EXCLUDE_PATHSPEC.to_string());

    let log = tokio::process::Command::new("git")
        .args(&args)
        .current_dir(workspace)
        .kill_on_drop(true)
        .output()
        .await
        .context("run git log -p for patch ids")?;
    if !log.status.success() {
        anyhow::bail!(
            "git log -p for patch ids failed: {}",
            String::from_utf8_lossy(&log.stderr)
        );
    }

    let mut child = tokio::process::Command::new("git")
        .args(["patch-id", "--stable"])
        .current_dir(workspace)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawn git patch-id")?;
    let mut stdin = child
        .stdin
        .take()
        .context("git patch-id stdin was not captured")?;
    let patch = log.stdout;
    let writer = tokio::spawn(async move {
        let written = stdin.write_all(&patch).await;
        drop(stdin);
        written
    });
    let output = child
        .wait_with_output()
        .await
        .context("wait for git patch-id")?;
    writer
        .await
        .context("join the git patch-id writer")?
        .context("write the patch to git patch-id")?;
    if !output.status.success() {
        anyhow::bail!(
            "git patch-id failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Each line is `<patch-id> <commit>`.
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (id, sha) = line.trim().split_once(' ')?;
            Some((sha.trim().to_string(), id.to_string()))
        })
        .collect())
}

/// Whether a commit message attributes its commit to a bead other than
/// `bead_id`: the subject's conventional-commit scope (`fix(<id>): ...`,
/// the fleet's commit convention) names a bead id with this bead's prefix
/// that is not `bead_id`, and the message never mentions `bead_id`.
///
/// Anything less certain — no scope, a component scope such as
/// `fix(needle-ci)`, a different prefix, a revert of another bead's commit —
/// is not foreign, so the commit is judged as this dispatch's own: the strict
/// reading the gate applied to every commit before.
fn names_other_bead(message: &str, bead_id: &str) -> bool {
    if bead_id.is_empty() || message.contains(bead_id) {
        return false;
    }
    let Some((prefix, _)) = bead_id.rsplit_once('-') else {
        return false;
    };
    let subject = message.lines().next().unwrap_or("").trim();
    let Some(scope) = conventional_scope(subject) else {
        return false;
    };
    scope
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .any(|token| token != bead_id && is_bead_id_with_prefix(token, prefix))
}

/// The scope of a conventional-commit subject: `type(scope): ...` or
/// `type(scope)!: ...`. `None` for any other subject shape.
fn conventional_scope(subject: &str) -> Option<&str> {
    let open = subject.find('(')?;
    let kind = &subject[..open];
    if kind.is_empty()
        || !kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let close = open + subject[open..].find(')')?;
    let rest = &subject[close + 1..];
    let rest = rest.strip_prefix('!').unwrap_or(rest);
    if !rest.starts_with(':') {
        return None;
    }
    Some(&subject[open + 1..close])
}

/// `<prefix>-<suffix>` where the suffix looks like a bead id rather than a
/// word: lowercase alphanumerics, at least three of them, at least one digit
/// (`twill-ab007a11`, `needle-49vt`; not `needle-ci` or `twill-docs`).
fn is_bead_id_with_prefix(token: &str, prefix: &str) -> bool {
    let Some(suffix) = token
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    suffix.len() >= 3
        && suffix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && suffix.chars().any(|c| c.is_ascii_digit())
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

    /// The bead every fixture dispatch works; commits naming it are its own.
    const BEAD: &str = "needle-1a2b3c4d";
    /// Another bead of the same workspace, as a concurrent worker commits it.
    const OTHER_BEAD: &str = "needle-9f8e7d6c";

    async fn verdict(repo: &TempDir, snapshot: &predispatch::PreDispatch) -> GateResult {
        check_commit(repo.path(), BEAD, Some(snapshot))
            .await
            .expect("shipped-work check should run")
            .expect("substantial committed work should produce a verdict")
    }

    fn head(repo: &TempDir) -> String {
        git_stdout(repo.path(), &["rev-parse", "HEAD"])
    }

    fn branch(repo: &TempDir) -> String {
        git_stdout(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"])
    }

    fn commit_file(repo: &TempDir, path: &str, content: &str, message: &str) -> String {
        let full = repo.path().join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create fixture directory");
        }
        fs::write(&full, content).expect("write fixture file");
        git_ok(repo.path(), &["add", path]);
        git_ok(repo.path(), &["commit", "-q", "-m", message]);
        head(repo)
    }

    /// Run `land` on a detached checkout of the upstream tip, push what it
    /// committed as the upstream branch, and return to the local branch —
    /// the shape of a concurrent worker landing commits while this
    /// dispatch's local branch stays where it was.
    fn land_upstream(repo: &TempDir, land: impl FnOnce(&TempDir)) -> String {
        let branch = branch(repo);
        let local = head(repo);
        git_ok(
            repo.path(),
            &["checkout", "-q", "--detach", &format!("origin/{branch}")],
        );
        land(repo);
        let landed = head(repo);
        git_ok(
            repo.path(),
            &["push", "-q", "origin", &format!("HEAD:{branch}")],
        );
        git_ok(repo.path(), &["checkout", "-q", &branch]);
        assert_eq!(head(repo), local, "the local branch must not move");
        landed
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

    // ── needle-9798bd7e: rewritten commits and shared-checkout tips ──

    /// (a) The dispatch's commit was rebased to a new SHA by a concurrent
    /// `git pull --rebase` and landed with an identical patch: shipped.
    #[tokio::test]
    async fn rebased_commit_with_identical_patch_passes_as_rewritten() {
        let repo = git_repo();
        let _remote = bare_upstream(&repo);
        let before = head(&repo);
        let orphaned = commit_file(&repo, "work.rs", "fn work() {}\n", "ship work");
        let landed = land_upstream(&repo, |repo| {
            commit_file(repo, "other.rs", "fn other() {}\n", "another worker");
            git_ok(repo.path(), &["cherry-pick", &orphaned]);
        });
        assert_ne!(landed, orphaned);

        match verdict(&repo, &snapshot_at(before)).await {
            GateResult::PassRewritten(detail) => {
                assert!(detail.contains(&orphaned[..7]), "{detail}");
                assert!(detail.contains(&landed[..7]), "{detail}");
                assert!(detail.contains("patch-id"), "{detail}");
            }
            other => panic!("expected PassRewritten, got {other:?}"),
        }
    }

    /// A rewrite that folded a `.beads/` checkpoint into the commit (seen in
    /// the field: ibkr-mcp a53f515 -> 1845f7b) still matches, because the
    /// patch id covers substantial paths only.
    #[tokio::test]
    async fn rewritten_commit_matches_when_the_rewrite_folded_in_bead_state() {
        let repo = git_repo();
        let _remote = bare_upstream(&repo);
        let before = head(&repo);
        let orphaned = commit_file(&repo, "work.rs", "fn work() {}\n", "ship work");
        land_upstream(&repo, |repo| {
            git_ok(repo.path(), &["cherry-pick", "--no-commit", &orphaned]);
            fs::create_dir_all(repo.path().join(".beads")).expect("create .beads");
            fs::write(repo.path().join(".beads/checkpoint.json"), "{}\n")
                .expect("write checkpoint");
            git_ok(repo.path(), &["add", ".beads/checkpoint.json"]);
            git_ok(repo.path(), &["commit", "-q", "-m", "ship work"]);
        });

        let result = verdict(&repo, &snapshot_at(before)).await;
        assert!(
            matches!(result, GateResult::PassRewritten(_)),
            "got {result:?}"
        );
    }

    /// (b) Content that never reached the upstream still fails, even when
    /// the upstream moved on with a different change to the same file.
    #[tokio::test]
    async fn commit_whose_content_never_landed_still_fails() {
        let repo = git_repo();
        let _remote = bare_upstream(&repo);
        let before = head(&repo);
        let own = commit_file(&repo, "work.rs", "fn work() {}\n", "ship work");
        land_upstream(&repo, |repo| {
            commit_file(
                repo,
                "work.rs",
                "fn work() { /* a different change */ }\n",
                "another worker",
            );
        });

        let result = verdict(&repo, &snapshot_at(before)).await;
        match &result {
            GateResult::Fail(reason) => {
                assert!(reason.contains("has not been pushed"), "{reason}");
                assert!(reason.contains(&own[..7]), "{reason}");
            }
            other => panic!("expected Fail, got {other:?}"),
        }
        assert!(result.failure_reason().is_some());
    }

    /// (c) No new commit at all — nothing for `git cherry` to compare — is
    /// unchanged: no git verdict, and without a note the closure fails on
    /// the "no substantial pushed commit" reason, not an unpushed one.
    #[tokio::test]
    async fn no_new_commit_is_unchanged_by_the_rewrite_check() {
        let repo = git_repo();
        let _remote = bare_upstream(&repo);
        // HEAD carries unpushed work from before the dispatch started.
        commit_file(&repo, "work.rs", "fn work() {}\n", "ship work");
        let before = head(&repo);
        let snapshot = snapshot_at(before);

        let git_verdict = check_commit(repo.path(), BEAD, Some(&snapshot))
            .await
            .expect("shipped-work check should run");
        assert!(git_verdict.is_none(), "got {git_verdict:?}");

        let result = evaluate(repo.path(), BEAD, Some(&snapshot), "", false)
            .await
            .expect("evaluate should run");
        assert!(
            matches!(result, GateResult::Fail(ref reason)
                if reason.contains("no substantial pushed commit")),
            "got {result:?}"
        );
    }

    /// The dominant field cause (2026-10-16/17): the shared checkout's HEAD
    /// is another bead's commit, not yet pushed, while this dispatch's own
    /// work is already on the upstream. That is a plain pass.
    #[tokio::test]
    async fn another_beads_unpushed_tip_does_not_fail_pushed_own_work() {
        let repo = git_repo();
        let _remote = bare_upstream(&repo);
        let before = head(&repo);
        commit_file(
            &repo,
            "work.rs",
            "fn work() {}\n",
            &format!("fix({BEAD}): own work"),
        );
        git_ok(repo.path(), &["push", "-q"]);
        commit_file(
            &repo,
            "other.rs",
            "fn other() {}\n",
            &format!("feat({OTHER_BEAD}): a concurrent worker's commit"),
        );

        assert_eq!(verdict(&repo, &snapshot_at(before)).await, GateResult::Pass);
    }

    /// A dispatch that committed nothing of its own gets no credit for other
    /// beads' commits, pushed or not: the gate defers to the note, so the
    /// closure fails without one and passes with one.
    #[tokio::test]
    async fn other_beads_commits_are_not_credited_to_this_dispatch() {
        let repo = git_repo();
        let _remote = bare_upstream(&repo);
        let before = head(&repo);
        commit_file(
            &repo,
            "pushed.rs",
            "fn pushed() {}\n",
            &format!("feat({OTHER_BEAD}): pushed by another worker"),
        );
        git_ok(repo.path(), &["push", "-q"]);
        commit_file(
            &repo,
            "unpushed.rs",
            "fn unpushed() {}\n",
            "fix(needle-5e6f7a8b): not yet pushed by a third worker",
        );
        let snapshot = snapshot_at(before);

        let git_verdict = check_commit(repo.path(), BEAD, Some(&snapshot))
            .await
            .expect("shipped-work check should run");
        assert!(git_verdict.is_none(), "got {git_verdict:?}");

        let without_note = evaluate(repo.path(), BEAD, Some(&snapshot), "", false)
            .await
            .expect("evaluate should run");
        assert!(
            matches!(without_note, GateResult::Fail(ref reason)
                if reason.contains("no substantial pushed commit")),
            "got {without_note:?}"
        );
        let with_note = evaluate(
            repo.path(),
            BEAD,
            Some(&snapshot),
            "no code change needed: already fixed upstream",
            false,
        )
        .await
        .expect("evaluate should run");
        assert_eq!(with_note, GateResult::Pass);
    }

    /// The dispatch's own unpushed commit beneath another bead's tip fails,
    /// and the failure names the dispatch's commit, not the tip.
    #[tokio::test]
    async fn own_unpushed_commit_beneath_another_beads_tip_fails_naming_it() {
        let repo = git_repo();
        let _remote = bare_upstream(&repo);
        let before = head(&repo);
        let own = commit_file(
            &repo,
            "work.rs",
            "fn work() {}\n",
            &format!("fix({BEAD}): own work"),
        );
        let tip = commit_file(
            &repo,
            "other.rs",
            "fn other() {}\n",
            &format!("feat({OTHER_BEAD}): a concurrent worker's commit"),
        );

        match verdict(&repo, &snapshot_at(before)).await {
            GateResult::Fail(reason) => {
                assert!(reason.contains("has not been pushed"), "{reason}");
                assert!(reason.contains(&own[..7]), "{reason}");
                assert!(!reason.contains(&tip[..7]), "{reason}");
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    /// An unpushed commit that touches only bead state carries nothing the
    /// gate requires to ship; the pushed substantial work passes.
    #[tokio::test]
    async fn unpushed_trivial_commit_does_not_fail_pushed_work() {
        let repo = git_repo();
        let _remote = bare_upstream(&repo);
        let before = head(&repo);
        commit_file(
            &repo,
            "work.rs",
            "fn work() {}\n",
            &format!("fix({BEAD}): own work"),
        );
        git_ok(repo.path(), &["push", "-q"]);
        commit_file(
            &repo,
            ".beads/checkpoint.json",
            "{}\n",
            "chore(beads): checkpoint",
        );

        assert_eq!(verdict(&repo, &snapshot_at(before)).await, GateResult::Pass);
    }

    #[test]
    fn commit_attribution_reads_the_conventional_scope_strictly() {
        // Another bead of this workspace, never naming this one: foreign.
        assert!(names_other_bead("feat(needle-9f8e7d6c): x", BEAD));
        assert!(names_other_bead("wip(needle-49vt): x", BEAD));
        assert!(names_other_bead("fix(needle-9f8e7d6c)!: x", BEAD));
        assert!(names_other_bead(
            "fix(needle-9f8e7d6c, needle-5e6f7a8b): x",
            BEAD
        ));
        // This bead, anywhere in the message: its own.
        assert!(!names_other_bead("fix(needle-1a2b3c4d): x", BEAD));
        assert!(!names_other_bead(
            "feat(needle-9f8e7d6c): x\n\nAlso closes needle-1a2b3c4d.",
            BEAD
        ));
        // Not attributable, so judged as this dispatch's own.
        assert!(!names_other_bead("ship work", BEAD));
        assert!(!names_other_bead(
            "chore(release): auto-bump to v0.6.41",
            BEAD
        ));
        assert!(!names_other_bead("fix(needle-ci): x", BEAD));
        assert!(!names_other_bead("docs(needle-docs): x", BEAD));
        assert!(!names_other_bead("feat(twill-9f8e7d6c): x", BEAD));
        assert!(!names_other_bead(
            "Revert \"feat(needle-9f8e7d6c): x\"",
            BEAD
        ));
        assert!(!names_other_bead(
            "Merge remote-tracking branch 'origin/main'",
            BEAD
        ));
        assert!(!names_other_bead(
            "feat(needle-9f8e7d6c) without colon",
            BEAD
        ));
        assert!(!names_other_bead("feat(needle-9f8e7d6c): x", ""));
        assert!(!names_other_bead("feat(needle-9f8e7d6c): x", "nodash"));
    }
}
