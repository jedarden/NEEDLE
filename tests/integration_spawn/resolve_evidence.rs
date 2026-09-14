//! Real-Git contracts for the bounded post-Pluck evidence bundle.
//!
//! These belong in the process integration harness: production capture invokes
//! read-only Git commands, while the library suite keeps only pure parser and
//! renderer contracts.

use needle::attempt_history::{self, AttemptRecord, SCHEMA_VERSION};
use needle::resolve::evidence::{
    capture_in, render, MAX_COMMITS, MAX_DIRTY_PATHS, MAX_OUTPUT_TAIL_BYTES, MAX_RENDER_BYTES,
    MAX_TRACE_TAIL_BYTES,
};
use needle::types::{Bead, BeadId, BeadStatus};
use needle::validation::predispatch::{self, DirtyFile, PreDispatch};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const BEAD_ID: &str = "needle-evidence-test";
const DEFAULT_BODY: &str =
    "Do the thing.\n\n## Acceptance criteria\n\n- clean case covered\n- dirty case covered";

fn git_out(workspace: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn git(workspace: &Path, args: &[&str]) {
    git_out(workspace, args);
}

fn committed_repo() -> (TempDir, PathBuf, String) {
    let dir = TempDir::new().expect("tempdir");
    let workspace = dir.path().to_path_buf();
    git(&workspace, &["init", "-q"]);
    git(&workspace, &["config", "user.email", "t@example.com"]);
    git(&workspace, &["config", "user.name", "t"]);
    std::fs::write(workspace.join("base.txt"), "base\n").expect("write base");
    git(&workspace, &["add", "base.txt"]);
    git(&workspace, &["commit", "-q", "-m", "base"]);
    let sha = git_out(&workspace, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    (dir, workspace, sha)
}

fn bead_in(workspace: &Path, body: &str) -> Bead {
    Bead {
        id: BeadId::from(BEAD_ID),
        title: "Build bounded evidence bundles".to_string(),
        body: Some(body.to_string()),
        priority: 1,
        status: BeadStatus::InProgress,
        assignee: Some("worker-01".to_string()),
        labels: vec![],
        workspace: workspace.to_path_buf(),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn write_snapshot(root: &Path, workspace: &Path, head: Option<&str>, paths: Vec<String>) {
    let snapshot = PreDispatch {
        head_sha: head.map(str::to_string),
        notes_hash: None,
        dirty_files: paths
            .into_iter()
            .map(|path| DirtyFile {
                path,
                blob_hash: "0123456789abcdef".to_string(),
            })
            .collect(),
        captured_at: Some(chrono::Utc::now()),
    };
    let path = predispatch::snapshot_path_in(root, workspace, &BeadId::from(BEAD_ID));
    std::fs::create_dir_all(path.parent().expect("snapshot parent")).expect("mkdir snapshot");
    std::fs::write(
        path,
        serde_json::to_vec(&snapshot).expect("serialize snapshot"),
    )
    .expect("write snapshot");
}

fn attempt_record(summary: Option<String>) -> AttemptRecord {
    AttemptRecord {
        schema_version: SCHEMA_VERSION,
        attempt_id: "att-timeout".to_string(),
        recorded_at: "2026-09-13T10:00:00.000Z".to_string(),
        worker: "worker-01".to_string(),
        adapter: "test-adapter".to_string(),
        model: None,
        outcome: "work_failure".to_string(),
        terminal_reason: Some("timeout".to_string()),
        exit_code: 1,
        requested_action: "Released".to_string(),
        commits: vec!["abcdef1234567890".to_string()],
        duration_ms: 1000,
        failure_summary: summary,
    }
}

#[tokio::test]
async fn clean_no_commit_capture_is_read_only() {
    let (_dir, workspace, base) = committed_repo();
    let root = TempDir::new().expect("state root");
    write_snapshot(root.path(), &workspace, Some(&base), vec![]);
    let bead = bead_in(&workspace, DEFAULT_BODY);
    let status_before = git_out(&workspace, &["status", "--porcelain=v1"]);
    let base_before = std::fs::read(workspace.join("base.txt")).expect("read base");

    let bundle = capture_in(root.path(), &workspace, &bead, 0, "done", "", false).await;

    assert_eq!(
        git_out(&workspace, &["status", "--porcelain=v1"]),
        status_before
    );
    assert_eq!(
        std::fs::read(workspace.join("base.txt")).expect("read base"),
        base_before
    );
    assert!(bundle.git.pre_dispatch.as_ref().expect("pre").clean);
    assert!(bundle.git.post_dispatch.clean);
    assert!(bundle.commits.is_empty());
    assert!(bundle.diff_summary.is_none());
    let rendered = render(&bundle);
    assert!(rendered.contains("No commits were made"), "{rendered}");
    assert!(rendered.contains("clean case covered"), "{rendered}");
}

#[tokio::test]
async fn dirty_workspace_paths_are_bounded() {
    let (_dir, workspace, base) = committed_repo();
    let root = TempDir::new().expect("state root");
    let preexisting = (0..=MAX_DIRTY_PATHS)
        .map(|i| format!("preexisting-{i}.rs"))
        .collect();
    write_snapshot(root.path(), &workspace, Some(&base), preexisting);
    std::fs::write(workspace.join("dirty.rs"), "fn dirty() {}\n").expect("write dirty");
    std::fs::write(workspace.join("base.txt"), "base changed\n").expect("edit tracked file");

    let bundle = capture_in(
        root.path(),
        &workspace,
        &bead_in(&workspace, DEFAULT_BODY),
        1,
        "",
        "tests failed",
        false,
    )
    .await;

    let pre = bundle.git.pre_dispatch.as_ref().expect("pre");
    assert_eq!(pre.dirty_paths.len(), MAX_DIRTY_PATHS);
    assert_eq!(pre.dirty_paths_omitted, 1);
    assert!(bundle
        .git
        .post_dispatch
        .dirty_paths
        .iter()
        .any(|path| path == "dirty.rs"));
    let diff = bundle.diff_summary.as_ref().expect("worktree diff");
    assert_eq!(diff.files_changed, 1);
    assert_eq!(diff.insertions, 1);
    assert_eq!(diff.deletions, 1);
    let rendered = render(&bundle);
    assert!(rendered.contains("+1 more"), "{rendered}");
    assert!(rendered.contains("dirty.rs"), "{rendered}");
}

#[tokio::test]
async fn timeout_history_and_no_commit_reason_are_rendered() {
    let (_dir, workspace, base) = committed_repo();
    let root = TempDir::new().expect("state root");
    write_snapshot(root.path(), &workspace, Some(&base), vec![]);
    attempt_history::append_local(
        &workspace,
        &BeadId::from(BEAD_ID),
        &attempt_record(Some("killed at 30m".to_string())),
    )
    .expect("append history");

    let bundle = capture_in(
        root.path(),
        &workspace,
        &bead_in(&workspace, DEFAULT_BODY),
        3,
        "",
        "agent gave up",
        false,
    )
    .await;

    assert_eq!(bundle.history_total, 1);
    assert_eq!(
        bundle.validation[0].terminal_reason.as_deref(),
        Some("timeout")
    );
    let rendered = render(&bundle);
    assert!(rendered.contains("killed at 30m"), "{rendered}");
    assert!(rendered.contains("No commits were made"), "{rendered}");
    assert!(rendered.contains("reason: `exit_code:3`"), "{rendered}");
}

#[tokio::test]
async fn commit_history_and_diff_are_capped_from_the_baseline() {
    let (_dir, workspace, base) = committed_repo();
    let root = TempDir::new().expect("state root");
    write_snapshot(root.path(), &workspace, Some(&base), vec![]);
    for i in 0..MAX_COMMITS + 2 {
        std::fs::write(workspace.join("work.txt"), format!("{i}\n")).expect("write work");
        git(&workspace, &["add", "work.txt"]);
        git(
            &workspace,
            &["commit", "-q", "-m", &format!("evidence commit {i}")],
        );
    }

    let bundle = capture_in(
        root.path(),
        &workspace,
        &bead_in(&workspace, DEFAULT_BODY),
        0,
        "done",
        "",
        false,
    )
    .await;

    assert_eq!(bundle.commits.len(), MAX_COMMITS);
    assert_eq!(bundle.commits_omitted, 2);
    assert_eq!(
        bundle.commits.last().expect("latest commit").subject,
        "evidence commit 11"
    );
    let diff = bundle.diff_summary.expect("diff summary");
    assert_eq!(diff.files_changed, 1);
    assert_eq!(diff.insertions, 1);
    assert_eq!(diff.deletions, 0);
}

#[tokio::test]
async fn oversized_secret_bearing_fields_are_redacted_and_bounded() {
    let (_dir, workspace, _base) = committed_repo();
    let root = TempDir::new().expect("state root");
    let secret = [
        "AI", "za", "SyBn", "Fb9R", "kQ3m", "D2eW", "l8Tp", "Xa0v", "N7hJ", "cK4o", "MiY",
    ]
    .concat();
    let secret_line = format!("key = \"{secret}\"");
    write_snapshot(
        root.path(),
        &workspace,
        Some(&secret),
        vec![secret_line.clone()],
    );
    let mut record = attempt_record(Some(secret_line.clone()));
    record.adapter = secret.clone();
    record.outcome = secret.clone();
    record.terminal_reason = Some(secret.clone());
    record.commits = vec![secret.clone()];
    attempt_history::append_local(&workspace, &BeadId::from(BEAD_ID), &record)
        .expect("append history");
    let trace_dir = workspace.join(".beads").join("traces").join(BEAD_ID);
    std::fs::create_dir_all(&trace_dir).expect("mkdir traces");
    let mut trace = String::new();
    for i in 0..200 {
        trace.push_str(&format!(
            "{{\"line\":{i},\"pad\":\"{}\"}}\n",
            "p".repeat(40)
        ));
    }
    trace.push_str(&secret_line);
    trace.push('\n');
    std::fs::write(trace_dir.join("trace.jsonl"), trace).expect("write trace");
    let stdout = format!(
        "HEAD-MARKER\n{}\n{secret_line}\nTAIL-MARKER",
        "x".repeat(100_000)
    );

    let bundle = capture_in(
        root.path(),
        &workspace,
        &bead_in(&workspace, DEFAULT_BODY),
        1,
        &stdout,
        &secret_line,
        false,
    )
    .await;

    assert!(bundle.dispatch.stdout_tail.len() <= MAX_OUTPUT_TAIL_BYTES);
    assert!(bundle.dispatch.stdout_tail.contains("[elided]"));
    assert!(bundle
        .trace_tail
        .as_deref()
        .is_some_and(|tail| tail.len() <= MAX_TRACE_TAIL_BYTES));
    let rendered = render(&bundle);
    assert!(rendered.len() <= MAX_RENDER_BYTES);
    assert!(!rendered.contains(&secret), "secret leaked: {rendered}");
    assert!(rendered.contains("[REDACTED:"), "{rendered}");
    assert!(bundle.git.pre_dispatch.as_ref().expect("pre").dirty_paths[0].contains("[REDACTED:"));
    assert!(bundle.validation[0].outcome.contains("[REDACTED:"));
    assert!(bundle.failure_history[0].adapter.contains("[REDACTED:"));
    assert!(bundle.failure_history[0].commits[0].contains("[REDACTED:"));
}

#[tokio::test]
async fn missing_workspace_degrades_to_unknown() {
    let root = TempDir::new().expect("state root");
    let workspace = root.path().join("missing");
    let bundle = capture_in(
        root.path(),
        &workspace,
        &bead_in(&workspace, DEFAULT_BODY),
        0,
        "out",
        "",
        false,
    )
    .await;

    assert_eq!(bundle.git.post_dispatch.head_sha, None);
    assert!(bundle.git.pre_dispatch.is_none());
    assert!(bundle.commits.is_empty());
    assert!(bundle.diff_summary.is_none());
    assert!(bundle.trace_tail.is_none());
    let rendered = render(&bundle);
    assert!(rendered.contains("Commits unknown"), "{rendered}");
}
