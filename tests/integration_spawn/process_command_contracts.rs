//! Real Git, tar, shell, and filesystem command contracts.
//!
//! These cases deliberately execute operating-system processes. Keeping them
//! in the `integration_spawn` binary lets the process-free library gate finish
//! without waiting on host command scheduling.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::ci::{correlate_commit, CorrelationError};
use needle::commit_hook::{inject_bead_id_trailer, validate_commit};
use needle::dispatch::{cleanup_extraction, extract_clean_workspace, ExtractionConfig};
use needle::mitosis::timeout_context::capture_timeout_context;
use needle::mitosis::timeout_eligibility::TimeoutEligibility;
use needle::outcome::{AttemptContext, OutcomeHandler};
use needle::scratch_sweep::{sweep_scratch_directory_with_proc_root, SweepOutcome};
use needle::telemetry::{Telemetry, TelemetryEvent};
use needle::types::{AgentOutcome, Bead, BeadAction, BeadId, BeadStatus, ClaimResult};
use needle::validation::dod_bypass::check_dod_bypass;
use needle::validation::predispatch::{self, DirtyFile, PreDispatch};
use needle::validation::{
    upstream_status, verify_shipped_work, CommandGate, Gate, GateResult, RunIn, UpstreamStatus,
    ValidationGate,
};
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Mutex;
use tempfile::TempDir;

struct HomeGuard {
    previous: Option<OsString>,
}

impl HomeGuard {
    fn set(path: &Path) -> Self {
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
}

struct GitRepo {
    root: TempDir,
}

impl GitRepo {
    fn new() -> Self {
        let root = TempDir::new().expect("create isolated Git fixture");
        git_ok(root.path(), &["init", "-q"]);
        git_ok(
            root.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git_ok(root.path(), &["config", "user.name", "Needle Test"]);
        fs::write(root.path().join("README.md"), "seed\n").expect("write seed");
        git_ok(root.path(), &["add", "README.md"]);
        git_ok(root.path(), &["commit", "-q", "-m", "seed"]);
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn head(&self) -> String {
        git_stdout(self.path(), &["rev-parse", "HEAD"])
    }

    fn commit(&self, path: &str, content: &str, message: &str) -> String {
        let destination = self.path().join(path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(destination, content).expect("write fixture file");
        git_ok(self.path(), &["add", path]);
        git_ok(self.path(), &["commit", "-q", "-m", message]);
        self.head()
    }
}

fn git_output(path: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("execute Git fixture command")
}

fn git_ok(path: &Path, args: &[&str]) {
    let output = git_output(path, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = git_output(path, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git output is UTF-8")
        .trim()
        .to_string()
}

fn test_bead(workspace: &Path, status: BeadStatus) -> Bead {
    Bead {
        id: BeadId::from("needle-process-contract"),
        title: "Process contract".to_string(),
        body: Some("Exercise a real command boundary".to_string()),
        priority: 1,
        status,
        assignee: Some("test-worker".to_string()),
        labels: Vec::new(),
        workspace: workspace.to_path_buf(),
        dependencies: Vec::new(),
        dependents: Vec::new(),
        comments: Vec::new(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn telemetry_events(log_dir: &Path) -> Vec<TelemetryEvent> {
    fs::read_dir(log_dir)
        .expect("read telemetry directory")
        .flat_map(|entry| {
            fs::read_to_string(entry.expect("read telemetry entry").path())
                .expect("read telemetry log")
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("parse telemetry event"))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test]
async fn ci_commit_correlation_requires_exactly_one_bead_trailer() {
    let repo = GitRepo::new();
    let one = repo.commit("one", "one\n", "feat: one\n\nBead-Id: parent");
    assert_eq!(
        correlate_commit(repo.path(), &one)
            .await
            .expect("correlate one"),
        BeadId::from("parent")
    );

    let ambiguous = repo.commit(
        "two",
        "two\n",
        "feat: two\n\nBead-Id: first\nBead-Id: second",
    );
    assert!(matches!(
        correlate_commit(repo.path(), &ambiguous).await,
        Err(CorrelationError::AmbiguousTrailers { .. })
    ));

    let missing = repo.commit("three", "three\n", "feat: three");
    assert!(matches!(
        correlate_commit(repo.path(), &missing).await,
        Err(CorrelationError::MissingTrailer { .. })
    ));
}

#[tokio::test]
async fn commit_hook_tags_only_the_matching_unpublished_head() {
    let repo = GitRepo::new();
    let base = repo.head();
    repo.commit("work.rs", "fn work() {}\n", "feat(needle-match): real work");

    inject_bead_id_trailer(repo.path(), &BeadId::from("needle-other"), &base)
        .await
        .expect("mismatched injection is a safe no-op");
    assert!(!git_stdout(
        repo.path(),
        &["log", "-1", "--format=%(trailers:key=Bead-Id,valueonly)"]
    )
    .contains("needle-other"));

    inject_bead_id_trailer(repo.path(), &BeadId::from("needle-match"), &base)
        .await
        .expect("matching injection succeeds");
    assert!(git_stdout(
        repo.path(),
        &["log", "-1", "--format=%(trailers:key=Bead-Id,valueonly)"]
    )
    .contains("needle-match"));
}

#[serial_test::serial]
#[tokio::test]
async fn commit_validation_rejects_an_unchanged_foreign_staged_blob() {
    let home = TempDir::new().expect("create isolated HOME");
    let _home = HomeGuard::set(home.path());

    let repo = GitRepo::new();
    fs::write(repo.path().join("foreign.txt"), "another worker\n").unwrap();
    let blob = git_stdout(repo.path(), &["hash-object", "foreign.txt"]);
    let bead_id = BeadId::from("needle-foreign");
    let snapshot = PreDispatch {
        head_sha: Some(repo.head()),
        notes_hash: None,
        dirty_files: vec![DirtyFile {
            path: "foreign.txt".to_string(),
            blob_hash: blob,
        }],
        captured_at: Some(Utc::now()),
    };
    let snapshot_path = predispatch::snapshot_path(repo.path(), &bead_id);
    fs::create_dir_all(snapshot_path.parent().unwrap()).unwrap();
    fs::write(snapshot_path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    git_ok(repo.path(), &["add", "foreign.txt"]);

    let error = validate_commit(repo.path(), &bead_id)
        .await
        .expect_err("unchanged foreign blob must be rejected");
    assert!(error.to_string().contains("foreign.txt"));
}

#[tokio::test]
async fn clean_archive_extracts_only_committed_state_and_cleans_up() {
    let repo = GitRepo::new();
    let head = repo.commit("tracked.txt", "committed\n", "add tracked");
    fs::write(repo.path().join("untracked.txt"), "not archived\n").unwrap();
    let scratch = TempDir::new().expect("create scratch root");
    let result = extract_clean_workspace(
        repo.path(),
        ExtractionConfig::new("test-worker".to_string(), "needle-archive".to_string())
            .with_scratch_base(scratch.path().to_path_buf()),
    )
    .await
    .expect("extract committed tree");

    assert_eq!(result.extracted_head_sha, head);
    assert_eq!(
        fs::read_to_string(result.extraction_path.join("tracked.txt")).unwrap(),
        "committed\n"
    );
    assert!(!result.extraction_path.join("untracked.txt").exists());
    cleanup_extraction(&result.extraction_path)
        .await
        .expect("remove extraction");
    assert!(!result.extraction_path.exists());
}

#[tokio::test]
async fn timeout_context_reports_modified_but_not_untracked_paths() {
    let repo = GitRepo::new();
    fs::write(repo.path().join("README.md"), "modified\n").unwrap();
    fs::write(repo.path().join("untracked.txt"), "untracked\n").unwrap();
    let bead = test_bead(repo.path(), BeadStatus::InProgress);

    let context = capture_timeout_context(
        &bead,
        repo.path(),
        TimeoutEligibility::Eligible {
            reason: "agent wall-clock timeout".to_string(),
        },
        3600,
    )
    .await
    .expect("capture timeout context")
    .expect("context is available");

    assert!(context
        .git_state
        .post_attempt
        .dirty_paths
        .iter()
        .any(|path| path == "README.md"));
    assert!(!context
        .git_state
        .post_attempt
        .dirty_paths
        .iter()
        .any(|path| path == "untracked.txt"));
}

#[test]
fn scratch_sweep_removes_only_a_clean_pushed_clone() {
    use filetime::{set_file_mtime, FileTime};
    use std::time::{Duration, SystemTime};

    let root = TempDir::new().expect("create isolated scratch root");
    let proc_root = TempDir::new().expect("create isolated procfs root");
    let remote = root.path().join("origin.git");
    let seed = root.path().join("seed");
    fs::create_dir(&remote).unwrap();
    git_ok(&remote, &["init", "-q", "--bare"]);
    fs::create_dir(&seed).unwrap();
    git_ok(&seed, &["init", "-q"]);
    git_ok(&seed, &["config", "user.email", "test@example.invalid"]);
    git_ok(&seed, &["config", "user.name", "Needle Test"]);
    fs::write(seed.join("tracked.txt"), "seed\n").unwrap();
    git_ok(&seed, &["add", "tracked.txt"]);
    git_ok(&seed, &["commit", "-q", "-m", "seed"]);
    git_ok(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git_ok(&seed, &["push", "-q", "-u", "origin", "HEAD"]);

    let clean = root.path().join("needle-clean.contract");
    let unpushed = root.path().join("seam-unpushed.contract");
    let clone = |destination: &Path| {
        let output = Command::new("git")
            .arg("clone")
            .arg("-q")
            .arg(&remote)
            .arg(destination)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("clone fixture");
        assert!(output.status.success());
    };
    clone(&clean);
    clone(&unpushed);
    git_ok(&unpushed, &["config", "user.email", "test@example.invalid"]);
    git_ok(&unpushed, &["config", "user.name", "Needle Test"]);
    fs::write(unpushed.join("local.txt"), "local\n").unwrap();
    git_ok(&unpushed, &["add", "local.txt"]);
    git_ok(&unpushed, &["commit", "-q", "-m", "unpushed"]);

    let old = FileTime::from_system_time(SystemTime::now() - Duration::from_secs(2 * 60 * 60));
    set_file_mtime(&clean, old).unwrap();
    set_file_mtime(&unpushed, old).unwrap();

    let report = match sweep_scratch_directory_with_proc_root(root.path(), 1, proc_root.path())
        .expect("sweep scratch")
    {
        SweepOutcome::Completed(report) => report,
        other => panic!("expected a completed sweep, got {other:?}"),
    };
    assert!(
        !clean.exists(),
        "clean pushed clone should be removed: {report:?}"
    );
    assert!(unpushed.exists(), "unpushed commit must be preserved");
    assert_eq!(report.removed.len(), 1);
}

#[tokio::test]
async fn dod_bypass_attributes_only_new_reachable_commits() {
    let repo = GitRepo::new();
    let pre = repo.head();
    let bypassed = repo.commit("src.rs", "fn shipped() {}\n", "bypassed work");
    let captured_at = Utc::now() - ChronoDuration::minutes(10);
    fs::create_dir_all(repo.path().join(".beads")).unwrap();
    fs::write(
        repo.path().join(".beads/bypasses.jsonl"),
        format!(
            "{{\"timestamp\":\"{}\",\"commit_sha\":\"{}\",\"pattern\":\"--no-verify\"}}\n",
            (captured_at + ChronoDuration::minutes(1)).to_rfc3339(),
            bypassed
        ),
    )
    .unwrap();
    let snapshot = PreDispatch {
        head_sha: Some(pre),
        notes_hash: None,
        dirty_files: Vec::new(),
        captured_at: Some(captured_at),
    };

    let result = check_dod_bypass(repo.path(), Some(&snapshot))
        .await
        .expect("evaluate bypass log");
    match result {
        GateResult::Fail(reason) => assert!(reason.contains(&bypassed)),
        other => panic!("expected bypass failure, got {other:?}"),
    }
}

#[tokio::test]
async fn command_gate_routes_success_failure_and_stderr_caps() {
    let workspace = TempDir::new().expect("create command workspace");
    let bead = test_bead(workspace.path(), BeadStatus::InProgress);

    assert_eq!(
        CommandGate::new(vec!["true".to_string()])
            .validate(&bead, workspace.path())
            .await
            .unwrap(),
        GateResult::Pass
    );

    let failed = CommandGate::with_stderr_cap(
        vec!["head -c 200 /dev/zero | tr '\\0' x 1>&2; exit 7".to_string()],
        10,
    )
    .validate(&bead, workspace.path())
    .await
    .unwrap();
    assert!(matches!(failed, GateResult::Fail(_)));
    assert!(failed.failure_reason().unwrap().contains("[truncated]"));

    let aggregate = ValidationGate::from_commands(
        vec!["true".to_string(), "false".to_string()],
        workspace.path().to_path_buf(),
    )
    .expect("nonempty gate");
    let report = aggregate.run(&bead).await.unwrap();
    assert!(!report.all_passed);
}

#[tokio::test]
async fn clean_command_gate_detects_an_uncommitted_dependency() {
    let repo = GitRepo::new();
    let bead = test_bead(repo.path(), BeadStatus::InProgress);
    fs::write(repo.path().join("dependency.txt"), "workspace only\n").unwrap();
    let gate = CommandGate::with_options(
        vec!["test -f dependency.txt".to_string()],
        4096,
        RunIn::Clean,
    );

    let result = gate.validate(&bead, repo.path()).await.unwrap();
    match result {
        GateResult::Fail(reason) => assert!(reason.contains("uncommitted files")),
        other => panic!("expected uncommitted-dependency failure, got {other:?}"),
    }
}

#[tokio::test]
async fn command_gate_child_is_killed_when_timeout_drops_the_future() {
    let workspace = TempDir::new().expect("create command workspace");
    let marker = workspace.path().join("should-not-exist");
    let bead = test_bead(workspace.path(), BeadStatus::InProgress);
    let gate = CommandGate::new(vec![format!("sleep 3 && touch {}", marker.display())]);

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        gate.validate(&bead, workspace.path()),
    )
    .await;
    assert!(result.is_err(), "the outer timeout must win");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!marker.exists(), "dropped gate future left its child alive");
}

struct TestStore {
    bead: Bead,
    notes: Option<String>,
    actions: Mutex<Vec<String>>,
}

impl TestStore {
    fn new(bead: Bead) -> Self {
        Self {
            bead,
            notes: None,
            actions: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl BeadStore for TestStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn show(&self, _id: &BeadId) -> Result<Bead> {
        Ok(self.bead.clone())
    }

    async fn notes(&self, _id: &BeadId) -> Result<Option<String>> {
        Ok(self.notes.clone())
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
        Ok(ClaimResult::NotClaimable {
            reason: "fixture".to_string(),
        })
    }

    async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
        Ok(ClaimResult::NotClaimable {
            reason: "fixture".to_string(),
        })
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        self.actions.lock().unwrap().push(format!("release:{id}"));
        Ok(())
    }

    async fn block(&self, id: &BeadId) -> Result<()> {
        self.actions.lock().unwrap().push(format!("block:{id}"));
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        self.actions.lock().unwrap().push(format!("reopen:{id}"));
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
        Ok(self.bead.labels.clone())
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.actions
            .lock()
            .unwrap()
            .push(format!("label:{id}:{label}"));
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
        Ok(BeadId::from("needle-fixture-created"))
    }

    async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn remove_dependency(&self, _blocked_id: &BeadId, _blocker_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn full_rebuild(&self) -> Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn predispatch_records_git_head_and_filters_internal_state() {
    let repo = GitRepo::new();
    fs::write(repo.path().join("README.md"), "modified\n").unwrap();
    fs::write(repo.path().join("untracked.txt"), "untracked\n").unwrap();
    fs::create_dir_all(repo.path().join(".beads")).unwrap();
    fs::write(repo.path().join(".beads/internal.json"), "{}\n").unwrap();
    let state = TempDir::new().expect("create explicit state root");
    let bead = test_bead(repo.path(), BeadStatus::InProgress);
    let store = TestStore::new(bead.clone());

    let token = predispatch::record_in(state.path(), repo.path(), &bead.id, &store)
        .await
        .expect("record predispatch state")
        .expect("record has identity");
    let path = predispatch::snapshot_path_in(state.path(), repo.path(), &bead.id);
    let snapshot: PreDispatch =
        serde_json::from_slice(&fs::read(path).expect("read snapshot")).unwrap();

    assert_eq!(snapshot.head_sha.as_deref(), Some(repo.head().as_str()));
    assert!(snapshot
        .dirty_files
        .iter()
        .any(|entry| entry.path == "README.md"));
    assert!(snapshot
        .dirty_files
        .iter()
        .any(|entry| entry.path == "untracked.txt"));
    assert!(!snapshot
        .dirty_files
        .iter()
        .any(|entry| entry.path.starts_with(".beads/")));
    assert_eq!(snapshot.identity().as_deref(), Some(token.as_str()));
}

#[tokio::test]
async fn shipped_work_distinguishes_pushed_unpushed_and_missing_upstreams() {
    let repo = GitRepo::new();
    assert!(matches!(
        upstream_status(repo.path()).await,
        UpstreamStatus::NotConfigured
    ));
    let pre = repo.head();
    let remote = TempDir::new().expect("create bare remote");
    git_ok(remote.path(), &["init", "-q", "--bare"]);
    git_ok(
        repo.path(),
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    git_ok(repo.path(), &["push", "-q", "-u", "origin", "HEAD"]);
    assert!(matches!(
        upstream_status(repo.path()).await,
        UpstreamStatus::Present(_)
    ));

    repo.commit("work.rs", "fn work() {}\n", "unpushed work");
    let bead = test_bead(repo.path(), BeadStatus::Done);
    let store = TestStore::new(bead.clone());
    let snapshot = PreDispatch {
        head_sha: Some(pre),
        notes_hash: Some(predispatch::hash_notes("")),
        dirty_files: Vec::new(),
        captured_at: Some(Utc::now()),
    };
    let result = verify_shipped_work(&bead, repo.path(), &store, Some(&snapshot))
        .await
        .unwrap();
    assert!(matches!(result, GateResult::Fail(_)));

    git_ok(repo.path(), &["push", "-q"]);
    assert_eq!(
        verify_shipped_work(&bead, repo.path(), &store, Some(&snapshot))
            .await
            .unwrap(),
        GateResult::Pass
    );
}

#[serial_test::serial]
#[tokio::test]
async fn outcome_routes_an_unjudgeable_shipped_work_gate_without_penalty() {
    let home = TempDir::new().expect("create isolated HOME");
    let _home = HomeGuard::set(home.path());

    let repo = GitRepo::new();
    let pre = repo.head();
    repo.commit("work.rs", "fn work() {}\n", "real work");
    let bead = test_bead(repo.path(), BeadStatus::InProgress);
    let store = TestStore::new(test_bead(repo.path(), BeadStatus::Done));
    let mut config = needle::config::Config::default();
    config.worker.enforce_shipped_work = true;
    let log_dir = home.path().join("telemetry");
    let telemetry = Telemetry::with_log_dir("process-command-outcome".to_string(), &log_dir);
    telemetry.start();
    let handler = OutcomeHandler::new(config, telemetry.clone());
    handler.set_attempt_context(AttemptContext {
        bead_revision_start: Some(pre),
        ..AttemptContext::default()
    });

    let result = handler
        .handle(
            &store,
            &bead,
            &AgentOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            false,
        )
        .await
        .expect("route outcome");
    assert!(matches!(result.bead_action, BeadAction::Released(_)));
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .expect("flush telemetry");
    let events = telemetry_events(&log_dir);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "gate.execution_error")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "attempt.resolved")
            .count(),
        1
    );
    assert!(!store
        .actions
        .lock()
        .unwrap()
        .iter()
        .any(|action| action.contains("failure-count")));
    telemetry.shutdown().await;
}
