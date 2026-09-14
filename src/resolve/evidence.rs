//! Bounded evidence bundles for post-Pluck resolution.
//!
//! Before this module the resolve prompt carried only the bead context plus a
//! truncated stdout/stderr dump — the decider could not see what the attempt
//! actually did to the workspace, whether earlier attempts already failed the
//! same way, or what the acceptance criteria were. This module captures that
//! evidence and renders it as the prompt section appended by
//! [`Resolver::build_prompt`](super::Resolver::build_prompt).
//!
//! Seven sections, each sourced from data that already exists:
//!
//! - **Acceptance criteria** — parsed from the bead body's acceptance
//!   heading, falling back to the whole (bounded) body.
//! - **This dispatch** — exit code, derived exit reason, and sanitized tails
//!   of the finished attempt's output.
//! - **Git state** — the on-disk [`PreDispatch`] snapshot (pre) beside a
//!   freshly observed HEAD + dirty-path read (post).
//! - **Commits and diff summary** — `git log`/`git diff --stat` across the
//!   dispatch baseline, bounded like `CommittedWorkSummary`.
//! - **Validation results** — per-attempt outcome and terminal reason from
//!   the attempt ledger (e.g. `work_failure (gate:definition-of-done)`).
//! - **Failure history** — bounded, sanitized failure summaries from
//!   [`crate::attempt_history`].
//! - **Trace tail** — the last bytes of the attempt's sanitized
//!   `trace.jsonl`.
//!
//! Two invariants shape everything here:
//!
//! - **Capture never writes.** Every read is a file read or a read-only git
//!   invocation (`rev-parse`, `status`, `log`, `diff`) run with
//!   `GIT_OPTIONAL_LOCKS=0` so even git's opportunistic index refresh is
//!   suppressed. Resolve is an analysis-only role; the capture feeding it
//!   must be too.
//! - **Every untrusted field is sanitized then capped.** Text passes the
//!   trace [`Sanitizer`] before a byte cap is applied, so a secret cannot
//!   hide in a byte range a later truncation would keep. A sanitizer that
//!   fails to build withholds the text rather than emitting it unredacted.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::attempt_history;
use crate::sanitize::Sanitizer;
use crate::types::Bead;
use crate::validation::predispatch::{self, PreDispatch};

/// Byte cap on the acceptance-criteria section.
pub const MAX_ACCEPTANCE_BYTES: usize = 2000;
/// Byte cap on each tail of the finished dispatch's output.
pub const MAX_OUTPUT_TAIL_BYTES: usize = 2000;
/// Byte cap on a prior attempt's failure summary (mirrors
/// [`attempt_history::LOCAL_SUMMARY_BYTES`], which bounds it at write time).
pub const MAX_PRIOR_SUMMARY_BYTES: usize = attempt_history::LOCAL_SUMMARY_BYTES;
/// Byte cap on the trace tail.
pub const MAX_TRACE_TAIL_BYTES: usize = 2000;
/// Dirty paths listed before the rest collapse into a `+N more` marker.
pub const MAX_DIRTY_PATHS: usize = 20;
/// Byte cap on a single dirty path.
pub const MAX_PATH_BYTES: usize = 256;
/// Commits listed (oldest kept dropped first) before the rest are counted.
pub const MAX_COMMITS: usize = 10;
/// Byte cap on a single commit subject.
pub const MAX_COMMIT_SUBJECT_BYTES: usize = 200;
/// Byte cap on short ledger and Git metadata fields.
pub const MAX_METADATA_BYTES: usize = 200;
/// Diff file lines listed before the rest collapse into a `+N more` marker.
pub const MAX_DIFF_FILES: usize = 20;
/// Prior attempts rendered as failure history / validation rows.
pub const MAX_FAILURE_HISTORY: usize = 3;
/// Byte cap on the whole rendered section, enforced last.
pub const MAX_RENDER_BYTES: usize = 8000;
/// Wall-clock bound on each read-only git invocation.
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Filename of the structured trace inside a bead's trace directory.
const TRACE_FILE: &str = "trace.jsonl";

// ──────────────────────────────────────────────────────────────────────────────
// Bundle types
// ──────────────────────────────────────────────────────────────────────────────

/// The bounded evidence supplied to the resolver for one bead.
///
/// Built by [`capture`]; never contains an uncapped untrusted field, and is
/// cheap to serialize into telemetry alongside the decision it produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceBundle {
    /// The bead the evidence describes.
    pub bead_id: String,
    /// Acceptance criteria parsed from the bead body.
    pub acceptance_criteria: String,
    /// The finished dispatch's exit evidence.
    pub dispatch: DispatchEvidence,
    /// Pre- and post-dispatch git state.
    pub git: GitEvidence,
    /// Commits made during the dispatch, oldest first.
    pub commits: Vec<CommitEntry>,
    /// Older commits dropped by the [`MAX_COMMITS`] cap.
    pub commits_omitted: usize,
    /// Commit diff summary across the dispatch baseline.
    pub diff_summary: Option<DiffSummary>,
    /// Prior attempts' validation outcomes, oldest first.
    pub validation: Vec<ValidationOutcome>,
    /// Prior attempts' bounded failure records, oldest first.
    pub failure_history: Vec<AttemptFailure>,
    /// Total prior attempts on the ledger, of which [`Self::failure_history`]
    /// keeps the newest [`MAX_FAILURE_HISTORY`].
    pub history_total: usize,
    /// Bounded tail of the attempt's sanitized trace, when one exists.
    pub trace_tail: Option<String>,
}

/// Exit evidence for the dispatch being resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchEvidence {
    /// Process exit code from Pluck.
    pub exit_code: i32,
    /// `"success"` or `"failure"`, as the resolve prompt already shows it.
    pub exit_status: String,
    /// Whether the operation was interrupted (SIGINT/SIGTERM).
    pub was_interrupted: bool,
    /// Machine-readable reason: `interrupted` or `exit_code:N`.
    pub exit_reason: String,
    /// Sanitized, capped tail of the dispatch's stdout.
    pub stdout_tail: String,
    /// Sanitized, capped tail of the dispatch's stderr.
    pub stderr_tail: String,
}

/// Workspace git state at one end of the dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitState {
    /// `git rev-parse HEAD`, or `None` when not a git repo.
    pub head_sha: Option<String>,
    /// Whether the working tree has no captured dirty paths.
    pub clean: bool,
    /// Dirty paths (status-porcelain order, `.beads/` noise filtered).
    pub dirty_paths: Vec<String>,
    /// Paths dropped by the [`MAX_DIRTY_PATHS`] cap.
    pub dirty_paths_omitted: usize,
}

/// Both ends of the dispatch's git state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitEvidence {
    /// The on-disk pre-dispatch snapshot, when one was recorded.
    pub pre_dispatch: Option<GitState>,
    /// Freshly observed state at resolve time.
    pub post_dispatch: GitState,
}

/// One commit the dispatch produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitEntry {
    /// Full commit SHA.
    pub sha: String,
    /// Sanitized, capped commit subject.
    pub subject: String,
}

/// Net `git diff --stat` from the dispatch baseline through the current worktree:
/// what the dispatch left behind, including tracked uncommitted edits, rather
/// than cumulative per-commit churn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
    /// Per-file stat lines, capped by [`MAX_DIFF_FILES`].
    pub files: Vec<String>,
    /// File lines dropped by the cap.
    pub files_omitted: usize,
}

/// One prior attempt's validation outcome from the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationOutcome {
    /// 1-based attempt number across the bead's full history.
    pub attempt: u32,
    /// Semantic outcome class (`verified_success`, `work_failure`, …).
    pub outcome: String,
    /// Machine-readable terminal reason (`gate:definition-of-done`,
    /// `timeout`, `exit_code:1`, …).
    pub terminal_reason: Option<String>,
}

/// One prior attempt's bounded failure record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptFailure {
    /// 1-based attempt number across the bead's full history.
    pub attempt: u32,
    /// Adapter that executed the attempt.
    pub adapter: String,
    /// RFC 3339 time the attempt resolved.
    pub recorded_at: String,
    /// Semantic outcome class.
    pub outcome: String,
    /// Process exit code (observation only).
    pub exit_code: i32,
    /// Commits the attempt left in the workspace.
    pub commits: Vec<String>,
    /// Bounded, sanitized failure summary.
    pub summary: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Capture
// ──────────────────────────────────────────────────────────────────────────────

/// Capture the evidence bundle for a finished dispatch.
///
/// Reads the process-HOME predispatch state root; see [`capture_in`] for the
/// explicit-root variant tests and isolated callers use. Capture is
/// deliberately infallible: a piece of evidence that cannot be read degrades
/// to an absent/unknown field rather than failing the resolve, because a
/// partial bundle still beats no decision evidence at all.
pub async fn capture(
    workspace: &Path,
    bead: &Bead,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    was_interrupted: bool,
) -> EvidenceBundle {
    capture_at(
        &predispatch::snapshot_path(workspace, &bead.id),
        workspace,
        bead,
        exit_code,
        stdout,
        stderr,
        was_interrupted,
    )
    .await
}

/// [`capture`] against an explicit predispatch state root, so tests never
/// touch the process `HOME` the default root derives from.
pub async fn capture_in(
    root: &Path,
    workspace: &Path,
    bead: &Bead,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    was_interrupted: bool,
) -> EvidenceBundle {
    capture_at(
        &predispatch::snapshot_path_in(root, workspace, &bead.id),
        workspace,
        bead,
        exit_code,
        stdout,
        stderr,
        was_interrupted,
    )
    .await
}

/// The shared capture core. `snapshot_file` is where the dispatch's
/// [`PreDispatch`] baseline would live — outside the workspace, and only ever
/// read.
#[allow(clippy::too_many_arguments)]
async fn capture_at(
    snapshot_file: &Path,
    workspace: &Path,
    bead: &Bead,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    was_interrupted: bool,
) -> EvidenceBundle {
    let sanitizer = sanitizer();

    let pre_dispatch = load_snapshot(snapshot_file).map(|snapshot| {
        let (dirty_paths, dirty_paths_omitted) = cap_paths(
            snapshot
                .dirty_files
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>(),
            sanitizer,
        );
        GitState {
            head_sha: snapshot
                .head_sha
                .map(|sha| bounded(sanitizer, &sha, MAX_METADATA_BYTES)),
            clean: snapshot.dirty_files.is_empty(),
            dirty_paths,
            dirty_paths_omitted,
        }
    });

    let post_head = git_head(workspace).await;
    let post_has_head = post_head.is_some();
    let post_dispatch = match git_dirty_paths(workspace).await {
        Some(paths) => {
            let clean = paths.is_empty();
            let (dirty_paths, dirty_paths_omitted) = cap_paths(paths, sanitizer);
            GitState {
                head_sha: post_head,
                clean,
                dirty_paths,
                dirty_paths_omitted,
            }
        }
        None => GitState {
            head_sha: post_head,
            clean: false,
            dirty_paths: Vec::new(),
            dirty_paths_omitted: 0,
        },
    };

    // Commits and the diff both need the dispatch baseline; without it they
    // are unknown rather than empty.
    let baseline = pre_dispatch
        .as_ref()
        .and_then(|pre| pre.head_sha.clone())
        .filter(|sha| !sha.is_empty());
    let (commits, commits_omitted, diff_summary) = match baseline {
        Some(sha) if post_has_head => {
            let all = git_commits(workspace, &sha).await;
            let omitted = all.len().saturating_sub(MAX_COMMITS);
            let commits = all
                .into_iter()
                .skip(omitted)
                .map(|mut c| {
                    c.subject = bounded(sanitizer, &c.subject, MAX_COMMIT_SUBJECT_BYTES);
                    c
                })
                .collect();
            let diff_summary = git_diff_stat(workspace, &sha, sanitizer).await;
            (commits, omitted, diff_summary)
        }
        _ => (Vec::new(), 0, None),
    };

    let records = attempt_history::load_local(workspace, &bead.id).unwrap_or_default();
    let history_total = records.len();
    let window_start = records.len().saturating_sub(MAX_FAILURE_HISTORY);
    let window = &records[window_start..];
    let validation = window
        .iter()
        .enumerate()
        .map(|(i, r)| ValidationOutcome {
            attempt: (window_start + i + 1) as u32,
            outcome: bounded(sanitizer, &r.outcome, MAX_METADATA_BYTES),
            terminal_reason: r
                .terminal_reason
                .as_deref()
                .map(|reason| bounded(sanitizer, reason, MAX_METADATA_BYTES)),
        })
        .collect();
    let failure_history = window
        .iter()
        .enumerate()
        .map(|(i, r)| AttemptFailure {
            attempt: (window_start + i + 1) as u32,
            adapter: bounded(sanitizer, &r.adapter, MAX_METADATA_BYTES),
            recorded_at: bounded(sanitizer, &r.recorded_at, MAX_METADATA_BYTES),
            outcome: bounded(sanitizer, &r.outcome, MAX_METADATA_BYTES),
            exit_code: r.exit_code,
            // The ledger caps a record's summary but not its commit list.
            commits: {
                let mut commits = r.commits.clone();
                commits.truncate(MAX_COMMITS);
                commits
                    .into_iter()
                    .map(|commit| bounded(sanitizer, &commit, MAX_METADATA_BYTES))
                    .collect()
            },
            summary: r
                .failure_summary
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(|s| bounded(sanitizer, s, MAX_PRIOR_SUMMARY_BYTES)),
        })
        .collect();

    let trace_tail = trace_tail(workspace, bead, sanitizer);

    EvidenceBundle {
        bead_id: bead.id.as_ref().to_string(),
        acceptance_criteria: acceptance_criteria(bead, sanitizer),
        dispatch: DispatchEvidence {
            exit_code,
            exit_status: if exit_code == 0 { "success" } else { "failure" }.to_string(),
            was_interrupted,
            exit_reason: exit_reason(was_interrupted, exit_code),
            stdout_tail: bounded(sanitizer, stdout, MAX_OUTPUT_TAIL_BYTES),
            stderr_tail: bounded(sanitizer, stderr, MAX_OUTPUT_TAIL_BYTES),
        },
        git: GitEvidence {
            pre_dispatch,
            post_dispatch,
        },
        commits,
        commits_omitted,
        diff_summary,
        validation,
        failure_history,
        history_total,
        trace_tail,
    }
}

/// The shared trace sanitizer, built once from the vendored rules.
///
/// A build failure is remembered and withholds every untrusted field
/// (`bounded` emits a placeholder) — bundling never emits unredacted text.
fn sanitizer() -> Option<&'static Sanitizer> {
    static SANITIZER: OnceLock<Result<Sanitizer, String>> = OnceLock::new();
    match SANITIZER.get_or_init(|| Sanitizer::new(&[]).map_err(|e| e.to_string())) {
        Ok(sanitizer) => Some(sanitizer),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "evidence: sanitizer unavailable — untrusted text will be withheld"
            );
            None
        }
    }
}

/// Sanitize then cap untrusted text: sanitize first, so a secret cannot be
/// cut in half by a truncation that would then evade the rules.
fn bounded(sanitizer: Option<&Sanitizer>, text: &str, max_bytes: usize) -> String {
    match sanitizer {
        Some(sanitizer) => {
            attempt_history::truncate_head_tail(&sanitizer.sanitize(text), max_bytes)
        }
        // Fail closed: no sanitizer, no unredacted text.
        None => "(withheld: redaction unavailable)".to_string(),
    }
}

/// Sanitize and bound a dirty-path list, counting what the cap dropped.
fn cap_paths(paths: Vec<String>, sanitizer: Option<&Sanitizer>) -> (Vec<String>, usize) {
    let omitted = paths.len().saturating_sub(MAX_DIRTY_PATHS);
    let bounded_paths = paths
        .into_iter()
        .skip(omitted)
        .map(|p| {
            let capped: String = p.chars().take(MAX_PATH_BYTES).collect();
            bounded(sanitizer, &capped, MAX_PATH_BYTES)
        })
        .collect();
    (bounded_paths, omitted)
}

/// Cap a diff's file lines, counting what was dropped.
fn cap_diff_files(mut diff: DiffSummary) -> DiffSummary {
    if diff.files.len() > MAX_DIFF_FILES {
        diff.files_omitted = diff.files.len() - MAX_DIFF_FILES;
        diff.files.truncate(MAX_DIFF_FILES);
    }
    diff
}

/// Machine-readable exit reason for the finished dispatch.
fn exit_reason(was_interrupted: bool, exit_code: i32) -> String {
    if was_interrupted {
        "interrupted".to_string()
    } else {
        format!("exit_code:{exit_code}")
    }
}

/// Acceptance criteria for the bead: the body's acceptance heading section,
/// or the whole bounded body when no such heading exists.
fn acceptance_criteria(bead: &Bead, sanitizer: Option<&Sanitizer>) -> String {
    let body = bead.body.as_deref().unwrap_or("(no description)");
    let section = acceptance_section(body);
    let text = if section.trim().is_empty() {
        body
    } else {
        section.as_str()
    };
    bounded(sanitizer, text, MAX_ACCEPTANCE_BYTES)
}

/// Extract the section under a heading whose text mentions "acceptance",
/// ending at the next heading of the same or higher level.
fn acceptance_section(body: &str) -> String {
    let mut collected: Option<(usize, String)> = None;
    for line in body.lines() {
        match (heading(line), &mut collected) {
            (Some((level, text)), None) if text.to_lowercase().contains("acceptance") => {
                collected = Some((level, format!("{line}\n")));
            }
            (Some((level, _)), Some((start_level, _))) if level <= *start_level => break,
            (_, Some((_, out))) => {
                out.push_str(line);
                out.push('\n');
            }
            (_, None) => {}
        }
    }
    collected.map(|(_, out)| out).unwrap_or_default()
}

/// `(level, text)` of a markdown heading line, or `None` for other lines.
fn heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    let text = rest.strip_prefix(' ')?.trim();
    Some((hashes, text))
}

/// Load the dispatch's predispatch snapshot, when one exists on disk.
fn load_snapshot(path: &Path) -> Option<PreDispatch> {
    let raw = std::fs::read(path).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Bounded, sanitized tail of the bead's trace file, when one exists.
fn trace_tail(workspace: &Path, bead: &Bead, sanitizer: Option<&Sanitizer>) -> Option<String> {
    let path = trace_path(workspace, bead);
    let raw = file_tail(&path, MAX_TRACE_TAIL_BYTES)?;
    Some(bounded(sanitizer, &raw, MAX_TRACE_TAIL_BYTES))
}

/// Path of the structured trace for `bead`, mirroring the trace layout.
fn trace_path(workspace: &Path, bead: &Bead) -> PathBuf {
    workspace
        .join(".beads")
        .join("traces")
        .join(bead.id.as_ref())
        .join(TRACE_FILE)
}

/// The last `max_bytes` of a file, cut to a line boundary so the tail holds
/// whole records. `None` when the file is unreadable.
fn file_tail(path: &Path, max_bytes: usize) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.take(max_bytes as u64).read_to_end(&mut buf).ok()?;
    let mut text = String::from_utf8_lossy(&buf).to_string();
    if start > 0 {
        // Drop the partial record the window opened mid-way through.
        let newline = text.find('\n')? + 1;
        text.drain(..newline);
    }
    Some(text)
}

// ──────────────────────────────────────────────────────────────────────────────
// Read-only git helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Run a read-only git command, returning its stdout or `None`.
///
/// `GIT_OPTIONAL_LOCKS=0` stops git from taking the index lock for
/// background refreshes, so capture cannot touch the workspace even through
/// git's own opportunistic writes.
async fn git_text(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(GIT_TIMEOUT, async {
        tokio::process::Command::new("git")
            .args(args)
            .current_dir(workspace)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .kill_on_drop(true)
            .output()
            .await
    })
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// `git rev-parse HEAD` in the workspace.
async fn git_head(workspace: &Path) -> Option<String> {
    Some(
        git_text(workspace, &["rev-parse", "HEAD"])
            .await?
            .trim()
            .to_string(),
    )
}

/// Dirty paths from `git status --porcelain`, with the same `.beads/` noise
/// filter the predispatch snapshot uses. `None` when not a git repo.
async fn git_dirty_paths(workspace: &Path) -> Option<Vec<String>> {
    let stdout = git_text(workspace, &["status", "--porcelain"]).await?;
    Some(
        stdout
            .lines()
            .filter_map(dirty_path_of)
            .filter(|path| !path.starts_with(".beads/") && path != ".needle-predispatch-sha")
            .collect(),
    )
}

/// The path half of one porcelain line, following renames to their target.
fn dirty_path_of(line: &str) -> Option<String> {
    let path = line.get(3..)?.trim();
    if path.is_empty() {
        return None;
    }
    Some(
        path.rsplit_once(" -> ")
            .map(|(_, target)| target.trim().to_string())
            .unwrap_or_else(|| path.to_string()),
    )
}

/// Commits in `since..HEAD`, oldest first.
async fn git_commits(workspace: &Path, since: &str) -> Vec<CommitEntry> {
    let range = format!("{since}..HEAD");
    let Some(stdout) = git_text(
        workspace,
        &["log", "--format=%H%x1f%s", "--reverse", &range],
    )
    .await
    else {
        return Vec::new();
    };
    stdout
        .lines()
        .filter_map(|line| {
            let (sha, subject) = line.split_once('\u{1f}')?;
            Some(CommitEntry {
                sha: sha.trim().to_string(),
                subject: subject.trim().to_string(),
            })
        })
        .collect()
}

/// `git diff --stat` from `since` through the current worktree, parsed into a
/// bounded summary. Unlike a `since..HEAD` range, this includes tracked staged
/// and unstaged edits that the dispatch did not commit.
async fn git_diff_stat(
    workspace: &Path,
    since: &str,
    sanitizer: Option<&Sanitizer>,
) -> Option<DiffSummary> {
    let stdout = git_text(workspace, &["diff", "--stat", since]).await?;
    let mut lines = stdout.lines().filter(|l| !l.trim().is_empty());
    let totals = lines.next_back()?;
    let mut summary = DiffSummary {
        files_changed: 0,
        insertions: 0,
        deletions: 0,
        files: Vec::new(),
        files_omitted: 0,
    };
    for token in totals.split(',') {
        let number: u64 = token.trim().split(' ').next()?.parse().ok()?;
        if token.contains("insertion") {
            summary.insertions = number;
        } else if token.contains("deletion") {
            summary.deletions = number;
        } else if token.contains("file") {
            summary.files_changed = number;
        }
    }
    summary.files = lines
        .map(|line| bounded(sanitizer, line, MAX_PATH_BYTES))
        .collect();
    Some(cap_diff_files(summary))
}

// ──────────────────────────────────────────────────────────────────────────────
// Render
// ──────────────────────────────────────────────────────────────────────────────

/// Append the bundle's rendered section to a built resolve prompt.
///
/// Used by [`super::Resolver::resolve`] so both the default and custom
/// template paths carry the evidence, whatever variables the template itself
/// knows about.
pub(crate) fn append_evidence(prompt: &mut String, bundle: &EvidenceBundle) {
    prompt.push_str("\n\n");
    prompt.push_str(render(bundle).trim_end());
}

/// Render the bundle as the prompt section appended to the resolve prompt.
///
/// A pure function of the bundle: same bundle, same bytes.
pub fn render(bundle: &EvidenceBundle) -> String {
    let mut out = String::new();
    out.push_str("## Evidence Bundle\n\n");
    out.push_str(
        "Bounded evidence captured read-only at resolve time. Every untrusted field \
         below is sanitized and capped; `[REDACTED:…]` marks redacted material.\n\n",
    );

    out.push_str("### Acceptance criteria\n\n");
    out.push_str(bundle.acceptance_criteria.trim_end());
    out.push_str("\n\n");

    let d = &bundle.dispatch;
    out.push_str("### This dispatch\n\n");
    out.push_str(&format!(
        "- Exit: {} ({}), reason: `{}`, interrupted: {}\n",
        d.exit_code, d.exit_status, d.exit_reason, d.was_interrupted
    ));
    out.push_str(&format!(
        "- Stdout tail:\n```\n{}\n```\n",
        d.stdout_tail.trim_end()
    ));
    out.push_str(&format!(
        "- Stderr tail:\n```\n{}\n```\n",
        d.stderr_tail.trim_end()
    ));

    out.push_str("### Git state\n\n");
    out.push_str(&render_git_state(
        "- Pre-dispatch",
        bundle.git.pre_dispatch.as_ref(),
        "unknown (no snapshot recorded)",
    ));
    out.push_str(&render_git_state(
        "- Post-dispatch",
        Some(&bundle.git.post_dispatch),
        "unknown",
    ));

    out.push_str("\n### Commits and diff summary\n\n");
    if bundle.commits.is_empty() {
        match bundle
            .git
            .pre_dispatch
            .as_ref()
            .and_then(|p| p.head_sha.clone())
        {
            Some(_) => out.push_str("No commits were made during this dispatch.\n"),
            None => out.push_str(
                "Commits unknown — no pre-dispatch baseline was recorded for this dispatch.\n",
            ),
        }
    } else {
        out.push_str(&format!(
            "{} commit(s) since dispatch (oldest first){}:\n",
            bundle.commits.len(),
            omitted_suffix(bundle.commits_omitted)
        ));
        for commit in &bundle.commits {
            out.push_str(&format!(
                "- {} {}\n",
                commit.sha.chars().take(12).collect::<String>(),
                commit.subject.trim_end()
            ));
        }
    }
    match &bundle.diff_summary {
        Some(diff) => {
            out.push_str(&format!(
                "Diff: {} file(s) changed, {} insertion(s), {} deletion(s)\n",
                diff.files_changed, diff.insertions, diff.deletions
            ));
            for file in &diff.files {
                out.push_str(&format!("  {}\n", file.trim_end()));
            }
            if diff.files_omitted > 0 {
                out.push_str(&format!("  … (+{} more file(s))\n", diff.files_omitted));
            }
        }
        None => out.push_str("Diff: unknown (no pre-dispatch baseline or no commits).\n"),
    }

    out.push_str("\n### Validation results\n\n");
    if bundle.validation.is_empty() {
        out.push_str("No prior validation results — this is the first attempt.\n");
    } else {
        for v in &bundle.validation {
            out.push_str(&format!(
                "- Attempt {}: {}{}\n",
                v.attempt,
                v.outcome,
                v.terminal_reason
                    .as_deref()
                    .map(|t| format!(" (`{t}`)"))
                    .unwrap_or_default()
            ));
        }
    }

    out.push_str("\n### Failure history\n\n");
    if bundle.failure_history.is_empty() {
        out.push_str("No prior attempts.\n");
    } else {
        out.push_str(&format!(
            "{} prior attempt(s) on this bead; showing the newest {}{}\n",
            bundle.history_total,
            bundle.failure_history.len(),
            if bundle.failure_history.len() < bundle.history_total {
                " (older omitted)"
            } else {
                ""
            }
        ));
        // Newest first, mirroring the attempt-history prompt section.
        for failure in bundle.failure_history.iter().rev() {
            out.push_str(&format!(
                "\n#### Attempt {} — {} — {} — exit {}\n",
                failure.attempt, failure.adapter, failure.outcome, failure.exit_code
            ));
            if !failure.commits.is_empty() {
                out.push_str(&format!(
                    "Commits it left: {}\n",
                    failure
                        .commits
                        .iter()
                        .map(|c| c.chars().take(12).collect::<String>())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if let Some(summary) = &failure.summary {
                out.push_str(&format!("```\n{}\n```\n", summary.trim_end()));
            }
        }
    }

    out.push_str("\n### Trace tail\n\n");
    match &bundle.trace_tail {
        Some(tail) => out.push_str(&format!("```\n{}\n```\n", tail.trim_end())),
        None => out.push_str("No trace recorded for this bead.\n"),
    }

    attempt_history::truncate_head_tail(&out, MAX_RENDER_BYTES)
}

/// One `### Git state` bullet for one end of the dispatch.
fn render_git_state(label: &str, state: Option<&GitState>, unknown: &str) -> String {
    let Some(state) = state else {
        return format!("{label}: {unknown}\n");
    };
    let head = state
        .head_sha
        .as_deref()
        .map(|sha| sha.chars().take(12).collect::<String>())
        .unwrap_or_else(|| "unknown".to_string());
    if state.clean {
        return format!("{label}: HEAD {head}, clean\n");
    }
    if state.dirty_paths.is_empty() {
        // Dirty with no listable paths: the status read failed, not the tree.
        return format!("{label}: HEAD {head}, state unknown\n");
    }
    let mut line = format!(
        "{label}: HEAD {head}, dirty ({} path(s){})\n",
        state.dirty_paths.len(),
        omitted_suffix(state.dirty_paths_omitted)
    );
    for path in &state.dirty_paths {
        line.push_str(&format!("{label}:   {path}\n"));
    }
    line
}

/// `", +N more"` when a cap dropped entries, `""` otherwise.
fn omitted_suffix(omitted: usize) -> String {
    if omitted == 0 {
        String::new()
    } else {
        format!(", +{omitted} more")
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BeadId, BeadStatus};

    const DEFAULT_BODY: &str =
        "Do the thing.\n\n## Acceptance criteria\n\n- clean case covered\n- dirty case covered\n\n## Notes\n\nsomething else";

    fn bead(body: &str) -> Bead {
        Bead {
            id: BeadId::from("needle-evidence-test"),
            title: "Build bounded evidence bundles".to_string(),
            body: Some(body.to_string()),
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some("worker-01".to_string()),
            labels: vec![],
            workspace: PathBuf::from("/workspace"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn sample_bundle() -> EvidenceBundle {
        EvidenceBundle {
            bead_id: "needle-evidence-test".to_string(),
            acceptance_criteria: "- deterministic".to_string(),
            dispatch: DispatchEvidence {
                exit_code: 0,
                exit_status: "success".to_string(),
                was_interrupted: false,
                exit_reason: "exit_code:0".to_string(),
                stdout_tail: "done".to_string(),
                stderr_tail: String::new(),
            },
            git: GitEvidence {
                pre_dispatch: Some(GitState {
                    head_sha: Some("0123456789abcdef".to_string()),
                    clean: true,
                    dirty_paths: vec![],
                    dirty_paths_omitted: 0,
                }),
                post_dispatch: GitState {
                    head_sha: Some("0123456789abcdef".to_string()),
                    clean: true,
                    dirty_paths: vec![],
                    dirty_paths_omitted: 0,
                },
            },
            commits: vec![],
            commits_omitted: 0,
            diff_summary: None,
            validation: vec![],
            failure_history: vec![],
            history_total: 0,
            trace_tail: None,
        }
    }

    #[test]
    fn acceptance_section_is_extracted_and_bounded() {
        let criteria = acceptance_criteria(&bead(DEFAULT_BODY), sanitizer());
        assert!(criteria.contains("## Acceptance criteria"), "{criteria}");
        assert!(criteria.contains("- dirty case covered"), "{criteria}");
        assert!(!criteria.contains("something else"), "{criteria}");
    }

    #[test]
    fn body_without_acceptance_heading_falls_back_to_whole_body() {
        let criteria = acceptance_criteria(&bead("Just do it, deterministically."), sanitizer());
        assert_eq!(criteria, "Just do it, deterministically.");
    }

    #[test]
    fn acceptance_section_stops_at_a_same_level_heading() {
        let body = "# Acceptance criteria\n\n- a\n- b\n# Next\n\nother";
        assert_eq!(
            acceptance_section(body),
            "# Acceptance criteria\n\n- a\n- b\n"
        );
    }

    #[test]
    fn oversized_body_is_capped() {
        let criteria =
            acceptance_criteria(&bead(&"y".repeat(MAX_ACCEPTANCE_BYTES * 3)), sanitizer());
        assert!(criteria.len() <= MAX_ACCEPTANCE_BYTES + "[elided]\n".len());
        assert!(criteria.contains("[elided]"), "{criteria}");
    }

    #[test]
    fn render_is_pure_and_bounded() {
        let mut bundle = sample_bundle();
        bundle.dispatch.stdout_tail = "x".repeat(MAX_RENDER_BYTES * 2);
        let first = render(&bundle);
        let second = render(&bundle);
        assert_eq!(first, second);
        assert!(first.len() <= MAX_RENDER_BYTES);
        assert!(first.contains("[elided]"), "{first}");
    }

    #[test]
    fn append_evidence_preserves_prompt_and_adds_one_section() {
        let bundle = sample_bundle();
        let mut prompt = "# Resolve needle-evidence-test\n".to_string();
        append_evidence(&mut prompt, &bundle);
        assert!(prompt.starts_with("# Resolve needle-evidence-test\n"));
        assert_eq!(prompt.matches("## Evidence Bundle").count(), 1);
        assert!(prompt.contains("No commits were made"));
        assert!(prompt.contains("### Acceptance criteria"));
    }
}
