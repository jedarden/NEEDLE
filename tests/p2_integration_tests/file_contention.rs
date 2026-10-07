//! Phase 20 file-contention release evidence (needle-4386d7bd).
//!
//! Drives the real `needle contention` binary and the worker-side session
//! API against real git checkouts. Every subprocess runs with a cleared
//! environment, an isolated temporary HOME, and only PATH carried over, so no
//! operator state, fleet heartbeat, or inherited NEEDLE_* contract can leak
//! in (AGENTS.md test-isolation policy).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use needle::config::FILE_CONTENTION_HOOK_CAPABILITY;
use needle::file_contention::git_safety;
use needle::file_contention::session::{prepare, DispatchIdentity, Preparation};
use serde_json::Value;

/// Larger than the kernel's PID_MAX_LIMIT (4 * 1024 * 1024): never alive.
const DEAD_PID: u32 = 4 * 1024 * 1024;

struct Checkout {
    _home: tempfile::TempDir,
    home: PathBuf,
    repo: tempfile::TempDir,
}

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run git")
}

fn git_ok(repo: &Path, args: &[&str]) -> String {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// An isolated HOME and a committed checkout, opted in when `enabled`.
fn checkout(enabled: bool) -> Checkout {
    let home = tempfile::tempdir().expect("isolated HOME");
    let repo = tempfile::tempdir().expect("checkout");
    git_ok(repo.path(), &["init", "-q", "-b", "main"]);
    git_ok(repo.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    for file in ["src/lib.rs", "src/a.rs", "src/b.rs", "src/other.rs"] {
        std::fs::write(repo.path().join(file), format!("// {file}\n")).unwrap();
    }
    if enabled {
        std::fs::write(
            repo.path().join(".needle.yaml"),
            "file_contention:\n  enabled: true\n  lease_secs: 60\n",
        )
        .unwrap();
    }
    git_ok(repo.path(), &["add", "-A"]);
    git_ok(repo.path(), &["commit", "-q", "-m", "initial"]);
    Checkout {
        home: home.path().to_path_buf(),
        _home: home,
        repo,
    }
}

/// One participant: the env a dispatch (or an interactive session) hands
/// its hook.
struct Who {
    env: Vec<(&'static str, String)>,
    pid: u32,
}

fn worker(name: &str, pid: u32) -> Who {
    Who {
        env: vec![
            ("NEEDLE_BEAD_ID", format!("needle-{name}")),
            ("NEEDLE_ATTEMPT_ID", format!("attempt-{name}")),
            ("NEEDLE_WORKER_ID", name.to_string()),
        ],
        pid,
    }
}

fn session(id: &str) -> Who {
    Who {
        env: vec![("NEEDLE_SESSION_ID", id.to_string())],
        pid: std::process::id(),
    }
}

/// Run `needle contention <args> --repo <repo> --json` as `who`.
fn contention(co: &Checkout, who: &Who, args: &[&str]) -> (i32, Value) {
    contention_in(co, co.repo.path(), who, args)
}

fn contention_in(co: &Checkout, repo: &Path, who: &Who, args: &[&str]) -> (i32, Value) {
    // Cargo embeds an absolute path for direct runs; a nextest archive
    // relocates the binary and publishes its runtime path instead.
    let needle_binary = std::env::var_os("NEXTEST_BIN_EXE_needle")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_needle")));
    let mut command = Command::new(needle_binary);
    command
        .env_clear()
        .env("HOME", &co.home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .arg("contention")
        .args(args)
        .arg("--repo")
        .arg(repo)
        .arg("--pid")
        .arg(who.pid.to_string())
        .arg("--json");
    for (key, value) in &who.env {
        command.env(key, value);
    }
    let out = command.output().expect("run needle contention");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "non-JSON output {stdout:?} ({e}); stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code().expect("exit code"), value)
}

fn marker(repo: &Path, rel: &str) -> PathBuf {
    repo.join(".needle/locks").join(format!("{rel}.lock"))
}

fn marker_holder(repo: &Path, rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(marker(repo, rel)).unwrap()).unwrap()
}

#[test]
fn file_contention_two_workers_contend_while_disjoint_writes_coexist() {
    let co = checkout(true);
    let alpha = worker("alpha", std::process::id());
    let bravo = worker("bravo", std::process::id());

    let (code, out) = contention(&co, &alpha, &["acquire", "--path", "src/lib.rs"]);
    assert_eq!(code, 0, "{out}");

    // Same file: bravo is told who holds it and writes nothing.
    let (code, out) = contention(&co, &bravo, &["acquire", "--path", "src/lib.rs"]);
    assert_eq!(code, 3, "{out}");
    assert_eq!(out["conflicts"][0]["assessment"], "active_other");
    assert_eq!(out["conflicts"][0]["holder"]["bead_id"], "needle-alpha");
    assert_eq!(
        marker_holder(co.repo.path(), "src/lib.rs")["worker_id"],
        "alpha"
    );

    // A different file needs no bead path declaration: it just proceeds.
    let (code, out) = contention(&co, &bravo, &["acquire", "--path", "src/other.rs"]);
    assert_eq!(code, 0, "{out}");

    let (code, out) = contention(&co, &bravo, &["list"]);
    assert_eq!(code, 0);
    let mut held: Vec<(String, String)> = out["markers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["path"].as_str().unwrap().to_string(),
                m["holder"]["worker_id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    held.sort();
    assert_eq!(
        held,
        vec![
            ("src/lib.rs".to_string(), "alpha".to_string()),
            ("src/other.rs".to_string(), "bravo".to_string()),
        ]
    );
}

#[test]
fn file_contention_interactive_session_rename_and_multi_path_are_all_or_none() {
    let co = checkout(true);
    let alpha = worker("alpha", std::process::id());
    let me = session("claude-interactive-1");

    let (code, out) = contention(
        &co,
        &me,
        &[
            "acquire",
            "--rename-from",
            "src/a.rs",
            "--rename-to",
            "src/renamed.rs",
        ],
    );
    assert_eq!(code, 0, "{out}");
    let from = marker_holder(co.repo.path(), "src/a.rs");
    let to = marker_holder(co.repo.path(), "src/renamed.rs");
    assert_eq!(from["intent"], "rename_from");
    assert_eq!(to["intent"], "rename_to");
    assert_eq!(from["session_id"], "claude-interactive-1");
    assert_eq!(from["worker_id"], "interactive");
    assert!(from["bead_id"].is_null());

    // Multi-path where one path is held: nothing is recorded for any path.
    assert_eq!(
        contention(&co, &alpha, &["acquire", "--path", "src/b.rs"]).0,
        0
    );
    let (code, out) = contention(
        &co,
        &me,
        &["acquire", "--path", "src/lib.rs", "--path", "src/b.rs"],
    );
    assert_eq!(code, 3, "{out}");
    assert!(!marker(co.repo.path(), "src/lib.rs").exists());

    // The session releases only its own markers.
    let (code, out) = contention(&co, &me, &["release"]);
    assert_eq!(code, 0);
    assert_eq!(out["released"].as_array().unwrap().len(), 2);
    assert!(marker(co.repo.path(), "src/b.rs").exists());
}

#[test]
fn file_contention_stale_unchanged_is_taken_over_and_stale_modified_preserved() {
    let co = checkout(true);
    let ghost = worker("ghost", DEAD_PID);
    let bravo = worker("bravo", std::process::id());

    assert_eq!(
        contention(&co, &ghost, &["acquire", "--path", "src/a.rs"]).0,
        0
    );
    assert_eq!(
        contention(&co, &ghost, &["acquire", "--path", "src/b.rs"]).0,
        0
    );
    std::fs::write(co.repo.path().join("src/b.rs"), "// half-finished work\n").unwrap();

    // Unchanged file, dead holder: atomically taken over.
    let (code, out) = contention(&co, &bravo, &["acquire", "--path", "src/a.rs"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        marker_holder(co.repo.path(), "src/a.rs")["worker_id"],
        "bravo"
    );

    // Modified file, dead holder: preserved and routed to the owning bead.
    let (code, out) = contention(&co, &bravo, &["acquire", "--path", "src/b.rs"]);
    assert_eq!(code, 4, "{out}");
    let conflict = &out["conflicts"][0];
    assert_eq!(conflict["assessment"], "stale_modified");
    assert!(
        conflict["detail"]
            .as_str()
            .unwrap()
            .contains("resume through bead needle-ghost"),
        "{conflict}"
    );
    assert_eq!(
        marker_holder(co.repo.path(), "src/b.rs")["worker_id"],
        "ghost"
    );
    assert_eq!(
        std::fs::read_to_string(co.repo.path().join("src/b.rs")).unwrap(),
        "// half-finished work\n"
    );
}

#[test]
fn file_contention_dispatch_coverage_and_terminal_cleanup() {
    let co = checkout(true);
    let capable = vec![FILE_CONTENTION_HOOK_CAPABILITY.to_string()];
    let identity = DispatchIdentity {
        bead_id: "needle-delta",
        attempt_id: "attempt-delta",
        worker_id: "delta",
        host: &gethostname(),
        holder_pid: std::process::id(),
    };

    // Unsupported harness in an opted-in checkout: no contract, no tree.
    match prepare(&[], co.repo.path(), &identity) {
        Preparation::Inactive { coverage, .. } => assert_eq!(coverage, "unsupported"),
        Preparation::Active { .. } => panic!("unsupported harness must not be enabled"),
    }
    assert!(!co.repo.path().join(".needle").exists());

    // Capable harness, checkout not opted in: also unchanged.
    let disabled = checkout(false);
    match prepare(&capable, disabled.repo.path(), &identity) {
        Preparation::Inactive { coverage, .. } => assert_eq!(coverage, "supported_disabled"),
        Preparation::Active { .. } => panic!("disabled workspace must not be enabled"),
    }
    assert!(!disabled.repo.path().join(".needle").exists());

    // Enabled: the hook acquires with exactly the dispatch env; the worker's
    // terminal release removes it.
    let (session, env) = match prepare(&capable, co.repo.path(), &identity) {
        Preparation::Active { session, env } => (session, env),
        Preparation::Inactive { coverage, reason } => panic!("{coverage}: {reason:?}"),
    };
    let env: HashMap<String, String> = env.into_iter().collect();
    assert_eq!(env["NEEDLE_FILE_CONTENTION"], "enabled");
    let hook = Who {
        env: ["NEEDLE_BEAD_ID", "NEEDLE_ATTEMPT_ID", "NEEDLE_WORKER_ID"]
            .into_iter()
            .map(|k| (k, env[k].clone()))
            .collect(),
        pid: env["NEEDLE_FILE_CONTENTION_PID"].parse().unwrap(),
    };
    assert_eq!(
        contention(&co, &hook, &["acquire", "--path", "src/lib.rs"]).0,
        0
    );
    assert!(marker(co.repo.path(), "src/lib.rs").exists());

    let released = session.release(None).expect("terminal release");
    assert_eq!(released.released, vec!["src/lib.rs".to_string()]);
    assert!(released.committed_markers.is_empty());
    assert!(!marker(co.repo.path(), "src/lib.rs").exists());
}

#[test]
fn file_contention_separate_checkouts_are_independent() {
    let first = checkout(true);
    let second = checkout(true);
    let alpha = worker("alpha", std::process::id());
    let bravo = worker("bravo", std::process::id());

    assert_eq!(
        contention(&first, &alpha, &["acquire", "--path", "src/lib.rs"]).0,
        0
    );
    let (code, out) = contention_in(
        &first,
        second.repo.path(),
        &bravo,
        &["acquire", "--path", "src/lib.rs"],
    );
    assert_eq!(
        code, 0,
        "another checkout never sees this one's markers: {out}"
    );
    assert_eq!(
        marker_holder(second.repo.path(), "src/lib.rs")["worker_id"],
        "bravo"
    );
    assert_eq!(
        marker_holder(first.repo.path(), "src/lib.rs")["worker_id"],
        "alpha"
    );
}

#[test]
fn file_contention_markers_stay_untracked_even_when_force_added() {
    let co = checkout(true);
    let alpha = worker("alpha", std::process::id());
    assert_eq!(
        contention(&co, &alpha, &["acquire", "--path", "src/lib.rs"]).0,
        0
    );

    // Normal flow: invisible to status and to a blanket add.
    let status = git_ok(
        co.repo.path(),
        &["status", "--porcelain", "--untracked-files=all"],
    );
    assert!(!status.contains(".needle"), "{status}");
    git_ok(co.repo.path(), &["add", "-A"]);
    assert!(git_safety::staged_or_tracked_markers(co.repo.path())
        .unwrap()
        .is_empty());
    assert!(!std::fs::read_to_string(co.repo.path().join(".gitignore"))
        .unwrap_or_default()
        .contains(".needle"));

    // Forced add: detected and rejected by the guard.
    git_ok(
        co.repo.path(),
        &["add", "-f", "--", ".needle/locks/src/lib.rs.lock"],
    );
    let staged = git_safety::staged_or_tracked_markers(co.repo.path()).unwrap();
    assert_eq!(staged, vec![".needle/locks/src/lib.rs.lock".to_string()]);
    assert!(git_safety::guard_no_markers(co.repo.path()).is_err());

    // And if one is committed anyway, the range check names it.
    let base = git_ok(co.repo.path(), &["rev-parse", "HEAD"]);
    git_ok(co.repo.path(), &["commit", "-q", "-m", "forced"]);
    let committed = git_safety::markers_in_range(co.repo.path(), base.trim(), "HEAD").unwrap();
    assert_eq!(committed, vec![".needle/locks/src/lib.rs.lock".to_string()]);
}

/// The host name `needle contention` records, so ownership checks match.
fn gethostname() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}
