//! DoD bypass detection: a dispatch that committed around the
//! Definition-of-Done hook is a failed dispatch, whatever the commit contains.
//!
//! Replaces `scripts/gate-no-dod-bypass.sh` (removed from `.needle.yaml` in the
//! same change). The shell gate grepped the bypass log for commits whose
//! message named the bead, so it broke whenever a commit message didn't carry
//! the bead id. This check keys on the `validation::predispatch` snapshot
//! instead and never reads a commit message:
//!
//! - entries recorded before the dispatch started (`captured_at`) are ignored;
//! - a commit reachable from the pre-dispatch HEAD predates this dispatch and
//!   is not this dispatch's work;
//! - what remains — a bypass whose commit is reachable from HEAD but not from
//!   the pre-dispatch sha — fails the gate.
//!
//! The failure is reported under gate name [`GATE_NAME`] and routed through the
//! same `handle_gate_failure` path as any other gate, so it reopens the bead,
//! releases it, and counts toward quarantine. The `verification.failed` event
//! carries the offending shas in its output.
//!
//! Depends on: `types`, `validation::predispatch`.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::validation::predispatch::PreDispatch;
use crate::validation::GateResult;

/// Gate name under which a detected bypass is reported (telemetry `command`
/// field of `verification.failed`, and the `GateReport` result key).
pub const GATE_NAME: &str = "dod_bypass";

/// The post-commit hook's bypass log, relative to the workspace root.
const BYPASSES_LOG: &str = ".beads/bypasses.jsonl";

/// One row of `.beads/bypasses.jsonl`, as `scripts/bypass-detection.sh` writes
/// it. Only `timestamp` is required: legacy rows are `{timestamp, lane, pwd}`
/// with no commit to attribute, so they deserialize with an empty `commit_sha`
/// and are never selected. Fields this gate does not use are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct BypassEvent {
    /// When the hook recorded the bypass.
    pub timestamp: DateTime<Utc>,
    /// Full sha of the commit that bypassed verification (absent on legacy rows).
    #[serde(default)]
    pub commit_sha: String,
    /// How the bypass happened (`--no-verify`, `SKIP_CHECKS=1`), for the reason text.
    #[serde(default)]
    pub pattern: String,
}

/// Check whether this dispatch committed around the DoD hook.
///
/// `snapshot` is the pre-dispatch snapshot the worker recorded (`None` means it
/// could not be read — without it neither filter has a baseline, so the gate
/// passes and leaves the judgment to the shipped-work check, which fails closed
/// without a snapshot).
///
/// A missing log, an empty selection, or an infrastructure failure (unreadable
/// log, git error) all pass: this gate detects bypasses, it does not speculate
/// about them, and a workspace whose git is momentarily unusable must not have
/// every closure fail.
pub async fn check_dod_bypass(
    workspace: &Path,
    snapshot: Option<&PreDispatch>,
) -> Result<GateResult> {
    let Some(snapshot) = snapshot else {
        tracing::debug!(
            workspace = %workspace.display(),
            "no pre-dispatch snapshot — dod_bypass check has no baseline, passing"
        );
        return Ok(GateResult::Pass);
    };

    let log_path = workspace.join(BYPASSES_LOG);
    if !log_path.exists() {
        return Ok(GateResult::Pass);
    }
    let events = match read_bypasses_log(&log_path).await {
        Ok(events) => events,
        Err(e) => {
            tracing::warn!(
                workspace = %workspace.display(),
                error = %e,
                "failed to read bypass log — dod_bypass check failing open"
            );
            return Ok(GateResult::Pass);
        }
    };
    if events.is_empty() {
        return Ok(GateResult::Pass);
    }

    let new_commits = match commits_since_dispatch(workspace, snapshot).await {
        Ok(commits) => commits,
        Err(e) => {
            tracing::warn!(
                workspace = %workspace.display(),
                error = %e,
                "could not list commits since dispatch — dod_bypass check failing open"
            );
            return Ok(GateResult::Pass);
        }
    };

    let hits = select_bypassed_events(&events, snapshot.captured_at, &new_commits);
    if hits.is_empty() {
        tracing::debug!(
            workspace = %workspace.display(),
            entries = events.len(),
            "no bypass attributable to this dispatch"
        );
        return Ok(GateResult::Pass);
    }

    let reason = failure_reason(&hits);
    tracing::warn!(
        workspace = %workspace.display(),
        shas = ?hits.iter().map(|h| h.commit_sha.as_str()).collect::<Vec<_>>(),
        "bypassed Definition-of-Done hook during dispatch — gate failing"
    );
    Ok(GateResult::Fail(reason))
}

/// Select the bypass events attributable to this dispatch.
///
/// An event counts when it carries a commit sha, was recorded after the
/// snapshot's `captured_at` (when the snapshot predates that field, there is no
/// time baseline and every entry is a candidate), and that sha is among
/// `commits_since_dispatch`.
fn select_bypassed_events(
    events: &[BypassEvent],
    captured_at: Option<DateTime<Utc>>,
    commits_since_dispatch: &HashSet<String>,
) -> Vec<BypassEvent> {
    events
        .iter()
        .filter(|e| !e.commit_sha.trim().is_empty())
        .filter(|e| captured_at.map_or(true, |t| e.timestamp > t))
        .filter(|e| commits_since_dispatch.contains(normalize_sha(&e.commit_sha).as_str()))
        .cloned()
        .collect()
}

/// The failure reason for a detected bypass. Starts with [`GATE_NAME`] and
/// carries the full shas so the `verification.failed` telemetry event names
/// exactly which commits bypassed.
fn failure_reason(hits: &[BypassEvent]) -> String {
    let shas: Vec<&str> = hits.iter().map(|e| e.commit_sha.trim()).collect();
    format!(
        "{}: {} commit(s) made with the Definition-of-Done hook bypassed since dispatch: {}. \
         A --no-verify commit is a failed dispatch. The fast lane is scoped to the paths a \
         commit stages (--changed-only), so another worker's in-flight file is not a reason \
         to bypass: fix what the hook names in your files and commit with the hook on.",
        GATE_NAME,
        hits.len(),
        shas.join(", ")
    )
}

/// Commits reachable from HEAD but not from the snapshot's pre-dispatch HEAD —
/// exactly the commits this dispatch added, in one `git rev-list` call. With no
/// recorded HEAD (workspace was not a git repo at dispatch time) every commit
/// reachable from HEAD is a candidate and the timestamp filter does the work.
async fn commits_since_dispatch(
    workspace: &Path,
    snapshot: &PreDispatch,
) -> Result<HashSet<String>> {
    let range = match snapshot.head_sha.as_deref() {
        Some(pre) if !pre.trim().is_empty() => format!("{}..HEAD", pre.trim()),
        _ => "HEAD".to_string(),
    };

    let output = tokio::process::Command::new("git")
        .args(["rev-list", &range])
        .current_dir(workspace)
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!(
            "git rev-list {} failed: {}",
            range,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(normalize_sha)
        .filter(|l| !l.is_empty())
        .collect())
}

/// Shas are compared case-insensitively: git prints lowercase, but a
/// hand-edited log entry should still match rather than be silently dropped.
fn normalize_sha(sha: &str) -> String {
    sha.trim().to_lowercase()
}

/// Read and parse the bypass log. Unparseable lines are skipped with a debug
/// note: one malformed row must not blind the gate to the rows after it.
async fn read_bypasses_log(path: &Path) -> Result<Vec<BypassEvent>> {
    let raw = tokio::fs::read_to_string(path).await?;
    Ok(raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| match serde_json::from_str::<BypassEvent>(line) {
            Ok(event) => Some(event),
            Err(e) => {
                tracing::debug!(
                    line = %line,
                    error = %e,
                    "skipping unparseable bypasses.jsonl line"
                );
                None
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::fs;
    use tempfile::TempDir;

    /// A synthetic bypass event, serialized the way the hook writes it.
    fn event_json(timestamp: DateTime<Utc>, commit_sha: &str, pattern: &str) -> String {
        format!(
            r#"{{"timestamp":"{}","commit_sha":"{}","hostname":"testhost","username":"testuser","lanes_skipped":["fast"],"pattern":"{}","reason":"test bypass","working_directory":"/tmp/test"}}"#,
            timestamp.to_rfc3339_opts(chrono::format::SecondsFormat::Secs, true),
            commit_sha,
            pattern
        )
    }

    fn event(timestamp: DateTime<Utc>, commit_sha: &str) -> BypassEvent {
        BypassEvent {
            timestamp,
            commit_sha: commit_sha.to_string(),
            pattern: "--no-verify".to_string(),
        }
    }

    fn snapshot(head: Option<&str>, captured_at: Option<DateTime<Utc>>) -> PreDispatch {
        PreDispatch {
            head_sha: head.map(|s| s.to_string()),
            notes_hash: None,
            dirty_files: vec![],
            captured_at,
        }
    }

    fn shas(result: &GateResult) -> String {
        match result {
            GateResult::Fail(reason) => reason.clone(),
            other => panic!("expected Fail, got {:?}", other),
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("failed to spawn git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repo with one initial commit; returns its sha.
    async fn init_repo(dir: &Path) -> String {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "test"]);
        fs::write(dir.join("README.md"), "init\n").unwrap();
        git(dir, &["add", "README.md"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        head_sha(dir)
    }

    /// Commit one new file; returns the new HEAD sha.
    async fn commit_file(dir: &Path, name: &str, msg: &str) -> String {
        fs::write(dir.join(name), "contents\n").unwrap();
        git(dir, &["add", name]);
        git(dir, &["commit", "-q", "-m", msg]);
        head_sha(dir)
    }

    fn head_sha(dir: &Path) -> String {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("failed to spawn git rev-parse");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn write_log(dir: &Path, lines: &[String]) {
        let beads = dir.join(".beads");
        fs::create_dir_all(&beads).unwrap();
        fs::write(beads.join("bypasses.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    // ── selection (pure, no git) ──

    #[test]
    fn selects_a_bypass_recorded_after_dispatch_for_a_new_commit() {
        let dispatch_time = Utc::now() - Duration::hours(1);
        let new_sha = "a".repeat(40);
        let mut new_commits = HashSet::new();
        new_commits.insert(new_sha.clone());

        let hits = select_bypassed_events(
            &[event(dispatch_time + Duration::minutes(5), &new_sha)],
            Some(dispatch_time),
            &new_commits,
        );

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].commit_sha, new_sha);
    }

    #[test]
    fn skips_entries_recorded_before_the_snapshot_timestamp() {
        let dispatch_time = Utc::now() - Duration::hours(1);
        let sha = "b".repeat(40);
        let mut new_commits = HashSet::new();
        new_commits.insert(sha.clone());

        let hits = select_bypassed_events(
            &[event(dispatch_time - Duration::minutes(5), &sha)],
            Some(dispatch_time),
            &new_commits,
        );

        assert!(
            hits.is_empty(),
            "an entry older than the snapshot is not this dispatch's"
        );
    }

    #[test]
    fn skips_entries_whose_commit_predates_the_dispatch() {
        let dispatch_time = Utc::now() - Duration::hours(1);
        let old_sha = "c".repeat(40);
        let new_sha = "d".repeat(40);
        let mut new_commits = HashSet::new();
        new_commits.insert(new_sha.clone());

        let hits = select_bypassed_events(
            &[
                event(dispatch_time + Duration::minutes(5), &old_sha),
                event(dispatch_time + Duration::minutes(6), &new_sha),
            ],
            Some(dispatch_time),
            &new_commits,
        );

        assert_eq!(hits.len(), 1, "only the new commit's bypass is selected");
        assert_eq!(hits[0].commit_sha, new_sha);
    }

    #[test]
    fn skips_legacy_rows_without_a_commit_sha() {
        let dispatch_time = Utc::now() - Duration::hours(1);
        let legacy = BypassEvent {
            timestamp: dispatch_time + Duration::minutes(5),
            commit_sha: String::new(),
            pattern: String::new(),
        };
        let mut new_commits = HashSet::new();
        new_commits.insert("e".repeat(40));

        let hits = select_bypassed_events(&[legacy], Some(dispatch_time), &new_commits);

        assert!(
            hits.is_empty(),
            "a legacy row names no commit, so it fails nothing"
        );
    }

    #[test]
    fn matches_shas_case_insensitively() {
        let dispatch_time = Utc::now() - Duration::hours(1);
        let mut new_commits = HashSet::new();
        new_commits.insert("f".repeat(40));

        let hits = select_bypassed_events(
            &[event(dispatch_time + Duration::minutes(5), &"F".repeat(40))],
            Some(dispatch_time),
            &new_commits,
        );

        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn no_time_baseline_admits_every_entry() {
        let sha = "1".repeat(40);
        let mut new_commits = HashSet::new();
        new_commits.insert(sha.clone());

        let hits = select_bypassed_events(
            &[event(Utc::now() - Duration::days(30), &sha)],
            None,
            &new_commits,
        );

        assert_eq!(
            hits.len(),
            1,
            "legacy snapshots have no timestamp to filter on"
        );
    }

    // ── reason text ──

    #[test]
    fn failure_reason_names_the_gate_and_carries_the_shas() {
        let sha_a = "2".repeat(40);
        let sha_b = "3".repeat(40);
        let reason = failure_reason(&[event(Utc::now(), &sha_a), event(Utc::now(), &sha_b)]);

        assert!(
            reason.starts_with(GATE_NAME),
            "reason must start with dod_bypass: {reason}"
        );
        assert!(
            reason.contains(&sha_a),
            "reason must carry the full sha {sha_a}"
        );
        assert!(
            reason.contains(&sha_b),
            "reason must carry the full sha {sha_b}"
        );
    }

    // ── end to end against a real repo and a synthetic log ──

    #[tokio::test]
    async fn fails_when_a_commit_since_dispatch_bypassed_the_hook() {
        let dir = TempDir::new().unwrap();
        let pre = init_repo(dir.path()).await;
        let new_sha = commit_file(dir.path(), "src.rs", "work without the hook").await;
        let dispatch_time = Utc::now() - Duration::hours(1);
        write_log(
            dir.path(),
            &[event_json(
                dispatch_time + Duration::minutes(5),
                &new_sha,
                "--no-verify",
            )],
        );

        let result = check_dod_bypass(dir.path(), Some(&snapshot(Some(&pre), Some(dispatch_time))))
            .await
            .unwrap();

        let reason = shas(&result);
        assert!(
            reason.contains(&new_sha),
            "reason must name the bypassed commit: {reason}"
        );
        assert!(reason.contains(GATE_NAME));
    }

    #[tokio::test]
    async fn passes_when_the_bypassed_commit_predates_the_dispatch() {
        let dir = TempDir::new().unwrap();
        let pre = init_repo(dir.path()).await;
        let dispatch_time = Utc::now() - Duration::hours(1);
        // The pre-dispatch commit is reachable from HEAD, but it is also
        // reachable from the pre-dispatch sha — it is not this dispatch's work.
        write_log(
            dir.path(),
            &[event_json(
                dispatch_time + Duration::minutes(5),
                &pre,
                "--no-verify",
            )],
        );

        let result = check_dod_bypass(dir.path(), Some(&snapshot(Some(&pre), Some(dispatch_time))))
            .await
            .unwrap();

        assert_eq!(result, GateResult::Pass);
    }

    #[tokio::test]
    async fn passes_when_the_bypass_entry_predates_the_snapshot() {
        let dir = TempDir::new().unwrap();
        let pre = init_repo(dir.path()).await;
        let new_sha = commit_file(dir.path(), "src.rs", "work").await;
        let dispatch_time = Utc::now() - Duration::hours(1);
        // Commit is new since dispatch, but the logged bypass is older than the
        // snapshot — someone else's history, replayed onto this dispatch.
        write_log(
            dir.path(),
            &[event_json(
                dispatch_time - Duration::hours(2),
                &new_sha,
                "--no-verify",
            )],
        );

        let result = check_dod_bypass(dir.path(), Some(&snapshot(Some(&pre), Some(dispatch_time))))
            .await
            .unwrap();

        assert_eq!(result, GateResult::Pass);
    }

    #[tokio::test]
    async fn reports_every_bypassed_commit() {
        let dir = TempDir::new().unwrap();
        let pre = init_repo(dir.path()).await;
        let sha_a = commit_file(dir.path(), "a.rs", "first bypass").await;
        let sha_b = commit_file(dir.path(), "b.rs", "second bypass").await;
        let dispatch_time = Utc::now() - Duration::hours(1);
        write_log(
            dir.path(),
            &[
                event_json(dispatch_time + Duration::minutes(5), &sha_a, "--no-verify"),
                event_json(
                    dispatch_time + Duration::minutes(6),
                    &sha_b,
                    "SKIP_CHECKS=1",
                ),
            ],
        );

        let result = check_dod_bypass(dir.path(), Some(&snapshot(Some(&pre), Some(dispatch_time))))
            .await
            .unwrap();

        let reason = shas(&result);
        assert!(
            reason.contains(&sha_a) && reason.contains(&sha_b),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn passes_when_no_bypass_log_exists() {
        let dir = TempDir::new().unwrap();
        let pre = init_repo(dir.path()).await;
        commit_file(dir.path(), "src.rs", "work with the hook on").await;

        let result = check_dod_bypass(
            dir.path(),
            Some(&snapshot(Some(&pre), Some(Utc::now() - Duration::hours(1)))),
        )
        .await
        .unwrap();

        assert_eq!(result, GateResult::Pass);
    }

    #[tokio::test]
    async fn legacy_rows_alone_pass() {
        let dir = TempDir::new().unwrap();
        let pre = init_repo(dir.path()).await;
        let dispatch_time = Utc::now() - Duration::hours(1);
        // The shape AGENTS.md documents: the log also holds a legacy
        // `{timestamp, lane, pwd}` row that is not a bypass.
        write_log(
            dir.path(),
            &[
                r#"{"timestamp":"2026-08-17T14:32:46Z","lane":"fast","pwd":"/tmp/test"}"#
                    .to_string(),
            ],
        );

        let result = check_dod_bypass(dir.path(), Some(&snapshot(Some(&pre), Some(dispatch_time))))
            .await
            .unwrap();

        assert_eq!(result, GateResult::Pass);
    }

    #[tokio::test]
    async fn unparseable_lines_do_not_blind_the_gate() {
        let dir = TempDir::new().unwrap();
        let pre = init_repo(dir.path()).await;
        let new_sha = commit_file(dir.path(), "src.rs", "work").await;
        let dispatch_time = Utc::now() - Duration::hours(1);
        write_log(
            dir.path(),
            &[
                "not json at all".to_string(),
                event_json(
                    dispatch_time + Duration::minutes(5),
                    &new_sha,
                    "--no-verify",
                ),
            ],
        );

        let result = check_dod_bypass(dir.path(), Some(&snapshot(Some(&pre), Some(dispatch_time))))
            .await
            .unwrap();

        assert!(shas(&result).contains(&new_sha));
    }

    #[tokio::test]
    async fn no_snapshot_passes() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let new_sha = commit_file(dir.path(), "src.rs", "work").await;
        write_log(
            dir.path(),
            &[event_json(Utc::now(), &new_sha, "--no-verify")],
        );

        let result = check_dod_bypass(dir.path(), None).await.unwrap();

        assert_eq!(result, GateResult::Pass);
    }

    #[tokio::test]
    async fn without_a_recorded_head_the_timestamp_filter_decides() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let new_sha = commit_file(dir.path(), "src.rs", "work").await;
        let dispatch_time = Utc::now() - Duration::hours(1);

        // Snapshot had no HEAD (not a git repo at dispatch time): the bypass is
        // recent enough to be attributable.
        write_log(
            dir.path(),
            &[event_json(
                dispatch_time + Duration::minutes(5),
                &new_sha,
                "--no-verify",
            )],
        );
        let result = check_dod_bypass(dir.path(), Some(&snapshot(None, Some(dispatch_time))))
            .await
            .unwrap();
        assert!(shas(&result).contains(&new_sha));

        // A bypass logged long before the snapshot is not.
        write_log(
            dir.path(),
            &[event_json(
                dispatch_time - Duration::hours(2),
                &new_sha,
                "--no-verify",
            )],
        );
        let result = check_dod_bypass(dir.path(), Some(&snapshot(None, Some(dispatch_time))))
            .await
            .unwrap();
        assert_eq!(result, GateResult::Pass);
    }

    #[tokio::test]
    async fn fails_open_when_the_workspace_has_no_git_repo() {
        let dir = TempDir::new().unwrap();
        let dispatch_time = Utc::now() - Duration::hours(1);
        write_log(
            dir.path(),
            &[event_json(
                dispatch_time + Duration::minutes(5),
                &"4".repeat(40),
                "--no-verify",
            )],
        );
        let snap = snapshot(Some(&"5".repeat(40)), Some(dispatch_time));

        let result = check_dod_bypass(dir.path(), Some(&snap)).await.unwrap();

        assert_eq!(
            result,
            GateResult::Pass,
            "git failure must not fail closures"
        );
    }

    #[tokio::test]
    async fn passes_when_head_equals_the_pre_dispatch_sha() {
        let dir = TempDir::new().unwrap();
        let pre = init_repo(dir.path()).await;
        let dispatch_time = Utc::now() - Duration::hours(1);
        write_log(
            dir.path(),
            &[event_json(
                dispatch_time + Duration::minutes(5),
                &"6".repeat(40),
                "--no-verify",
            )],
        );

        let result = check_dod_bypass(dir.path(), Some(&snapshot(Some(&pre), Some(dispatch_time))))
            .await
            .unwrap();

        assert_eq!(
            result,
            GateResult::Pass,
            "no new commits means nothing to attribute"
        );
    }
}
