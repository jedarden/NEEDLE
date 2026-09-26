//! Real Git, tar, shell, and filesystem command contracts.
//!
//! These cases deliberately execute operating-system processes. Keeping them
//! in the `integration_spawn` binary lets the process-free library gate finish
//! without waiting on host command scheduling.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use needle::attempt_archive::{
    sha256_file, spool_attempt, AttemptArchiveInput, Sidecar, SIDECAR_SCHEMA_VERSION,
};
use needle::bead_store::{
    builtin_bead_backends, open_configured, BeadBackend, BeadStore, CliBeadStore, Filters,
    RepairReport,
};
use needle::build_status::template_for_workspace;
use needle::canary::CanaryRunner;
use needle::ci::{correlate_commit, CorrelationError};
use needle::commit_hook::{inject_bead_id_trailer, validate_commit};
use needle::config::{
    resolve_bead_cli, ArchiveCompression, AttemptArchiveConfig, Backend, BackendSource,
    BeadBackend as ConfiguredBackend, BeadCliConfig, Config, HookConfig, ResolveConfig,
    RetrievalConfig, ValidationConfig,
};
use needle::dispatch::{
    cleanup_extraction, extract_clean_workspace, extract_tokens, AgentAdapter, Dispatcher,
    ExtractionConfig, TimeoutReason, TokenExtraction,
};
use needle::mitosis::timeout_context::capture_timeout_context;
use needle::mitosis::timeout_context::write_timeout_context;
use needle::mitosis::timeout_context::{clear_timeout_context, load_timeout_context};
use needle::mitosis::timeout_eligibility::TimeoutEligibility;
use needle::outcome::{AttemptContext, OutcomeHandler};
use needle::prompt::{BuiltPrompt, PromptBuilder};
use needle::resolve::executor::{AppliedDecision, DecisionExecutor, ReleaseCause};
use needle::resolve::{ResolveContext, ResolveDecision, Resolver, VerificationError};
use needle::retrieval::{retrieve, RetrievalRequest};
use needle::scratch_sweep::{sweep_scratch_directory_with_proc_root, SweepOutcome};
use needle::spawn_version::spawn_version_output;
use needle::telemetry::{EventKind, HookSink, Telemetry, TelemetryEvent};
use needle::types::{
    AgentOutcome, Bead, BeadAction, BeadId, BeadStatus, ClaimResult, ClaimStatus, InputMethod,
    Outcome,
};
use needle::util::{detect_bead_cli_backend, parse_backend_name_from_version, probe_bead_cli};
use needle::validation::dod_bypass::check_dod_bypass;
use needle::validation::predispatch::{self, DirtyFile, PreDispatch};
use needle::validation::{
    upstream_status, verify_shipped_work, CommandGate, Gate, GateResult, RunIn, UpstreamStatus,
    ValidationGate,
};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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

struct PathGuard {
    previous: Option<OsString>,
}

impl PathGuard {
    fn prepend(path: &Path) -> Self {
        let previous = std::env::var_os("PATH");
        let mut paths = vec![path.to_path_buf()];
        paths.extend(std::env::split_paths(
            previous.as_deref().unwrap_or_default(),
        ));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        Self { previous }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
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

fn process_contract_adapter(name: &str, template: &str) -> AgentAdapter {
    AgentAdapter {
        name: name.to_string(),
        description: None,
        agent_cli: "test".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: template.to_string(),
        environment: HashMap::new(),
        timeout_secs: 10,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        usage_format: None,
        output_transform: None,
        harness: None,
        harness_version: None,
    }
}

fn process_contract_prompt(content: &str) -> BuiltPrompt {
    BuiltPrompt {
        content: content.to_string(),
        hash: "testhash".to_string(),
        token_estimate: content.len() as u64 / 4,
        template_name: "pluck".to_string(),
        template_version: "pluck-default".to_string(),
    }
}

fn process_contract_dispatcher(adapters: HashMap<String, AgentAdapter>) -> Dispatcher {
    let telemetry = Telemetry::new("process-contract-worker".to_string());
    // The dispatcher refuses to spawn unless its pre-spawn claim verification
    // passes, so contract tests wire a permissive store matching the worker
    // id. Verification-specific behavior lives in the fail-closed tests.
    Dispatcher::with_adapters(adapters, telemetry, 3600)
        .with_bead_store(std::sync::Arc::new(
            crate::pre_spawn_pass_store::AlwaysClaimedStore::new("process-contract-worker"),
        ))
        .with_worker_id("process-contract-worker".to_string())
}

fn process_contract_event(event_type: &str) -> TelemetryEvent {
    TelemetryEvent {
        timestamp: Utc::now(),
        event_type: event_type.to_string(),
        worker_id: "alpha".to_string(),
        session_id: "test0000".to_string(),
        sequence: 0,
        bead_id: None,
        workspace: None,
        data: serde_json::json!({"test": true}),
        duration_ms: None,
        trace_id: None,
        span_id: None,
        attempt_id: None,
    }
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

    fn with_notes(mut self, notes: &str) -> Self {
        self.notes = Some(notes.to_string());
        self
    }

    fn actions(&self) -> Vec<String> {
        self.actions.lock().unwrap().clone()
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
        self.actions.lock().unwrap().push("show".to_string());
        Ok(self.bead.clone())
    }

    async fn claim_status(&self, _id: &BeadId) -> Result<ClaimStatus> {
        Ok(ClaimStatus {
            status: self.bead.status.clone(),
            assignee: self.bead.assignee.clone(),
            revision: None,
            claim_epoch: None,
        })
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

    async fn close(&self, id: &BeadId, reason: &str) -> Result<()> {
        self.actions
            .lock()
            .unwrap()
            .push(format!("close:{id}:{reason}"));
        Ok(())
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        self.actions.lock().unwrap().push(format!("reopen:{id}"));
        Ok(())
    }

    async fn append_notes(&self, id: &BeadId, note: &str) -> Result<()> {
        self.actions
            .lock()
            .unwrap()
            .push(format!("notes:{id}:{note}"));
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
    repo.commit("work.rs", "fn work() {}\n", "work without upstream");
    let bead = test_bead(repo.path(), BeadStatus::Done);
    let store = TestStore::new(bead.clone());
    let snapshot = PreDispatch {
        head_sha: Some(pre.clone()),
        notes_hash: Some(predispatch::hash_notes("")),
        dirty_files: Vec::new(),
        captured_at: Some(Utc::now()),
    };
    match verify_shipped_work(&bead, repo.path(), &store, Some(&snapshot))
        .await
        .unwrap()
    {
        GateResult::Unsatisfiable(reason) => {
            assert!(reason.contains("no upstream configured"));
            assert!(reason.contains("git remote add"));
            assert!(reason.contains("git push -u"));
            assert!(reason.contains("worker.enforce_shipped_work: false"));
            assert!(!reason.contains("has not been pushed"));
        }
        other => panic!("expected an unsatisfiable gate, got {other:?}"),
    }

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

    repo.commit(
        "work.rs",
        "fn work() {}\n// unpushed work\n",
        "unpushed work",
    );
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
    assert_eq!(result.outcome, Outcome::GateUnsatisfiable);
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
        0
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

fn archive_and_status_process_contracts_input() -> AttemptArchiveInput {
    AttemptArchiveInput {
        attempt_id: "0192-attempt-1".into(),
        bead_id: "needle-abc".into(),
        workspace: "/ws".into(),
        worker: "needle-alpha".into(),
        adapter: "claude-code-glm-5.3-flash".into(),
        model: Some("glm-5.3-flash".into()),
        outcome: "work_failure".into(),
        terminal_reason: Some("gate:dod".into()),
        recorded_at: "2026-09-12T15:00:00.000Z".into(),
    }
}

fn archive_and_status_process_contracts_executable(
    root: &Path,
    name: &str,
    body: &str,
) -> std::path::PathBuf {
    let path = root.join(name);
    let staged = root.join(format!(".{name}.staged"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .expect("create staged executable fixture");
    file.write_all(body.as_bytes())
        .expect("write executable fixture");
    file.sync_all().expect("sync executable fixture");
    drop(file);
    let mut permissions = fs::metadata(&staged).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&staged, permissions).unwrap();
    fs::rename(&staged, &path).expect("publish closed executable fixture atomically");
    path
}

#[test]
fn archive_and_status_process_contracts_spool_a_bundle_and_sidecar() {
    let spool = TempDir::new().unwrap();
    let trace = TempDir::new().unwrap();
    fs::write(trace.path().join("metadata.json"), "{\"exit_code\":0}").unwrap();
    fs::write(trace.path().join("stdout.txt"), "hello").unwrap();
    fs::write(trace.path().join("attempts.jsonl"), "{}\n").unwrap();
    let config = AttemptArchiveConfig {
        enabled: true,
        spool_dir: spool.path().to_path_buf(),
        ..AttemptArchiveConfig::default()
    };
    let input = archive_and_status_process_contracts_input();

    let receipt = spool_attempt(&config, &input, Some(trace.path()))
        .unwrap()
        .expect("spooled");
    assert!(receipt.bundle.is_file());
    assert!(receipt.sidecar.is_file());
    assert!(receipt.bundle_bytes > 0);
    let relative = receipt.sidecar.strip_prefix(spool.path()).unwrap();
    let parts: Vec<_> = relative.components().collect();
    assert_eq!(parts.len(), 3, "{relative:?}");
    assert_eq!(parts[1].as_os_str(), "2026-09-12");
    assert!(relative
        .to_string_lossy()
        .ends_with("needle-abc-0192-attempt-1.json"));

    let sidecar: Sidecar =
        serde_json::from_str(&fs::read_to_string(&receipt.sidecar).unwrap()).unwrap();
    assert_eq!(sidecar.schema_version, SIDECAR_SCHEMA_VERSION);
    let bundle = spool.path().join(&sidecar.bundle_path);
    assert_eq!(bundle, receipt.bundle);
    let (digest, size) = sha256_file(&bundle).unwrap();
    assert_eq!(digest, sidecar.bundle_sha256);
    assert_eq!(size, sidecar.bundle_bytes);
    assert_eq!(sidecar.attempt, input);
    assert!(sidecar.files.contains(&"attempt.json".to_string()));
    assert!(sidecar.files.contains(&"stdout.txt".to_string()));
    assert!(sidecar.files.contains(&"attempts.jsonl".to_string()));
    assert!(!spool
        .path()
        .join(".staging/needle-abc-0192-attempt-1")
        .exists());

    let listing = if bundle
        .extension()
        .is_some_and(|extension| extension == "zst")
    {
        Command::new("tar")
            .args(["--zstd", "-tf"])
            .arg(&bundle)
            .output()
            .unwrap()
    } else {
        Command::new("tar")
            .arg("-tf")
            .arg(&bundle)
            .output()
            .unwrap()
    };
    let listing = String::from_utf8_lossy(&listing.stdout);
    assert!(listing.contains("attempt.json"), "{listing}");
    assert!(listing.contains("stdout.txt"), "{listing}");
}

#[test]
fn archive_and_status_process_contracts_archive_missing_trace_facts() {
    let spool = TempDir::new().unwrap();
    let config = AttemptArchiveConfig {
        enabled: true,
        spool_dir: spool.path().to_path_buf(),
        compression: ArchiveCompression::None,
        ..AttemptArchiveConfig::default()
    };
    let receipt = spool_attempt(
        &config,
        &archive_and_status_process_contracts_input(),
        Some(Path::new("/nonexistent/trace")),
    )
    .unwrap()
    .expect("spooled");
    assert!(receipt.bundle.to_string_lossy().ends_with(".tar"));
    let sidecar: Sidecar =
        serde_json::from_str(&fs::read_to_string(&receipt.sidecar).unwrap()).unwrap();
    assert_eq!(sidecar.files, vec!["attempt.json".to_string()]);
}

#[test]
fn archive_and_status_process_contracts_version_stdout() {
    let root = TempDir::new().unwrap();
    let executable = archive_and_status_process_contracts_executable(
        root.path(),
        "fake-binary",
        "#!/bin/sh\necho 'fake-binary 1.0.0'\n",
    );
    assert_eq!(
        spawn_version_output(&executable).unwrap().trim(),
        "fake-binary 1.0.0"
    );
}

#[test]
fn archive_and_status_process_contracts_version_nonzero_exit() {
    let root = TempDir::new().unwrap();
    let executable = archive_and_status_process_contracts_executable(
        root.path(),
        "failing-binary",
        "#!/bin/sh\necho 'Error: something went wrong' >&2\nexit 1\n",
    );
    let error = spawn_version_output(&executable).unwrap_err().to_string();
    assert!(error.contains("exited with code"));
}

#[test]
fn archive_and_status_process_contracts_version_empty_output() {
    let root = TempDir::new().unwrap();
    let executable = archive_and_status_process_contracts_executable(
        root.path(),
        "empty-binary",
        "#!/bin/sh\n# intentionally empty\n",
    );
    assert!(spawn_version_output(&executable).unwrap().trim().is_empty());
}

#[test]
fn archive_and_status_process_contracts_version_multiline_output() {
    let root = TempDir::new().unwrap();
    let executable = archive_and_status_process_contracts_executable(
        root.path(),
        "multiline-binary",
        "#!/bin/sh\necho 'my-tool 2.0.0'\necho 'Build metadata: some info'\necho 'Copyright 2026'\n",
    );
    let output = spawn_version_output(&executable).unwrap();
    assert!(output.contains("my-tool 2.0.0"));
    assert!(output.contains("Build metadata"));
    assert!(output.contains("Copyright"));
}

#[test]
fn archive_and_status_process_contracts_version_preserves_raw_output() {
    let root = TempDir::new().unwrap();
    let executable = archive_and_status_process_contracts_executable(
        root.path(),
        "raw-binary",
        "#!/bin/sh\necho '  tool-with-spacing   1.2.3  '\n",
    );
    assert!(spawn_version_output(&executable)
        .unwrap()
        .contains("  tool-with-spacing   1.2.3  "));
}

#[test]
fn archive_and_status_process_contracts_version_returns_string() {
    let root = TempDir::new().unwrap();
    let executable = archive_and_status_process_contracts_executable(
        root.path(),
        "string-binary",
        "#!/bin/sh\necho 'test output'\n",
    );
    let output: String = spawn_version_output(&executable).unwrap();
    assert_eq!(output.trim(), "test output");
}

#[test]
fn archive_and_status_process_contracts_version_basic_spawn() {
    let root = TempDir::new().unwrap();
    let executable = archive_and_status_process_contracts_executable(
        root.path(),
        "basic-binary",
        "#!/bin/sh\necho 'basic 1.0'\n",
    );
    assert!(spawn_version_output(&executable).is_ok());
}

#[tokio::test]
async fn archive_and_status_process_contracts_workspace_template() {
    let template = template_for_workspace(Path::new(env!("CARGO_MANIFEST_DIR")))
        .await
        .expect("manifest directory has a Git remote");
    assert_eq!(template, "needle-ci");
}

#[test]
fn archive_and_status_process_contracts_canary_backend_projection() {
    let projection = r#"[{"status":"closed","labels":["native"]}]"#;
    let root = TempDir::new().unwrap();
    let binary = archive_and_status_process_contracts_executable(
        root.path(),
        "bound-backend",
        &format!("#!/bin/sh\nprintf '%s\\n' '{projection}'\n"),
    );
    fs::write(
        root.path().join(".needle.yaml"),
        format!(
            "bead_cli:\n  backend: bead-rs\n  path: {}\n",
            binary.display()
        ),
    )
    .unwrap();

    let runner = CanaryRunner::new(root.path().join("needle"), root.path().into(), 30);
    let isolated_home = TempDir::new().unwrap();
    let actual = runner
        .get_actual_outcome("example-1", Some(0), isolated_home.path())
        .unwrap();
    assert_eq!(actual.final_status, "closed");
    assert_eq!(actual.labels, vec!["native"]);
}

#[tokio::test]
async fn backend_probe_process_contracts_version_matrix() {
    let root = TempDir::new().unwrap();
    for (name, output, expected) in [
        ("bf-version", "bf 0.4.1", "bf"),
        ("bead-version", "bead 0.2.6", "bead"),
        ("custom-version", "my-backend 2.0.0", "my-backend"),
        ("multiline-version", "bead 0.2.6\nBuild metadata", "bead"),
    ] {
        let binary = archive_and_status_process_contracts_executable(
            root.path(),
            name,
            &format!("#!/bin/sh\nprintf '%s\\n' '{output}'\n"),
        );
        assert_eq!(
            parse_backend_name_from_version(&binary, &["--version"]).unwrap(),
            expected
        );
    }

    let custom = archive_and_status_process_contracts_executable(
        root.path(),
        "custom-args",
        "#!/bin/sh\n[ \"$1\" = version ] || exit 2\nprintf '%s\\n' 'custom 1.0'\n",
    );
    assert_eq!(
        parse_backend_name_from_version(&custom, &["version"]).unwrap(),
        "custom"
    );

    let failing = archive_and_status_process_contracts_executable(
        root.path(),
        "failing-version",
        "#!/bin/sh\nprintf '%s\\n' 'bad version' >&2\nexit 7\n",
    );
    assert!(parse_backend_name_from_version(&failing, &["--version"])
        .unwrap_err()
        .to_string()
        .contains("exited with code 7"));

    let empty = archive_and_status_process_contracts_executable(
        root.path(),
        "empty-version",
        "#!/bin/sh\nexit 0\n",
    );
    assert!(parse_backend_name_from_version(&empty, &["--version"])
        .unwrap_err()
        .to_string()
        .contains("empty"));

    let stderr = archive_and_status_process_contracts_executable(
        root.path(),
        "stderr-version",
        "#!/bin/sh\nprintf '%s\\n' 'bead 0.2.6' >&2\n",
    );
    assert_eq!(
        needle::bead_store::parse_backend_name_from_version(&stderr, None)
            .await
            .unwrap(),
        "bead"
    );
    assert!(BeadBackend::parse_backend_name_from_version(
        Path::new("/nonexistent/backend-probe-binary"),
        &["--version".to_string()],
    )
    .unwrap_err()
    .to_string()
    .contains("failed to spawn"));
}

#[test]
fn backend_probe_process_contracts_path_matrix_driver() {
    if std::env::var_os("NEEDLE_BACKEND_PROBE_CHILD").is_some() {
        return;
    }
    let root = TempDir::new().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_command_contracts::backend_probe_process_contracts_path_child",
            "--nocapture",
        ])
        .env("NEEDLE_BACKEND_PROBE_CHILD", root.path())
        .output()
        .expect("run isolated PATH probe matrix");
    assert!(
        output.status.success(),
        "isolated PATH probe matrix failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn backend_probe_process_contracts_path_child() {
    let Some(root) = std::env::var_os("NEEDLE_BACKEND_PROBE_CHILD") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let scenario = |name: &str, binaries: &[(&str, &str)]| {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        for (binary, identity) in binaries {
            archive_and_status_process_contracts_executable(
                &directory,
                binary,
                &format!("#!/bin/sh\nprintf '%s\\n' '{identity}'\n"),
            );
        }
        std::env::set_var("PATH", &directory);
        directory
    };

    let bead = scenario("bead-only", &[("bead", "bead 0.2.6")]);
    assert_eq!(probe_bead_cli().unwrap().name, "bead");
    let detected = detect_bead_cli_backend(ConfiguredBackend::Auto, None).unwrap();
    assert_eq!(detected.backend, "bead-rs");
    assert_eq!(detected.cli_path, bead.join("bead"));

    scenario("bf-only", &[("bf", "bf 0.4.1")]);
    assert_eq!(probe_bead_cli().unwrap().name, "bf");

    scenario("br-only", &[("br", "br 0.4.1")]);
    assert_eq!(probe_bead_cli().unwrap().name, "br");
    assert_eq!(
        detect_bead_cli_backend(ConfiguredBackend::Br, None)
            .unwrap()
            .backend,
        "br"
    );

    scenario("priority", &[("bead", "bead 0.2.6"), ("bf", "bf 0.4.1")]);
    assert_eq!(probe_bead_cli().unwrap().name, "bead");

    scenario(
        "identity-fallback",
        &[("bead", "bf 0.4.1"), ("bf", "bf 0.4.1")],
    );
    assert_eq!(probe_bead_cli().unwrap().name, "bf");

    let spaced = scenario("path with spaces", &[("bead", "bead 0.2.6")]);
    assert_eq!(probe_bead_cli().unwrap().path, spaced.join("bead"));

    scenario("none", &[]);
    assert!(probe_bead_cli().is_none());
    assert!(detect_bead_cli_backend(ConfiguredBackend::Auto, None).is_none());
}

#[test]
fn backend_probe_process_contracts_config_resolution() {
    let root = TempDir::new().unwrap();
    for name in ["my-bead-cli", "bead-nightly", "custom-bead-cli"] {
        let binary = archive_and_status_process_contracts_executable(
            root.path(),
            name,
            "#!/bin/sh\nprintf '%s\\n' 'bead 0.2.6'\n",
        );
        let (backend, path, source) = resolve_bead_cli(&BeadCliConfig {
            backend: ConfiguredBackend::Auto,
            path: Some(binary.clone()),
        })
        .unwrap();
        assert_eq!(backend, Backend::Bead);
        assert_eq!(path, binary);
        assert_eq!(source, BackendSource::ExplicitPath);
    }

    assert!(resolve_bead_cli(&BeadCliConfig {
        backend: ConfiguredBackend::Auto,
        path: Some(Path::new("/").to_path_buf()),
    })
    .is_err());
}

#[test]
fn backend_probe_process_contracts_retry_real_etxtbsy() {
    fn release_write_guard(path: &Path) -> std::thread::JoinHandle<()> {
        let guard = fs::OpenOptions::new().write(true).open(path).unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            drop(guard);
        })
    }

    let root = TempDir::new().unwrap();
    let binary = archive_and_status_process_contracts_executable(
        root.path(),
        "busy-bead",
        "#!/bin/sh\nprintf '%s\\n' 'bead 0.2.6'\n",
    );

    let release = release_write_guard(&binary);
    assert_eq!(spawn_version_output(&binary).unwrap(), "bead 0.2.6\n");
    release.join().unwrap();

    let release = release_write_guard(&binary);
    let resolved = resolve_bead_cli(&BeadCliConfig {
        backend: ConfiguredBackend::Auto,
        path: Some(binary.clone()),
    })
    .unwrap();
    release.join().unwrap();
    assert_eq!(resolved.0, Backend::Bead);
    assert_eq!(resolved.1, binary);
    assert_eq!(resolved.2, BackendSource::ExplicitPath);
}

fn backend_probe_capability_script(version: &str, capabilities: &str) -> String {
    format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) printf '%s\\n' '{version}' ;;\n  capabilities) [ \"$2\" = --profile ] && [ \"$3\" = native-v1 ] || exit 9; printf '%s' '{capabilities}' ;;\n  *) exit 8 ;;\nesac\n"
    )
}

#[tokio::test]
async fn backend_probe_process_contracts_identity_and_capabilities() {
    const VALID: &str = r#"{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:event:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}"#;
    let cases = [
        ("bead 0.2.6", VALID, None),
        ("bf 0.4.1", VALID, Some("identity mismatch")),
        (
            "bead 0.2.6",
            r#"{"implementation":"bead-rs","atomic_claim":false,"statuses":["open","in_progress","deferred","closed"],"schemas":[]}"#,
            Some("capability mismatch"),
        ),
        (
            "bead 0.2.6",
            r#"{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","deferred","closed","blocked"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:event:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}"#,
            Some("unexpected status"),
        ),
        (
            "bead 0.2.6",
            r#"{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:event:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}"#,
            Some("deferred"),
        ),
        (
            "bead 0.2.6",
            r#"{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"}],"commands":["ref","data","query"]}"#,
            Some("schema"),
        ),
        (
            "bead 0.2.6",
            r#"{"implementation":"bead-forge","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:event:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}"#,
            Some("backend identity mismatch"),
        ),
        ("bead 0.2.6", "{invalid-json}", Some("JSON")),
    ];

    for (index, (version, capabilities, expected_error)) in cases.into_iter().enumerate() {
        let workspace = TempDir::new().unwrap();
        let binary = archive_and_status_process_contracts_executable(
            workspace.path(),
            "bead",
            &backend_probe_capability_script(version, capabilities),
        );
        let result = open_configured(
            &BeadCliConfig {
                backend: ConfiguredBackend::Bead,
                path: Some(binary),
            },
            workspace.path().to_path_buf(),
            None,
            None,
            None,
        );
        match expected_error {
            None => assert!(result.is_ok(), "case {index} should succeed"),
            Some(expected) => {
                let error = result.err().expect("case should fail").to_string();
                assert!(
                    error.contains(expected),
                    "case {index}: expected {expected:?}, got {error:?}"
                );
            }
        }
    }

    super::capabilities_negotiation_conformance::verify_required_identity_status_schema_and_command_capabilities();
    super::capabilities_negotiation_conformance::verify_optional_and_static_capability_projection();
    super::capabilities_negotiation_conformance::verify_each_transition_capability_gate();
    super::capabilities_negotiation_conformance::verify_worker_blocks_incompatible_capability_probes()
        .await;
}

#[tokio::test]
async fn backend_probe_process_contracts_cli_store_commands() {
    let workspace = TempDir::new().unwrap();
    let binary = archive_and_status_process_contracts_executable(
        workspace.path(),
        "fake-bead",
        r#"#!/bin/sh
case "$1" in
  list)
    printf '%s\n' \
      '{"id":"active","title":"active","priority":1,"status":"open","labels":["quarantine-until:2099-01-01T00:00:00Z"],"created_at":"2026-08-13T00:00:00Z"}' \
      '{"id":"legacy-marked","title":"legacy","priority":1,"status":"open","labels":["deferred","failure-count:1","quarantine-until:2000-01-01T00:00:00Z"],"created_at":"2026-08-13T00:00:00Z"}' \
      '{"id":"operator-deferred","title":"operator","priority":1,"status":"open","labels":["deferred"],"created_at":"2026-08-13T00:00:00Z"}'
    ;;
  show)
    case "$2" in
      good) printf '%s\n' '{"id":"good","title":"Good","priority":1,"status":"open","labels":[],"created_at":"2026-08-13T00:00:00Z"}' ;;
      missing) printf '%s\n' '[]' ;;
      failed) printf '%s\n' 'not found' >&2; exit 1 ;;
      malformed) printf '%s\n' '{"id":' ;;
    esac
    ;;
  init) printf '%s' incomplete > .beads/beads.db ;;
  sync) printf '%s\n' import-failed >&2; exit 1 ;;
  *) exit 2 ;;
esac
"#,
    );
    let descriptor = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let store = CliBeadStore::new(
        descriptor,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    let filters = Filters {
        assignee: None,
        exclude_labels: vec!["deferred".into(), "human".into(), "blocked".into()],
        exclude_ids: std::collections::HashSet::new(),
    };
    let ready = store.ready(&filters).await.unwrap();
    assert_eq!(
        ready
            .iter()
            .map(|bead| bead.id.as_ref())
            .collect::<Vec<_>>(),
        ["legacy-marked"]
    );

    let bead = store.show(&BeadId::from("good")).await.unwrap();
    assert_eq!(bead.title, "Good");
    for id in ["missing", "failed", "malformed"] {
        assert!(store.show(&BeadId::from(id)).await.is_err(), "{id}");
    }

    let beads = workspace.path().join(".beads");
    fs::create_dir_all(beads.join("checkpoint")).unwrap();
    fs::write(beads.join("checkpoint/forensic.jsonl"), "checkpoint\n").unwrap();
    fs::write(beads.join("beads.db"), "original database").unwrap();
    let error = store.full_rebuild().await.unwrap_err();
    assert!(error.to_string().contains("original database restored"));
    assert_eq!(
        fs::read_to_string(beads.join("beads.db")).unwrap(),
        "original database"
    );
    assert!(!beads.join("beads.db.needle-rebuild-backup").exists());
}
#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_all_template_variables_substituted() {
    // Verify that {workspace}, {prompt_file}, {bead_id}, and {model} are
    // all rendered into the command the agent receives.
    let mut adapter = process_contract_adapter(
        "vars",
        "echo ws={workspace} pf={prompt_file} bid={bead_id} m={model}",
    );
    adapter.model = Some("test-model-v1".to_string());

    let mut adapters = HashMap::new();
    adapters.insert("vars".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("vars").unwrap().clone();

    let workspace = std::env::temp_dir().join("needle-e2e-vars");
    let _ = std::fs::create_dir_all(&workspace);

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("needle-tmpl"),
            &process_contract_prompt("irrelevant"),
            &adapter,
            &workspace,
            &crate::pre_spawn_pass_store::claimed_context(&workspace),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    let out = result.stdout.trim();
    assert!(
        out.contains(&format!("ws={}", workspace.display())),
        "workspace not substituted: {out}"
    );
    assert!(
        out.contains("bid=needle-tmpl"),
        "bead_id not substituted: {out}"
    );
    assert!(
        out.contains("m=test-model-v1"),
        "model not substituted: {out}"
    );
    // prompt_file is a temp path — just verify it was substituted (not literal)
    assert!(
        !out.contains("{prompt_file}"),
        "prompt_file placeholder not replaced: {out}"
    );
    assert!(
        out.contains("pf=/"),
        "prompt_file should be an absolute path: {out}"
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_adapter_with_custom_env_and_base_url() {
    // Simulate an adapter with ANTHROPIC_BASE_URL and custom env vars,
    // verifying they're all available to the child process.
    let mut adapter = process_contract_adapter(
        "custom-env",
        "echo base=$ANTHROPIC_BASE_URL custom=$CUSTOM_FLAG",
    );
    adapter.environment.insert(
        "ANTHROPIC_BASE_URL".to_string(),
        "https://api.example.com".to_string(),
    );
    adapter
        .environment
        .insert("CUSTOM_FLAG".to_string(), "enabled".to_string());

    let mut adapters = HashMap::new();
    adapters.insert("custom-env".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("custom-env").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-baseurl"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert!(
        result.stdout.contains("base=https://api.example.com"),
        "ANTHROPIC_BASE_URL not set: {}",
        result.stdout
    );
    assert!(
        result.stdout.contains("custom=enabled"),
        "CUSTOM_FLAG not set: {}",
        result.stdout
    );
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_on_rapid_output() {
    // Test that activity detection handles rapid output without missing bytes
    let adapter = process_contract_adapter(
        "rapid-test",
        "for i in $(seq 1 100); do echo \"rapid $i\"; done",
    );
    let mut adapters = HashMap::new();
    adapters.insert("rapid-test".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);

    let adapter_ref = dispatcher.adapter("rapid-test").unwrap().clone();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-rapid-test"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    // Should complete successfully
    assert_eq!(result.exit_code, 0);
    // All rapid lines should be captured
    assert!(result.stdout.contains("rapid 1"));
    assert!(result.stdout.contains("rapid 100"));
    // Count the lines to verify none were missed
    let line_count = result.stdout.lines().count();
    assert!(
        line_count >= 100,
        "Expected at least 100 lines, got {}",
        line_count
    );
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_on_normal_stdout_output() {
    // Test that activity detection works for normal stdout output
    let adapter = process_contract_adapter("echo-test", "echo 'hello world'");
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);

    let adapter_ref = dispatcher.adapter("echo-test").unwrap().clone();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-echo-test"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    // Should complete successfully
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("hello world"));
    // Activity was detected (process completed without timeout)
    assert!(result.elapsed < Duration::from_secs(10));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_during_transforms() {
    // Test that activity detection works when output_transform is configured
    let mut adapter = process_contract_adapter("transform-test", "echo 'test output'");
    adapter.output_transform = Some("cat".to_string()); // Use cat as simple transform
    let mut adapters = HashMap::new();
    adapters.insert("transform-test".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);

    let adapter_ref = dispatcher.adapter("transform-test").unwrap().clone();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-transform-test"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    // Should complete successfully with transform
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("test output"));
    // Activity was detected even with transform active
    assert!(result.elapsed < Duration::from_secs(10));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_hard_timeout_kills_entire_process_group_active() {
    // Test that hard timeout kills the entire process group, even when agent is active.
    let pid_file = std::env::temp_dir().join(format!("needle-hard-pg-{}.pid", std::process::id()));
    let pid_file_str = pid_file.display().to_string();

    let cmd = format!(
        "sleep 1000 & echo $! > {pid_file_str}; while true; do echo 'active'; sleep 0.05; done"
    );

    let adapter = process_contract_adapter("hard-pgkill", &cmd);
    let mut adapters = HashMap::new();

    let mut adapter_with_hard = adapter.clone();
    adapter_with_hard.timeout_secs = 0;
    adapter_with_hard.idle_timeout_secs = 0;
    adapter_with_hard.hard_timeout_secs = 2;

    adapters.insert("hard-pgkill".to_string(), adapter_with_hard);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter_ref = dispatcher.adapter("hard-pgkill").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-hard-pgkill"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 124);
    assert!(matches!(
        result.timeout_reason,
        Some(TimeoutReason::Hard { .. })
    ));

    let pid_str =
        std::fs::read_to_string(&pid_file).expect("grandchild PID file should have been written");
    let grandchild_pid: libc::pid_t = pid_str
        .trim()
        .parse()
        .expect("PID file should contain a valid integer PID");

    // Liveness is judged by the crate's own definition
    // (`registry::is_pid_alive`, ADR-010 / GH #12), not by a raw
    // `kill(pid, 0)`: on Linux that syscall also succeeds for a zombie, and
    // in iad-ci nothing ever reaps one. The killed `sleep`'s parent shell is
    // reaped by the dispatcher above, so the orphan is reparented to PID 1
    // of the container's PID namespace — `argoexec` under the emissary
    // executor, which does not reap orphans (argoproj/argo-workflows#9446).
    // The grandchild therefore parks as a zombie indefinitely in CI, while a
    // host with a reaping init (systemd) clears it in milliseconds — the
    // exact passes-locally/fails-in-pod split of needle-2a15a0a0. A zombie
    // has received the group SIGKILL and terminated, which is what this
    // assertion exists to prove; only a process still scheduled (state
    // R/S/D/T, which `is_pid_alive` reports as alive) is a kill that missed.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let dead = loop {
        if !needle::registry::is_pid_alive(grandchild_pid as u32) {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        dead,
        "grandchild {grandchild_pid} still alive 3s after the hard-timeout \
         group kill — the kill never reached it"
    );

    let _ = std::fs::remove_file(&pid_file);
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_workspace_directory_is_correct() {
    // Verify the agent process can see the workspace directory.
    let workspace = std::env::temp_dir().join("needle-e2e-wsdir");
    let _ = std::fs::create_dir_all(&workspace);

    let mut adapters = HashMap::new();
    adapters.insert(
        "pwd".to_string(),
        process_contract_adapter("pwd", "cd {workspace} && pwd"),
    );
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("pwd").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-wsdir"),
            &process_contract_prompt("t"),
            &adapter,
            &workspace,
            &crate::pre_spawn_pass_store::claimed_context(&workspace),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    // Canonicalize both to handle symlinks (e.g., /tmp -> /private/tmp on macOS)
    let expected = std::fs::canonicalize(&workspace)
        .unwrap_or_else(|_| workspace.clone())
        .display()
        .to_string();
    let actual = result.stdout.trim().to_string();
    let actual_canonical = std::fs::canonicalize(&actual)
        .map(|p| p.display().to_string())
        .unwrap_or(actual);
    assert_eq!(actual_canonical, expected);

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_exit_code_137_is_crash() {
    let mut adapters = HashMap::new();
    adapters.insert(
        "crash".to_string(),
        process_contract_adapter("crash", "exit 137"),
    );
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("crash").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-exit137"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 137);
    assert_eq!(
        Outcome::classify(result.exit_code, false),
        Outcome::Crash(137)
    );
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_dispatch_template_renders_bead_id() {
    let mut adapters = HashMap::new();
    adapters.insert(
        "id".to_string(),
        process_contract_adapter("id", "echo bead={bead_id}"),
    );
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("id").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("needle-xyz"),
            &process_contract_prompt("test"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout.trim(), "bead=needle-xyz");
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_dispatch_stdin_redirect_from_prompt_file() {
    let mut adapters = HashMap::new();
    adapters.insert(
        "cat".to_string(),
        process_contract_adapter("cat", "cat < {prompt_file}"),
    );
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("cat").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-stdin"),
            &process_contract_prompt("prompt-content-here"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout.trim(), "prompt-content-here");
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_dispatch_captures_stderr() {
    let mut adapters = HashMap::new();
    adapters.insert(
        "err".to_string(),
        process_contract_adapter("err", "echo error-output >&2"),
    );
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("err").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-stderr"),
            &process_contract_prompt("test"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stderr.trim(), "error-output");
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_prompt_with_newlines_preserved() {
    let multiline = "line one\nline two\nline three";

    let mut adapters = HashMap::new();
    adapters.insert(
        "wc".to_string(),
        process_contract_adapter("wc", "wc -l < {prompt_file}"),
    );
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("wc").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-newlines"),
            &process_contract_prompt(multiline),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    let line_count: i32 = result.stdout.trim().parse().unwrap_or(-1);
    // wc -l counts newline characters; "line one\nline two\nline three"
    // has 2 newlines, so wc -l reports 2.
    assert_eq!(line_count, 2, "prompt should have 2 newlines (3 lines)");
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_on_binary_data() {
    // Verify that binary/non-text byte sequences are detected as activity.
    // Use printf to emit raw bytes including non-printable characters.
    let mut adapter = process_contract_adapter(
        "binary-output",
        // Emit binary bytes: 0x00 0x01 0x02 ... 0x09, then newline, repeat
        "for i in $(seq 1 20); do printf '\\x00\\x01\\x02\\x03\\x04\\x05\\x06\\x07\\x08\\x09\\n'; sleep 0.15; done",
    );
    adapter.idle_timeout_secs = 1;
    adapter.hard_timeout_secs = 10;

    let mut adapters = HashMap::new();
    adapters.insert("binary-output".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("binary-output").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-activity-binary"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(
        result.exit_code, 0,
        "binary output should prevent idle timeout"
    );
    // Should run ~3s (20 * 0.15s), well past idle deadline
    assert!(
        result.elapsed >= Duration::from_millis(2800),
        "should run full duration with binary output"
    );
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_large_output_burst() {
    // Verify that a large burst of output (multiple chunks) is detected as activity.
    let mut adapter = process_contract_adapter(
        "large-burst",
        // Emit 50KB of data in one go
        "dd if=/dev/zero bs=1024 count=50 2>/dev/null; sleep 0.5; echo done",
    );
    adapter.idle_timeout_secs = 1;
    adapter.hard_timeout_secs = 10;

    let mut adapters = HashMap::new();
    adapters.insert("large-burst".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("large-burst").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-activity-burst"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(
        result.exit_code, 0,
        "large output burst should be detected as activity"
    );
    // 50KB read time + 0.5s sleep
    assert!(result.elapsed >= Duration::from_millis(400));
    assert!(result.stdout.contains("done"));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_on_stdout_resets_idle_timeout() {
    // Verify that ongoing stdout output prevents idle timeout from firing.
    // A process that outputs continuously should not be killed by idle deadline.
    let mut adapter = process_contract_adapter(
        "chatty-stdout",
        // Output a dot every 200ms, then sleep 100 at the end.
        "for i in $(seq 1 10); do echo -n .; sleep 0.2; done; sleep 0.1",
    );
    adapter.idle_timeout_secs = 1; // 1 second idle timeout
    adapter.hard_timeout_secs = 10; // 10 second hard timeout (should not fire)

    let mut adapters = HashMap::new();
    adapters.insert("chatty-stdout".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("chatty-stdout").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-activity-stdout"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    // Should succeed, not be killed by idle timeout
    assert_eq!(
        result.exit_code, 0,
        "process with continuous output should not idle timeout"
    );
    // The loop runs for ~2.1s (10 * 0.2s + 0.1s), well past the 1s idle deadline
    assert!(
        result.elapsed >= Duration::from_millis(1900),
        "should run full duration"
    );
    assert!(
        result.stdout.contains(".........."),
        "should capture all output"
    );
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_on_multiline_output() {
    // Test that activity detection works for multiline output
    let adapter = process_contract_adapter(
        "multiline-test",
        "for i in 1 2 3 4 5; do echo \"line $i\"; sleep 0.1; done",
    );
    let mut adapters = HashMap::new();
    adapters.insert("multiline-test".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);

    let adapter_ref = dispatcher.adapter("multiline-test").unwrap().clone();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-multiline-test"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    // Should complete successfully
    assert_eq!(result.exit_code, 0);
    // All lines should be captured
    assert!(result.stdout.contains("line 1"));
    assert!(result.stdout.contains("line 5"));
    // Activity was detected continuously (prevents idle timeout)
    assert!(result.elapsed < Duration::from_secs(10));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_multiple_environment_variables() {
    // Verify that all adapter environment variables are set in the child.
    let mut adapter = process_contract_adapter("multienv", "echo $NDL_A $NDL_B $NDL_C");
    adapter
        .environment
        .insert("NDL_A".to_string(), "alpha".to_string());
    adapter
        .environment
        .insert("NDL_B".to_string(), "beta".to_string());
    adapter
        .environment
        .insert("NDL_C".to_string(), "gamma".to_string());

    let mut adapters = HashMap::new();
    adapters.insert("multienv".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("multienv").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-env-multi"),
            &process_contract_prompt("test"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout.trim(), "alpha beta gamma");
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_json_output_capture_and_token_extraction() {
    // Simulate a claude-like JSON output and verify token extraction works
    // on real process output.
    let json = r#"{"type":"result","result":"done","cost_usd":0.001,"usage":{"input_tokens":1500,"output_tokens":750}}"#;
    let cmd = format!("echo '{json}'");

    let mut adapter = process_contract_adapter("json-agent", &cmd);
    adapter.token_extraction = TokenExtraction::JsonField {
        input_path: "usage.input_tokens".to_string(),
        output_path: "usage.output_tokens".to_string(),
    };

    let mut adapters = HashMap::new();
    adapters.insert("json-agent".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("json-agent").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-json"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);

    // Parse the captured stdout with the token extraction logic.
    let usage = extract_tokens(&adapter.token_extraction, &result.stdout, &result.stderr);
    assert_eq!(usage.input_tokens, Some(1500));
    assert_eq!(usage.output_tokens, Some(750));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_exit_code_2_is_failure() {
    let mut adapters = HashMap::new();
    adapters.insert("f2".to_string(), process_contract_adapter("f2", "exit 2"));
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("f2").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-exit2"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 2);
    assert_eq!(Outcome::classify(result.exit_code, false), Outcome::Failure);
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_exit_code_1_is_failure() {
    let mut adapters = HashMap::new();
    adapters.insert("f1".to_string(), process_contract_adapter("f1", "exit 1"));
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("f1").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-exit1"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 1);
    assert_eq!(Outcome::classify(result.exit_code, false), Outcome::Failure);
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_dispatch_stdin_input_method_delivers_prompt_without_template_redirect(
) {
    let mut adapters = HashMap::new();
    adapters.insert("cat".to_string(), process_contract_adapter("cat", "cat"));
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("cat").unwrap().clone();
    let prompt = "prompt delivered through configured stdin";

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-stdin-method"),
            &process_contract_prompt(prompt),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, prompt);
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_hard_timeout_kills_active_agent() {
    // Test that hard deadline kills the process even when agent is actively producing output.
    // This is the key differentiator from idle timeout - hard timeout is absolute and
    // cannot be reset by activity.
    let adapter = process_contract_adapter(
        "hard-timeout-active",
        // Echo continuously with very short sleep to generate activity
        "while true; do echo 'active output'; sleep 0.05; done",
    );
    let mut adapters = HashMap::new();

    // Configure hard timeout only (no idle timeout)
    let mut adapter_with_hard = adapter.clone();
    adapter_with_hard.timeout_secs = 0; // Disable legacy timeout
    adapter_with_hard.idle_timeout_secs = 0; // No idle timeout
    adapter_with_hard.hard_timeout_secs = 1; // 1 second hard deadline

    adapters.insert("hard-timeout-active".to_string(), adapter_with_hard);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter_ref = dispatcher.adapter("hard-timeout-active").unwrap().clone();

    let start = Instant::now();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-hard-timeout-active"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();
    let wall = start.elapsed();

    // Should be killed by hard timeout
    assert_eq!(result.exit_code, 124, "hard timeout should yield exit 124");

    // Should have a Hard timeout reason
    assert!(
        matches!(result.timeout_reason, Some(TimeoutReason::Hard { .. })),
        "expected Hard timeout reason, got {:?}",
        result.timeout_reason
    );

    // Should have been killed after ~1 second (the hard deadline)
    assert!(
        wall < Duration::from_secs(3),
        "should have been killed by hard deadline after ~1s, took {:?}",
        wall
    );
    assert!(
        wall >= Duration::from_millis(900),
        "should have waited at least ~1s for hard deadline"
    );

    // Should have captured output before being killed (proving activity was happening)
    assert!(result.stdout.contains("active output"));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_hard_timeout_disabled_when_zero() {
    let adapter = process_contract_adapter("hard-disabled", "sleep 0.5");
    let mut adapters = HashMap::new();

    let mut adapter_idle_only = adapter.clone();
    adapter_idle_only.timeout_secs = 0;
    adapter_idle_only.idle_timeout_secs = 10;
    adapter_idle_only.hard_timeout_secs = 0;

    adapters.insert("hard-disabled".to_string(), adapter_idle_only);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter_ref = dispatcher.adapter("hard-disabled").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-hard-disabled"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert!(result.timeout_reason.is_none());
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_dispatch_timeout_returns_124() {
    let mut adapters = HashMap::new();
    let mut adapter = process_contract_adapter("slow", "sleep 100");
    adapter.timeout_secs = 1;
    adapters.insert("slow".to_string(), adapter);

    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("slow").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-timeout"),
            &process_contract_prompt("test"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 124);
    assert!(result.elapsed >= Duration::from_millis(900));
    assert!(result.elapsed < Duration::from_secs(5));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_idle_timeout_resets_on_activity_hard_does_not() {
    // Unit test: idle deadline resets, hard deadline does not.
    let adapter = process_contract_adapter(
        "idle-resets-hard-does-not",
        "for i in $(seq 1 10); do echo \"output $i\"; sleep 0.5; done",
    );
    let mut adapters = HashMap::new();

    let mut adapter_with_both = adapter.clone();
    adapter_with_both.timeout_secs = 0;
    adapter_with_both.idle_timeout_secs = 1;
    adapter_with_both.hard_timeout_secs = 2;

    adapters.insert("idle-resets-hard-does-not".to_string(), adapter_with_both);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter_ref = dispatcher
        .adapter("idle-resets-hard-does-not")
        .unwrap()
        .clone();

    let start = Instant::now();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-idle-resets-hard-does-not"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();
    let wall = start.elapsed();

    assert_eq!(result.exit_code, 124);

    match &result.timeout_reason {
        Some(TimeoutReason::Hard { timeout_secs }) => {
            assert_eq!(*timeout_secs, 2);
        }
        other => panic!("expected Hard timeout reason, got {:?}", other),
    }

    assert!(wall >= Duration::from_millis(1900) && wall < Duration::from_secs(4));
    assert!(result.stdout.contains("output"));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_outer_cancellation_still_kills_process_group() {
    // Regression test for bf-653n7 (the mitosis-evaluation-timeout leak).
    //
    // Worker's mitosis-evaluation step wraps the *entire* dispatch() call
    // in its own, much shorter, `tokio::time::timeout` — separate from
    // and unrelated to the agent's own configured timeout exercised by
    // `e2e_timeout_kills_entire_process_group` above. Before
    // ProcessGroupKillGuard, that outer timeout firing dropped the
    // in-flight dispatch() future before its *internal* timeout-kill
    // match ever ran, silently orphaning the agent process and any
    // process-group children it had spawned — indefinitely, since
    // nothing ever reaped them.
    //
    // Here the adapter's own timeout is set effectively unreachable
    // within the test's window, so the only thing that can kill the
    // process is the guard reacting to the *outer* future being dropped.
    let pid_file =
        std::env::temp_dir().join(format!("needle-outercancel-{}.pid", std::process::id()));
    let pid_file_str = pid_file.display().to_string();

    let cmd = format!("sleep 1000 & echo $! > {pid_file_str}; sleep 1000");
    let mut adapter = process_contract_adapter("outercancel", &cmd);
    adapter.timeout_secs = 3600;

    let mut adapters = HashMap::new();
    adapters.insert("outercancel".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("outercancel").unwrap().clone();

    // Mimic Worker's mitosis-evaluation wrapper: an outer timeout, far
    // shorter than the agent's own, wrapping the whole dispatch call.
    let outer = tokio::time::timeout(
        Duration::from_millis(500),
        dispatcher.dispatch_with_context(
            &BeadId::from("nd-outercancel"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        ),
    )
    .await;
    assert!(
        outer.is_err(),
        "outer timeout should fire well before the adapter's own 3600s timeout"
    );

    let pid_str = std::fs::read_to_string(&pid_file)
        .expect("grandchild PID file should have been written before the outer timeout fired");
    let grandchild_pid: libc::pid_t = pid_str
        .trim()
        .parse()
        .expect("PID file should contain a valid integer PID");

    // Poll until the grandchild is dead or we give up waiting.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let dead = loop {
        let alive = unsafe { libc::kill(grandchild_pid, 0) == 0 };
        if !alive {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        dead,
        "grandchild sleep (pid {grandchild_pid}) should be dead within 3s of the *outer* \
         future being dropped, even though dispatch()'s own internal timeout never fired \
         — this is what ProcessGroupKillGuard exists to guarantee"
    );

    let _ = std::fs::remove_file(&pid_file);
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_dispatch_environment_variables() {
    let mut adapter = process_contract_adapter("env", "echo $NEEDLE_TEST_VAR");
    adapter.environment.insert(
        "NEEDLE_TEST_VAR".to_string(),
        "hello-from-needle".to_string(),
    );
    let mut adapters = HashMap::new();
    adapters.insert("env".to_string(), adapter);

    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("env").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-env"),
            &process_contract_prompt("test"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout.trim(), "hello-from-needle");
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_timestamps_before_parsing() {
    // Test that activity timestamps are recorded before newline parsing
    // This test verifies the structural requirement from the acceptance criteria
    let adapter =
        process_contract_adapter("timestamp-order-test", "printf 'line1\\nline2\\nline3'");
    let mut adapters = HashMap::new();
    adapters.insert("timestamp-order-test".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);

    let adapter_ref = dispatcher.adapter("timestamp-order-test").unwrap().clone();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-timestamp-order-test"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    // Should complete successfully
    assert_eq!(result.exit_code, 0);
    // All lines captured (proving parsing happened after activity detection)
    assert!(result.stdout.contains("line1"));
    assert!(result.stdout.contains("line2"));
    assert!(result.stdout.contains("line3"));
    // Fast completion proves activity was detected continuously
    assert!(result.elapsed < Duration::from_secs(1));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_on_partial_chunks() {
    // Verify that partial reads (chunks < 8192 bytes) still register activity.
    // The real reader reads in chunks; we verify small chunks are detected.
    let mut adapter = process_contract_adapter(
        "small-chunks",
        // Emit small amounts of output with delays
        "for i in $(seq 1 15); do echo -n x; sleep 0.12; done; echo",
    );
    adapter.idle_timeout_secs = 1;
    adapter.hard_timeout_secs = 10;

    let mut adapters = HashMap::new();
    adapters.insert("small-chunks".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("small-chunks").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-activity-chunks"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(
        result.exit_code, 0,
        "small chunk writes should prevent idle timeout"
    );
    // 15 iterations * 0.12s = 1.8s
    assert!(result.elapsed >= Duration::from_millis(1700));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_timeout_kills_agent_returns_124() {
    let mut adapter = process_contract_adapter("sleeper", "sleep 100");
    adapter.timeout_secs = 1;

    let mut adapters = HashMap::new();
    adapters.insert("sleeper".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("sleeper").unwrap().clone();

    let start = Instant::now();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-timeout"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();
    let wall = start.elapsed();

    assert_eq!(result.exit_code, 124, "timeout should yield exit 124");
    assert!(
        wall < Duration::from_secs(5),
        "should have been killed after ~1s, took {:?}",
        wall
    );
    assert!(
        result.elapsed >= Duration::from_millis(900),
        "should have waited at least ~1s"
    );
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_dispatch_cleans_up_temp_file() {
    let bead_id = BeadId::from("nd-cleanup");
    let mut adapters = HashMap::new();
    adapters.insert("true".to_string(), process_contract_adapter("true", "true"));
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("true").unwrap().clone();

    let _ = dispatcher
        .dispatch_with_context(
            &bead_id,
            &process_contract_prompt("cleanup test"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    // Verify the temp file was cleaned up.
    let expected_path = std::env::temp_dir().join("needle").join(format!(
        "prompt-{}-{}.md",
        bead_id,
        std::process::id()
    ));
    assert!(!expected_path.exists(), "temp file should be cleaned up");
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_timestamp_tracked_per_process() {
    // Verify that activity tracking is isolated per process execution.
    // Two sequential dispatches should have independent activity timestamps.
    let mut adapter = process_contract_adapter("timestamped", "echo output-$(date +%s%N)");
    adapter.idle_timeout_secs = 1;

    let mut adapters = HashMap::new();
    adapters.insert("timestamped".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);

    // First dispatch
    let adapter1 = dispatcher.adapter("timestamped").unwrap().clone();
    let result1 = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-timestamp-1"),
            &process_contract_prompt("t"),
            &adapter1,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result1.exit_code, 0);

    // Small delay to ensure different timestamp
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second dispatch
    let adapter2 = dispatcher.adapter("timestamped").unwrap().clone();
    let result2 = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-timestamp-2"),
            &process_contract_prompt("t"),
            &adapter2,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result2.exit_code, 0);
    // Both should succeed independently
    assert!(result1.stdout.contains("output-"));
    assert!(result2.stdout.contains("output-"));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_on_stderr_output() {
    // Test that activity detection works for stderr output
    let adapter = process_contract_adapter("stderr-test", "echo 'error message' >&2");
    let mut adapters = HashMap::new();
    adapters.insert("stderr-test".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);

    let adapter_ref = dispatcher.adapter("stderr-test").unwrap().clone();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-stderr-test"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    // Should complete successfully
    assert_eq!(result.exit_code, 0);
    assert!(result.stderr.contains("error message"));
    // Activity was detected on stderr
    assert!(result.elapsed < Duration::from_secs(10));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_on_chunked_output() {
    // Test that activity detection works when output comes in chunks
    let adapter = process_contract_adapter(
        "chunked-test",
        "echo 'chunk1'; sleep 0.2; echo 'chunk2'; sleep 0.2; echo 'chunk3'",
    );
    let mut adapters = HashMap::new();
    adapters.insert("chunked-test".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);

    let adapter_ref = dispatcher.adapter("chunked-test").unwrap().clone();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-chunked-test"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    // Should complete successfully
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("chunk1"));
    assert!(result.stdout.contains("chunk2"));
    assert!(result.stdout.contains("chunk3"));
    // Activity was detected on each chunk
    assert!(result.elapsed < Duration::from_secs(10));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_prompt_with_shell_metacharacters() {
    // Verify that shell metacharacters in the prompt body are safely
    // delivered via the temp file without shell injection or corruption.
    let dangerous_prompt = "Hello $USER\nLine with `backticks`\nQuotes: 'single' \"double\"\nBackslash: \\\nDollar: $(echo injected)";

    let mut adapters = HashMap::new();
    adapters.insert(
        "catprompt".to_string(),
        process_contract_adapter("catprompt", "cat {prompt_file}"),
    );
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("catprompt").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-meta"),
            &process_contract_prompt(dangerous_prompt),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    // The prompt file content should be the exact string, not shell-expanded.
    assert!(
        result.stdout.contains("$USER"),
        "shell variable should be literal, not expanded"
    );
    assert!(
        result.stdout.contains("`backticks`"),
        "backticks should be preserved"
    );
    assert!(
        result.stdout.contains("$(echo injected)"),
        "command substitution should be literal"
    );
    assert!(
        result.stdout.contains("'single'"),
        "single quotes should be preserved"
    );
    assert!(
        result.stdout.contains("\"double\""),
        "double quotes should be preserved"
    );
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_on_stderr_resets_idle_timeout() {
    // Verify that ongoing stderr output also prevents idle timeout.
    let mut adapter = process_contract_adapter(
        "chatty-stderr",
        "for i in $(seq 1 10); do echo -n . >&2; sleep 0.2; done; sleep 0.1",
    );
    adapter.idle_timeout_secs = 1;
    adapter.hard_timeout_secs = 10;

    let mut adapters = HashMap::new();
    adapters.insert("chatty-stderr".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("chatty-stderr").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-activity-stderr"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(
        result.exit_code, 0,
        "process with continuous stderr should not idle timeout"
    );
    assert!(result.elapsed >= Duration::from_millis(1900));
    assert!(
        result.stderr.contains(".........."),
        "should capture all stderr output"
    );
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_idle_timeout_fires_when_no_activity() {
    // Verify that idle timeout DOES fire when there's no output.
    // This is the negative case proving activity detection works.
    let mut adapter = process_contract_adapter(
        "silent-process",
        // Sleep for 5 seconds without any output
        "sleep 5",
    );
    adapter.timeout_secs = 0; // Use new timeout mode, not legacy
    adapter.idle_timeout_secs = 1; // Should fire after 1s of silence

    let mut adapters = HashMap::new();
    adapters.insert("silent-process".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("silent-process").unwrap().clone();

    let start = Instant::now();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-idle-timeout"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(
        result.exit_code, 124,
        "idle timeout should return exit code 124"
    );
    assert!(
        result.timeout_reason.is_some(),
        "should have timeout reason"
    );
    match result.timeout_reason {
        Some(TimeoutReason::Idle { timeout_secs, .. }) => {
            assert_eq!(timeout_secs, 1);
        }
        _ => panic!(
            "expected Idle timeout reason, got {:?}",
            result.timeout_reason
        ),
    }
    // Should fire around 1s (allowing for scheduling overhead)
    assert!(elapsed >= Duration::from_millis(900));
    assert!(elapsed < Duration::from_secs(3));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_hard_timeout_shorter_than_idle_timeout() {
    // Integration test: hard timeout fires before idle timeout even when agent is active.
    let adapter = process_contract_adapter(
        "hard-before-idle",
        "while true; do echo 'continuous activity'; sleep 0.1; done",
    );
    let mut adapters = HashMap::new();

    let mut adapter_with_both = adapter.clone();
    adapter_with_both.timeout_secs = 0;
    adapter_with_both.idle_timeout_secs = 5;
    adapter_with_both.hard_timeout_secs = 1;

    adapters.insert("hard-before-idle".to_string(), adapter_with_both);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter_ref = dispatcher.adapter("hard-before-idle").unwrap().clone();

    let start = Instant::now();
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-hard-before-idle"),
            &process_contract_prompt("test"),
            &adapter_ref,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();
    let wall = start.elapsed();

    assert_eq!(result.exit_code, 124);

    match &result.timeout_reason {
        Some(TimeoutReason::Hard { timeout_secs }) => {
            assert_eq!(*timeout_secs, 1);
        }
        other => panic!("expected Hard timeout reason, got {:?}", other),
    }

    assert!(wall < Duration::from_secs(3));
    assert!(result.stdout.contains("continuous activity"));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_timeout_kills_entire_process_group() {
    // Verify that on timeout the entire process group (not just the direct
    // bash child) is killed.  The agent starts a background sleep and writes
    // its PID to a temp file before blocking.  After timeout we assert the
    // grandchild is gone.
    let pid_file = std::env::temp_dir().join(format!("needle-pgkill-{}.pid", std::process::id()));
    let pid_file_str = pid_file.display().to_string();

    // Start a background sleep, capture its PID, then sleep (will time out).
    let cmd = format!("sleep 1000 & echo $! > {pid_file_str}; sleep 1000");

    let mut adapter = process_contract_adapter("pgkill", &cmd);
    adapter.timeout_secs = 2;

    let mut adapters = HashMap::new();
    adapters.insert("pgkill".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("pgkill").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-pgkill"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 124, "timeout should yield 124");

    // The grandchild PID file must exist — echo runs in milliseconds, well
    // within the 2-second timeout window.
    let pid_str = std::fs::read_to_string(&pid_file)
        .expect("grandchild PID file should have been written before timeout fired");
    let grandchild_pid: libc::pid_t = pid_str
        .trim()
        .parse()
        .expect("PID file should contain a valid integer PID");

    // Poll until the grandchild is dead or we time out waiting.  SIGKILL
    // delivery and OS reaping can be slow in container environments.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let dead = loop {
        let alive = unsafe { libc::kill(grandchild_pid, 0) == 0 };
        if !alive {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        dead,
        "grandchild sleep (pid {grandchild_pid}) should be dead within 3s after killpg"
    );

    let _ = std::fs::remove_file(&pid_file);
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_dispatch_echo_captures_stdout() {
    let mut adapters = HashMap::new();
    adapters.insert(
        "echo".to_string(),
        process_contract_adapter("echo", "echo hello-needle"),
    );
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("echo").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-echo"),
            &process_contract_prompt("test"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout.trim(), "hello-needle");
    assert!(result.pid > 0);
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_with_mixed_stdout_stderr() {
    // Verify that both stdout and stderr activity reset the idle timer.
    let mut adapter = process_contract_adapter(
        "mixed-streams",
        // Alternate between stdout and stderr output
        "for i in $(seq 1 8); do echo -n out >&1; echo -n err >&2; sleep 0.18; done; echo done",
    );
    adapter.idle_timeout_secs = 1;
    adapter.hard_timeout_secs = 10;

    let mut adapters = HashMap::new();
    adapters.insert("mixed-streams".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("mixed-streams").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-activity-mixed"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(
        result.exit_code, 0,
        "mixed stdout/stderr should prevent idle timeout"
    );
    // 8 iterations * 0.18s ≈ 1.44s, plus overhead
    assert!(result.elapsed >= Duration::from_millis(1300));
    assert!(result.stdout.contains("outoutoutoutoutoutoutout"));
    assert!(result.stderr.contains("errerrerrerrerrerrerrerr"));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_activity_detection_happens_before_newline_parsing() {
    // Verify that activity is detected on every byte read, before newline parsing.
    // Output many bytes without newlines, then a newline at the end.
    let mut adapter = process_contract_adapter(
        "no-newlines",
        // Emit 1000 characters without newlines, sleep 200ms between chunks
        "printf '%0.s#' {1..1000}; sleep 0.2; echo done",
    );
    adapter.idle_timeout_secs = 1;
    adapter.hard_timeout_secs = 10;

    let mut adapters = HashMap::new();
    adapters.insert("no-newlines".to_string(), adapter);
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("no-newlines").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-activity-nonewline"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(
        result.exit_code, 0,
        "output without newlines should still reset idle timer"
    );
    // The initial printf is fast, but the 200ms sleep should extend execution
    assert!(result.elapsed >= Duration::from_millis(150));
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_dispatch_captures_exit_code() {
    let mut adapters = HashMap::new();
    adapters.insert(
        "fail".to_string(),
        process_contract_adapter("fail", "exit 42"),
    );
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("fail").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-exit"),
            &process_contract_prompt("test"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 42);
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_e2e_exit_code_0_is_success() {
    let mut adapters = HashMap::new();
    adapters.insert("ok".to_string(), process_contract_adapter("ok", "exit 0"));
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("ok").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-exit0"),
            &process_contract_prompt("t"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(Outcome::classify(result.exit_code, false), Outcome::Success);
}

#[tokio::test]
async fn dispatch_telemetry_process_contracts_dispatch_missing_binary_returns_127() {
    let mut adapters = HashMap::new();
    adapters.insert(
        "missing".to_string(),
        process_contract_adapter("missing", "nonexistent-binary-xyz-12345"),
    );
    let dispatcher = process_contract_dispatcher(adapters);
    let adapter = dispatcher.adapter("missing").unwrap().clone();

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("nd-missing"),
            &process_contract_prompt("test"),
            &adapter,
            Path::new("/tmp"),
            &crate::pre_spawn_pass_store::claimed_context(Path::new("/tmp")),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 127);
}

#[test]
fn dispatch_telemetry_process_contracts_hook_sink_dispatches_json_to_stdin() {
    let tmp = std::env::temp_dir().join("needle-hook-test-stdin");
    let _ = std::fs::remove_file(&tmp);

    let cmd = format!("cat > {}", tmp.display());
    let configs = vec![HookConfig {
        event_filter: "worker.*".to_string(),
        command: cmd,
        url: None,
    }];
    let sink = HookSink::new(&configs).unwrap();

    let event = process_contract_event("worker.started");
    let failures = sink.dispatch(&event);
    assert!(failures.is_empty());

    // Give the child process a moment to write
    std::thread::sleep(std::time::Duration::from_millis(200));

    let content = std::fs::read_to_string(&tmp).unwrap_or_default();
    assert!(
        !content.is_empty(),
        "hook should have received JSON on stdin"
    );
    // Verify it's valid JSON containing the event type
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["event_type"], "worker.started");

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn dispatch_telemetry_process_contracts_hook_sink_dispatch_captures_failure() {
    let configs = vec![HookConfig {
        event_filter: "bead.*".to_string(),
        command: "/nonexistent/command/that/does/not/exist".to_string(),
        url: None,
    }];
    let sink = HookSink::new(&configs).unwrap();

    let event = process_contract_event("bead.completed");
    let failures = sink.dispatch(&event);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].event_type, "telemetry.sink_error");
    assert!(failures[0].data["hook_command"]
        .as_str()
        .unwrap()
        .contains("nonexistent"));
}

#[test]
fn dispatch_telemetry_process_contracts_hook_sink_dispatch_matches_filter() {
    let configs = vec![HookConfig {
        event_filter: "outcome.*".to_string(),
        command: "true".to_string(), // always succeeds
        url: None,
    }];
    let sink = HookSink::new(&configs).unwrap();

    // Matching event — should dispatch (no failures expected)
    let event = process_contract_event("outcome.handled");
    let failures = sink.dispatch(&event);
    assert!(failures.is_empty());
}

#[test]
fn dispatch_telemetry_process_contracts_hook_sink_multiple_hooks_matching_same_event() {
    let configs = vec![
        HookConfig {
            event_filter: "outcome.*".to_string(),
            command: "true".to_string(),
            url: None,
        },
        HookConfig {
            event_filter: "outcome.handled".to_string(),
            command: "true".to_string(),
            url: None,
        },
    ];
    let sink = HookSink::new(&configs).unwrap();

    let event = process_contract_event("outcome.handled");
    let failures = sink.dispatch(&event);
    // Both hooks match, both succeed — no failures
    assert!(failures.is_empty());
}

fn resolution_process_contracts_complete() -> ResolveDecision {
    ResolveDecision::Complete {
        evidence: "verified the requested change".to_string(),
        commit_message: "fix: the thing".to_string(),
    }
}

fn resolution_process_contracts_workspace(commands: &[String]) -> (TempDir, Bead) {
    let workspace = TempDir::new().expect("create gate workspace");
    let yaml = serde_yaml::to_string(&serde_json::json!({ "verification": commands }))
        .expect("serialize gate fixture");
    fs::write(workspace.path().join(".needle.yaml"), yaml).expect("write gate fixture");
    let bead = test_bead(workspace.path(), BeadStatus::InProgress);
    (workspace, bead)
}

fn resolution_process_contracts_output(exit_code: i32) -> AgentOutcome {
    AgentOutcome {
        exit_code,
        stdout: String::new(),
        stderr: String::new(),
    }
}

fn resolution_process_contracts_handler(config: Config) -> OutcomeHandler {
    OutcomeHandler::new(
        config,
        Telemetry::new("resolution-process-contracts".to_string()),
    )
}

fn resolution_process_contracts_backend(
    name: &str,
    binary: &str,
    executable: PathBuf,
    identity_pattern: &str,
) -> BeadBackend {
    BeadBackend {
        name: name.to_string(),
        binary: binary.to_string(),
        detect_paths: vec![executable],
        identity_pattern: identity_pattern.to_string(),
        version_command: vec!["--version".to_string()],
        verified_against: format!("{binary} test fixture"),
        verified_on: "2026-09-14".to_string(),
        operations: HashMap::new(),
        capabilities: Default::default(),
        quirks: Vec::new(),
        error_markers: Default::default(),
    }
}

fn resolution_process_contracts_executable(root: &Path, name: &str, body: &str) -> PathBuf {
    let executable = archive_and_status_process_contracts_executable(root, name, body);
    for attempt in 0..8 {
        match spawn_version_output(&executable) {
            Ok(_) => return executable,
            Err(error)
                if attempt + 1 < 8
                    && error.chain().any(|cause| {
                        cause
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|source| source.raw_os_error() == Some(26))
                    }) => {}
            Err(error) => panic!("published executable fixture was not ready: {error:#}"),
        }
    }
    unreachable!("fixture readiness loop returns or panics on its final attempt")
}

fn resolution_process_contracts_context<'a>(bead: &'a Bead) -> ResolveContext<'a> {
    ResolveContext::new(
        bead,
        1,
        "stdout".to_string(),
        "stderr".to_string(),
        Duration::from_secs(60),
        Utc::now(),
        false,
    )
}

#[tokio::test]
async fn resolution_process_contracts_complete_rejects_failed_and_missing_gates() {
    for command in ["false", "/nonexistent/needle-missing-gate-cmd"] {
        let (_workspace, bead) = resolution_process_contracts_workspace(&[command.to_string()]);
        let store = TestStore::new(bead.clone()).with_notes("did the work");
        let executor = DecisionExecutor::new(
            Config::default(),
            Telemetry::new("resolution-gate-rejection".to_string()),
        );

        let applied = executor
            .apply(
                &store,
                &bead,
                &resolution_process_contracts_complete(),
                "test-worker",
                None,
            )
            .await
            .expect("judged gate rejection should release");

        assert_eq!(applied, AppliedDecision::Released(ReleaseCause::Rejected));
        let actions = store.actions();
        assert!(actions.iter().any(|action| action.starts_with("release:")));
        assert!(actions
            .iter()
            .any(|action| action.contains("failure-count:1")));
        assert!(!actions.iter().any(|action| action.starts_with("close:")));
    }
}

#[tokio::test]
async fn resolution_process_contracts_unverifiable_shipped_work_is_not_penalized() {
    let repo = GitRepo::new();
    let base = repo.head();
    repo.commit("src.rs", "fn main() {}\n", "real work");
    let bead = test_bead(repo.path(), BeadStatus::InProgress);
    let store = TestStore::new(bead.clone());
    let fallback = PreDispatch {
        head_sha: Some(base),
        notes_hash: None,
        dirty_files: Vec::new(),
        captured_at: Some(Utc::now()),
    };
    let executor = DecisionExecutor::new(
        Config::default(),
        Telemetry::new("resolution-unverifiable".to_string()),
    );

    let applied = executor
        .apply(
            &store,
            &bead,
            &resolution_process_contracts_complete(),
            "test-worker",
            Some(&fallback),
        )
        .await
        .expect("unverifiable shipped work should release");

    assert_eq!(
        applied,
        AppliedDecision::Released(ReleaseCause::Unverifiable)
    );
    assert!(store
        .actions()
        .iter()
        .all(|action| !action.contains("failure-count")));
}

#[serial_test::serial]
#[tokio::test]
async fn resolution_process_contracts_resolve_agent_timeout_is_bounded() {
    let root = TempDir::new().expect("create resolver fixture");
    resolution_process_contracts_executable(
        root.path(),
        "claude",
        "#!/bin/sh\nsleep 5\necho unreachable\n",
    );
    let _path = PathGuard::prepend(root.path());
    let resolver = Resolver::with_config(
        PromptBuilder::new(&needle::config::PromptConfig::default()),
        ResolveConfig {
            timeout_secs: 1,
            ..ResolveConfig::default()
        },
    );
    let started = Instant::now();
    let error = resolver
        .invoke_resolve_agent("bounded timeout fixture")
        .await
        .expect_err("slow agent must time out");
    assert!(error.to_string().contains("timed out"), "{error:#}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[serial_test::serial]
#[tokio::test]
async fn resolution_process_contracts_resolver_timeout_returns_safe_retry() {
    let root = TempDir::new().expect("create resolver fixture");
    resolution_process_contracts_executable(
        root.path(),
        "claude",
        "#!/bin/sh\nsleep 5\necho unreachable\n",
    );
    let _path = PathGuard::prepend(root.path());
    let bead = test_bead(root.path(), BeadStatus::InProgress);
    let resolver = Resolver::with_config(
        PromptBuilder::new(&needle::config::PromptConfig::default()),
        ResolveConfig {
            timeout_secs: 1,
            ..ResolveConfig::default()
        },
    );

    let decision = resolver
        .resolve(&resolution_process_contracts_context(&bead))
        .await;
    match decision {
        ResolveDecision::Retry { evidence, strategy } => {
            assert!(evidence.contains("agent_invocation_failed"), "{evidence}");
            assert_eq!(strategy, "same");
        }
        other => panic!("expected safe retry, got {other:?}"),
    }
}

#[tokio::test]
async fn resolution_process_contracts_prompt_failure_returns_safe_retry() {
    let workspace = TempDir::new().expect("create resolver workspace");
    let bead = test_bead(workspace.path(), BeadStatus::InProgress);
    let resolver = Resolver::with_config(
        PromptBuilder::new(&needle::config::PromptConfig::default()),
        ResolveConfig {
            use_default_template: false,
            custom_template_path: None,
            ..ResolveConfig::default()
        },
    );

    let decision = resolver
        .resolve(&resolution_process_contracts_context(&bead))
        .await;
    match decision {
        ResolveDecision::Retry { evidence, .. } => {
            assert!(evidence.contains("prompt_build_failed"), "{evidence}");
        }
        other => panic!("expected safe retry, got {other:?}"),
    }
}

#[test]
fn resolution_process_contracts_binary_identity_accepts_match_and_rejects_shims() {
    let root = TempDir::new().expect("create identity fixtures");
    let matching = resolution_process_contracts_executable(
        root.path(),
        "correct-bead",
        "#!/bin/sh\necho 'bead 0.2.6'\n",
    );
    let wrong = resolution_process_contracts_executable(
        root.path(),
        "wrong-bead",
        "#!/bin/sh\necho 'bf 0.4.1'\n",
    );
    let reverse = resolution_process_contracts_executable(
        root.path(),
        "wrong-bf",
        "#!/bin/sh\necho 'bead 0.2.6'\n",
    );
    let alien = resolution_process_contracts_executable(
        root.path(),
        "alien-bead",
        "#!/bin/sh\necho 'wrong-identity 1.0.0'\n",
    );
    let prompt = || PromptBuilder::new(&needle::config::PromptConfig::default());

    let matching = Resolver::new(prompt()).with_backend(resolution_process_contracts_backend(
        "bead-rs",
        "correct-bead",
        matching,
        r"^bead\s",
    ));
    assert!(matching.verify_binary_identity_before_agent().is_ok());

    let mismatched = Resolver::new(prompt()).with_backend(resolution_process_contracts_backend(
        "bead-rs",
        "wrong-bead",
        wrong,
        r"^bead\s",
    ));
    match mismatched.verify_binary_identity_before_agent() {
        Err(VerificationError::VerificationFailed(message)) => {
            assert!(message.contains("bf"), "{message}");
            assert!(message.contains("bead-rs"), "{message}");
            assert!(message.contains("mismatch") || message.contains("pattern"));
        }
        other => panic!("expected actionable identity mismatch, got {other:?}"),
    }

    let reverse = Resolver::new(prompt()).with_backend(resolution_process_contracts_backend(
        "bead-forge",
        "wrong-bf",
        reverse,
        r"^bf\s",
    ));
    match reverse.verify_binary_identity_before_agent() {
        Err(VerificationError::VerificationFailed(message)) => {
            assert!(message.contains("bead"), "{message}");
            assert!(message.contains("bead-forge"), "{message}");
            assert!(message.contains("mismatch") || message.contains("pattern"));
        }
        other => panic!("expected reverse identity mismatch, got {other:?}"),
    }

    let alien = Resolver::new(prompt()).with_backend(resolution_process_contracts_backend(
        "bead-rs",
        "alien-bead",
        alien,
        r"^bead\s",
    ));
    match alien.verify_binary_identity_before_agent() {
        Err(VerificationError::VerificationFailed(message)) => {
            assert!(message.contains("wrong-identity"), "{message}");
            assert!(message.contains("bead-rs"), "{message}");
            assert!(
                message.len() > 40,
                "diagnostic is not actionable: {message}"
            );
            assert!(
                message.contains("claims")
                    || message.contains("expected")
                    || message.contains("normalized")
                    || message.contains("pattern"),
                "diagnostic is not actionable: {message}"
            );
        }
        other => panic!("expected actionable wrong-identity error, got {other:?}"),
    }

    let missing = Resolver::new(prompt()).with_backend(resolution_process_contracts_backend(
        "bead-rs",
        "missing",
        root.path().join("absent"),
        r"^bead\s",
    ));
    assert!(matches!(
        missing.verify_binary_identity_before_agent(),
        Err(VerificationError::NotSupported(message)) if message.contains("not found")
    ));
}

#[serial_test::serial]
#[tokio::test]
async fn resolution_process_contracts_identity_failure_precedes_agent_invocation() {
    let root = TempDir::new().expect("create identity fixtures");
    let marker = root.path().join("agent-ran");
    resolution_process_contracts_executable(
        root.path(),
        "claude",
        &format!("#!/bin/sh\ntouch {}\n", marker.display()),
    );
    fs::remove_file(&marker).expect("clear fixture-readiness marker");
    let wrong = resolution_process_contracts_executable(
        root.path(),
        "wrong-bead",
        "#!/bin/sh\necho 'bf 0.4.1'\n",
    );
    let _path = PathGuard::prepend(root.path());
    let bead = test_bead(root.path(), BeadStatus::InProgress);
    let resolver =
        Resolver::new(PromptBuilder::new(&needle::config::PromptConfig::default())).with_backend(
            resolution_process_contracts_backend("bead-rs", "wrong-bead", wrong, r"^bead\s"),
        );

    let decision = resolver
        .resolve(&resolution_process_contracts_context(&bead))
        .await;
    assert!(matches!(decision, ResolveDecision::Retry { .. }));
    assert!(!marker.exists(), "agent ran before identity rejection");
}

#[serial_test::serial]
#[tokio::test]
async fn resolution_process_contracts_outcome_rejects_and_reopens_failed_gates() {
    let home = TempDir::new().expect("create isolated HOME");
    let _home = HomeGuard::set(home.path());
    for status in [BeadStatus::InProgress, BeadStatus::Done] {
        let (_workspace, bead) = resolution_process_contracts_workspace(&["false".to_string()]);
        let store = TestStore::new(test_bead(bead.workspace.as_path(), status.clone()));
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let result = resolution_process_contracts_handler(config)
            .handle(
                &store,
                &bead,
                &resolution_process_contracts_output(0),
                false,
            )
            .await
            .expect("route failed verification");

        assert_eq!(result.outcome, Outcome::Failure);
        assert!(matches!(result.bead_action, BeadAction::Released(_)));
        assert!(!result
            .telemetry_events
            .iter()
            .any(|event| matches!(event, EventKind::BeadOrphaned { .. })));
        let actions = store.actions();
        assert!(actions.iter().any(|action| action == "show"));
        assert!(actions
            .iter()
            .any(|action| action.contains("verification-failed")));
        assert!(actions
            .iter()
            .any(|action| action.contains("failure-count:1")));
        if status == BeadStatus::Done {
            assert!(actions.iter().any(|action| action.starts_with("reopen:")));
        }
    }
}

#[serial_test::serial]
#[tokio::test]
async fn resolution_process_contracts_outcome_short_circuits_workspace_gates() {
    let home = TempDir::new().expect("create isolated HOME");
    let _home = HomeGuard::set(home.path());
    let workspace = TempDir::new().expect("create gate workspace");
    fs::write(
        workspace.path().join(".needle.yaml"),
        "gates:\n  - type: command\n    run_in: workspace\n    commands:\n      - touch first-ran\n      - exit 42\n      - touch must-not-run\n",
    )
    .expect("write gate config");
    let bead = test_bead(workspace.path(), BeadStatus::InProgress);
    let store = TestStore::new(bead.clone());
    let mut config = Config::default();
    config.worker.enforce_shipped_work = false;

    let result = resolution_process_contracts_handler(config)
        .handle(
            &store,
            &bead,
            &resolution_process_contracts_output(0),
            false,
        )
        .await
        .expect("route workspace gates");

    assert!(matches!(result.bead_action, BeadAction::Released(_)));
    assert!(workspace.path().join("first-ran").exists());
    assert!(!workspace.path().join("must-not-run").exists());
}

#[serial_test::serial]
#[tokio::test]
async fn resolution_process_contracts_outcome_runs_missing_path_for_its_verdict() {
    let home = TempDir::new().expect("create isolated HOME");
    let _home = HomeGuard::set(home.path());
    let workspace = TempDir::new().expect("create gate workspace");
    fs::write(
        workspace.path().join(".needle.yaml"),
        "gates:\n  - type: command\n    run_in: workspace\n    commands:\n      - scripts/definition-of-done.sh --fast\n",
    )
    .expect("write gate config");
    let bead = test_bead(workspace.path(), BeadStatus::InProgress);
    let mut config = Config::default();
    config.worker.enforce_shipped_work = false;

    let store = TestStore::new(bead.clone());
    let result = resolution_process_contracts_handler(config)
        .handle(
            &store,
            &bead,
            &resolution_process_contracts_output(0),
            false,
        )
        .await
        .expect("resolve workspace gates");

    assert_eq!(result.outcome, Outcome::Failure);
    assert!(matches!(result.bead_action, BeadAction::Released(_)));
    assert!(store
        .actions()
        .iter()
        .any(|action| action.contains("verification-failed")));
}

/// N-T46 (ADR-030): a split attempt whose workspace gate really ran and passed
/// resolves `decomposed` in the ledger, and the row keeps that gate evidence;
/// only the semantic outcome moves.
#[serial_test::serial]
#[tokio::test]
async fn nt46_split_template_success_resolves_decomposed_and_keeps_gate_results() {
    let home = TempDir::new().expect("create isolated HOME");
    let _home = HomeGuard::set(home.path());
    let workspace = TempDir::new().expect("create gate workspace");
    fs::write(
        workspace.path().join(".needle.yaml"),
        "gates:\n  - type: command\n    run_in: workspace\n    commands:\n      - \"true\"\n",
    )
    .expect("write gate config");
    let bead = test_bead(workspace.path(), BeadStatus::InProgress);
    let store = TestStore::new(test_bead(workspace.path(), BeadStatus::Done));
    let mut config = Config::default();
    config.worker.enforce_shipped_work = false;
    let log_dir = home.path().join("telemetry");
    let telemetry = Telemetry::with_log_dir("nt46-split-gates".to_string(), &log_dir);
    telemetry.start();
    let handler = OutcomeHandler::new(config, telemetry.clone());
    handler.set_attempt_context(AttemptContext {
        prompt_template: "split".to_string(),
        template_version: "split-default".to_string(),
        ..AttemptContext::default()
    });

    let result = handler
        .handle(
            &store,
            &bead,
            &resolution_process_contracts_output(0),
            false,
        )
        .await
        .expect("route split outcome");
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .expect("flush telemetry");

    assert_eq!(result.outcome, Outcome::Success);
    let rows: Vec<TelemetryEvent> = telemetry_events(&log_dir)
        .into_iter()
        .filter(|event| event.event_type == "attempt.resolved")
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].data["outcome"], "decomposed");
    assert_eq!(rows[0].data["terminal_reason"], "decomposed:split_template");
    let gates = rows[0].data["gate_results"]
        .as_array()
        .expect("gate_results array");
    assert!(
        !gates.is_empty() && gates.iter().all(|gate| gate["status"] == "pass"),
        "the passing gate evidence must be kept: {gates:?}"
    );
    telemetry.shutdown().await;
}

#[serial_test::serial]
#[tokio::test]
async fn resolution_process_contracts_outcome_timeout_kills_gate_child() {
    let home = TempDir::new().expect("create isolated HOME");
    let _home = HomeGuard::set(home.path());
    let marker = home.path().join("late-marker");
    let (_workspace, bead) =
        resolution_process_contracts_workspace(&[format!("sleep 3 && touch {}", marker.display())]);
    let mut config = Config::default();
    config.worker.enforce_shipped_work = false;
    config.validation = ValidationConfig {
        outcome_timeout_seconds: 1,
        ..ValidationConfig::default()
    };
    let store = TestStore::new(bead.clone());
    let started = Instant::now();

    let result = resolution_process_contracts_handler(config)
        .handle_with_cancellation(
            &store,
            &bead,
            &resolution_process_contracts_output(0),
            false,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("timeout outcome handler");

    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(result.bead_action, BeadAction::Errored);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        !marker.exists(),
        "timed-out gate child survived cancellation"
    );
}

#[serial_test::serial]
#[tokio::test]
async fn resolution_process_contracts_attempt_ledger_is_once_per_terminal_path() {
    let home = TempDir::new().expect("create isolated HOME");
    let _home = HomeGuard::set(home.path());
    for (case, exit_code, interrupted, gates) in [
        ("success", 0, false, Vec::<String>::new()),
        ("failure", 1, false, Vec::<String>::new()),
        ("interrupted", 130, true, Vec::<String>::new()),
        ("gate-failure", 0, false, vec!["false".to_string()]),
    ] {
        let log_dir = home.path().join(case);
        let telemetry = Telemetry::with_log_dir(format!("resolution-{case}"), &log_dir);
        telemetry.start();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let handler = OutcomeHandler::new(config, telemetry.clone());
        let (_workspace, bead) = resolution_process_contracts_workspace(&gates);
        let store_status = if exit_code == 0 && gates.is_empty() {
            BeadStatus::Done
        } else {
            BeadStatus::InProgress
        };
        let store = TestStore::new(test_bead(&bead.workspace, store_status));

        let result = handler
            .handle(
                &store,
                &bead,
                &resolution_process_contracts_output(exit_code),
                interrupted,
            )
            .await
            .expect("route terminal outcome");
        telemetry
            .force_flush_async(Duration::from_secs(2))
            .await
            .expect("flush terminal ledger");
        let rows: Vec<_> = telemetry_events(&log_dir)
            .into_iter()
            .filter(|event| event.event_type == "attempt.resolved")
            .collect();
        assert_eq!(rows.len(), 1, "{case} emitted {} rows", rows.len());
        assert_eq!(rows[0].data["schema_version"], 2);
        assert_eq!(rows[0].data["provisional"], true);
        assert!(rows[0].data["context_manifest_hash"].is_null());
        if case == "gate-failure" {
            assert_eq!(rows[0].data["outcome"], "work_failure");
            assert!(rows[0].data["gate_results"]
                .as_array()
                .is_some_and(|results| !results.is_empty()));
            assert_eq!(result.outcome, Outcome::Failure);
        }
        telemetry.shutdown().await;
    }
}

#[tokio::test]
async fn resolution_process_contracts_mitosis_timeout_context_round_trips_and_clears() {
    let workspace = TempDir::new().expect("create timeout context workspace");
    let bead = test_bead(workspace.path(), BeadStatus::InProgress);
    let context = capture_timeout_context(
        &bead,
        workspace.path(),
        TimeoutEligibility::Eligible {
            reason: "agent wall-clock timeout".to_string(),
        },
        3600,
    )
    .await
    .expect("capture timeout context")
    .expect("eligible timeout yields context");

    write_timeout_context(workspace.path(), &bead.id, &context)
        .await
        .expect("write timeout context");
    let loaded = load_timeout_context(workspace.path(), &bead.id)
        .await
        .expect("load timeout context");
    assert_eq!(loaded.bead_def.bead_id, bead.id);
    assert!(loaded.qualifies_for_mitosis);

    clear_timeout_context(workspace.path(), &bead.id).await;
    assert!(load_timeout_context(workspace.path(), &bead.id)
        .await
        .is_none());
}

fn resolution_process_contracts_retrieval_config(command: &str) -> RetrievalConfig {
    RetrievalConfig {
        enabled: true,
        command: Some(command.to_string()),
        timeout_secs: 5,
        max_results: 3,
        max_bytes: 2000,
        min_attempt: 2,
    }
}

fn resolution_process_contracts_retrieval_request() -> RetrievalRequest {
    RetrievalRequest {
        bead_id: "needle-process-contract".to_string(),
        title: "Fix the widget".to_string(),
        workspace: "/fixture".to_string(),
        attempt: 2,
        failure_summary: "mismatched types".to_string(),
        terminal_reason: Some("gate:default_rust".to_string()),
        local_candidates: Vec::new(),
    }
}

#[tokio::test]
async fn resolution_process_contracts_retrieval_parses_stdin_and_caps_results() {
    let script = r#"read -r line
bid=$(printf '%s' "$line" | sed -n 's/.*"bead_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
i=0
while [ "$i" -lt 5 ]; do
  printf '{"id":"session:%d","source":"archive","title":"fix for %s","text":"closed"}\n' "$i" "$bid"
  i=$((i+1))
done"#;
    let result = retrieve(
        &resolution_process_contracts_retrieval_config(script),
        &resolution_process_contracts_retrieval_request(),
    )
    .await;

    assert_eq!(result.items.len(), 3, "{:?}", result.items);
    assert!(result.items[0].title.contains("needle-process-contract"));
    assert_eq!(result.ids(), vec!["session:0", "session:1", "session:2"]);
}

#[tokio::test]
async fn resolution_process_contracts_retrieval_failures_and_timeout_yield_no_hints() {
    let request = resolution_process_contracts_retrieval_request();
    for command in ["exit 3", "echo not-json"] {
        assert!(retrieve(
            &resolution_process_contracts_retrieval_config(command),
            &request
        )
        .await
        .items
        .is_empty());
    }

    let root = TempDir::new().expect("create retrieval fixture");
    let pid_path = root.path().join("retrieval.pid");
    let slow = RetrievalConfig {
        timeout_secs: 1,
        ..resolution_process_contracts_retrieval_config(&format!(
            "printf '%s' \"$$\" > {}; exec sleep 30",
            pid_path.display()
        ))
    };
    assert!(retrieve(&slow, &request).await.items.is_empty());
    let pid: u32 = fs::read_to_string(&pid_path)
        .expect("retrieval process recorded pid")
        .parse()
        .expect("pid is numeric");
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "timed-out retrieval process {pid} was not reaped"
    );

    let disabled = RetrievalConfig {
        enabled: false,
        ..resolution_process_contracts_retrieval_config("echo '{\"id\":\"x\"}'")
    };
    assert!(retrieve(&disabled, &request).await.items.is_empty());
    let unset = RetrievalConfig {
        command: None,
        ..resolution_process_contracts_retrieval_config("")
    };
    assert!(retrieve(&unset, &request).await.items.is_empty());
}
