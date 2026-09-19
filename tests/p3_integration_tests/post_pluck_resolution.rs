//! Isolated end-to-end coverage for the post-Pluck Resolve lifecycle.

#![cfg(feature = "integration")]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::config::{Config, MitosisConfig, PromptConfig, ResolveConfig};
use needle::mitosis::MitosisEvaluator;
use needle::prompt::PromptBuilder;
use needle::resolve::executor::{AppliedDecision, DecisionExecutor};
use needle::resolve::{ResolveContext, ResolveDecision, Resolver};
use needle::telemetry::{EventKind, Sink, Telemetry, TelemetryEvent};
use needle::types::{Bead, BeadId, BeadStatus, ClaimResult, ClaimStatus};
use tempfile::TempDir;

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct IsolatedRoots {
    _env_lock: MutexGuard<'static, ()>,
    home: TempDir,
    workspace: TempDir,
    bin: TempDir,
    old_home: Option<OsString>,
    old_path: Option<OsString>,
}

impl IsolatedRoots {
    fn new() -> Self {
        let _env_lock = env_lock();
        let home = tempfile::tempdir().expect("isolated HOME");
        let workspace = tempfile::tempdir().expect("isolated workspace");
        let bin = tempfile::tempdir().expect("isolated resolver bin");
        let old_home = std::env::var_os("HOME");
        let old_path = std::env::var_os("PATH");
        std::env::set_var("HOME", home.path());
        std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.path().display()));
        Self {
            _env_lock,
            home,
            workspace,
            bin,
            old_home,
            old_path,
        }
    }

    fn workspace(&self) -> PathBuf {
        self.workspace.path().to_path_buf()
    }

    fn install_resolver(&self, script_body: &str) {
        let path = self.bin.path().join("claude");
        let script = ["#!/bin/sh", script_body, ""].join("\n");
        std::fs::write(&path, script).expect("write fake resolver");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path)
                .expect("fake resolver metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).expect("make fake resolver executable");
        }
    }
}

impl Drop for IsolatedRoots {
    fn drop(&mut self) {
        match &self.old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match &self.old_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
    }
}

#[derive(Debug, Clone)]
struct StoreState {
    bead: Bead,
    children: Vec<Bead>,
    notes: Vec<String>,
    next_child: usize,
}

struct ScenarioStore {
    state: Mutex<StoreState>,
    claim_reads: AtomicUsize,
    ownership_race: bool,
}

impl ScenarioStore {
    fn new(workspace: PathBuf) -> Self {
        Self {
            state: Mutex::new(StoreState {
                bead: Bead {
                    id: BeadId::from("post-pluck-1"),
                    title: "Post-Pluck scenario".to_string(),
                    body: Some("exercise the Resolve lifecycle".to_string()),
                    priority: 1,
                    status: BeadStatus::InProgress,
                    assignee: Some("worker-a".to_string()),
                    labels: Vec::new(),
                    workspace,
                    dependencies: Vec::new(),
                    dependents: Vec::new(),
                    comments: Vec::new(),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                children: Vec::new(),
                notes: Vec::new(),
                next_child: 0,
            }),
            claim_reads: AtomicUsize::new(0),
            ownership_race: false,
        }
    }

    fn with_ownership_race(mut self) -> Self {
        self.ownership_race = true;
        self
    }

    fn bead(&self) -> Bead {
        self.state.lock().unwrap().bead.clone()
    }

    fn children(&self) -> Vec<Bead> {
        self.state.lock().unwrap().children.clone()
    }

    fn notes(&self) -> Vec<String> {
        self.state.lock().unwrap().notes.clone()
    }
}

#[async_trait]
impl BeadStore for ScenarioStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        let state = self.state.lock().unwrap();
        let mut beads = vec![state.bead.clone()];
        beads.extend(state.children.clone());
        Ok(beads)
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        let state = self.state.lock().unwrap();
        if *id == state.bead.id {
            Ok(state.bead.clone())
        } else {
            state
                .children
                .iter()
                .find(|child| child.id == *id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown bead {id}"))
        }
    }

    async fn claim_status(&self, id: &BeadId) -> Result<ClaimStatus> {
        let mut state = self.state.lock().unwrap();
        if self.ownership_race && self.claim_reads.fetch_add(1, Ordering::SeqCst) == 0 {
            state.bead.assignee = Some("worker-b".to_string());
        }
        let bead = if *id == state.bead.id {
            &state.bead
        } else {
            state
                .children
                .iter()
                .find(|child| child.id == *id)
                .ok_or_else(|| anyhow::anyhow!("unknown bead {id}"))?
        };
        Ok(ClaimStatus {
            status: bead.status.clone(),
            assignee: bead.assignee.clone(),
            revision: None,
            claim_epoch: None,
        })
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
        anyhow::bail!("claim is not part of this scenario")
    }

    async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
        Ok(ClaimResult::NotClaimable {
            reason: "scenario store".to_string(),
        })
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if *id == state.bead.id {
            state.bead.status = BeadStatus::Open;
            state.bead.assignee = None;
        }
        Ok(())
    }

    async fn block(&self, id: &BeadId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if *id == state.bead.id {
            state.bead.status = BeadStatus::Blocked;
        }
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn close(&self, id: &BeadId, _reason: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if *id == state.bead.id {
            state.bead.status = BeadStatus::Done;
            state.bead.assignee = None;
        }
        Ok(())
    }

    async fn append_notes(&self, id: &BeadId, note: &str) -> Result<()> {
        let bead_id = self.state.lock().unwrap().bead.id.clone();
        if *id == bead_id {
            self.state.lock().unwrap().notes.push(note.to_string());
        }
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        Ok(self.show(id).await?.labels)
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if *id == state.bead.id {
            state.bead.labels.push(label.to_string());
        } else if let Some(child) = state.children.iter_mut().find(|child| child.id == *id) {
            child.labels.push(label.to_string());
        }
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        let mut state = self.state.lock().unwrap();
        state.next_child += 1;
        let id = BeadId::from(format!("post-pluck-child-{}", state.next_child));
        let workspace = state.bead.workspace.clone();
        state.children.push(Bead {
            id: id.clone(),
            title: title.to_string(),
            body: Some(body.to_string()),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            workspace,
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        });
        Ok(id)
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

struct MemorySink {
    events: Arc<Mutex<Vec<TelemetryEvent>>>,
}

impl Sink for MemorySink {
    fn accept(&self, event: &TelemetryEvent) -> Result<()> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }

    fn flush(&self, _deadline: Duration) -> Result<()> {
        Ok(())
    }
}

fn telemetry() -> (Telemetry, Arc<Mutex<Vec<TelemetryEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    (
        Telemetry::with_sink(
            "post-pluck-test".to_string(),
            MemorySink {
                events: events.clone(),
            },
        ),
        events,
    )
}

fn executor(telemetry: Telemetry) -> DecisionExecutor {
    let mut config = Config::default();
    config.worker.enforce_shipped_work = false;
    DecisionExecutor::new(config, telemetry)
}

fn complete() -> ResolveDecision {
    ResolveDecision::Complete {
        evidence: "tests passed".to_string(),
        commit_message: "ship resolved work".to_string(),
    }
}

fn retry() -> ResolveDecision {
    ResolveDecision::Retry {
        evidence: "provider was transiently unavailable".to_string(),
        strategy: "retry after backoff".to_string(),
    }
}

fn blocked() -> ResolveDecision {
    ResolveDecision::Blocked {
        evidence: "upstream API is unavailable".to_string(),
        blocker_type: "external dependency".to_string(),
        description: "upstream must publish the required schema".to_string(),
    }
}

fn split(id: &BeadId) -> ResolveDecision {
    ResolveDecision::Split {
        evidence: "two independent deliverables".to_string(),
        parent_bead_id: id.to_string(),
        child_titles: vec![
            "first child task".to_string(),
            "second child task".to_string(),
        ],
    }
}

async fn resolve_from_fake_agent(
    roots: &IsolatedRoots,
    script: &str,
    timeout_secs: u64,
    exit_code: i32,
) -> Result<ResolveDecision> {
    roots.install_resolver(script);
    let scenario = ScenarioStore::new(roots.workspace());
    let bead = scenario.bead();
    let context = ResolveContext::new(
        &bead,
        exit_code,
        "pluck stdout".to_string(),
        "pluck stderr".to_string(),
        Duration::from_millis(12),
        Utc::now(),
        false,
    );
    Resolver::with_config(
        PromptBuilder::new(&PromptConfig::default()),
        ResolveConfig {
            enabled: true,
            timeout_secs,
            custom_template_path: None,
            use_default_template: true,
        },
    )
    .resolve_strict(&context)
    .await
}

#[tokio::test]
async fn post_pluck_complete_resolution_closes_the_claimed_bead() {
    let roots = IsolatedRoots::new();
    let store = ScenarioStore::new(roots.workspace());
    let bead = store.bead();
    let (telemetry, _) = telemetry();
    let applied = executor(telemetry)
        .apply(&store, &bead, &complete(), "worker-a", None)
        .await
        .expect("complete decision should apply");
    assert_eq!(applied, AppliedDecision::Completed);
    assert_eq!(store.bead().status, BeadStatus::Done);
    assert!(store.bead().assignee.is_none());
}

#[tokio::test]
async fn post_pluck_retry_resolution_releases_for_retry() {
    let roots = IsolatedRoots::new();
    let store = ScenarioStore::new(roots.workspace());
    let bead = store.bead();
    let (telemetry, _) = telemetry();
    let applied = executor(telemetry)
        .apply(&store, &bead, &retry(), "worker-a", None)
        .await
        .expect("retry decision should apply");
    assert!(matches!(applied, AppliedDecision::Released(_)));
    assert_eq!(store.bead().status, BeadStatus::Open);
    assert!(store.bead().assignee.is_none());
    assert!(!store.notes().is_empty());
}

#[tokio::test]
async fn post_pluck_blocked_resolution_blocks_the_bead() {
    let roots = IsolatedRoots::new();
    let store = ScenarioStore::new(roots.workspace());
    let bead = store.bead();
    let (telemetry, _) = telemetry();
    let applied = executor(telemetry)
        .apply(&store, &bead, &blocked(), "worker-a", None)
        .await
        .expect("blocked decision should apply");
    assert_eq!(applied, AppliedDecision::Blocked);
    assert_eq!(store.bead().status, BeadStatus::Blocked);
    assert!(!store.notes().is_empty());
}

#[tokio::test]
async fn post_pluck_split_resolution_creates_children_and_blocks_parent() {
    let roots = IsolatedRoots::new();
    let store = ScenarioStore::new(roots.workspace());
    let bead = store.bead();
    let (telemetry, _) = telemetry();
    let mitosis = MitosisEvaluator::new(
        MitosisConfig::default(),
        telemetry.clone(),
        roots.home.path().join("mitosis-locks"),
    );
    let applied = executor(telemetry)
        .with_mitosis(mitosis)
        .apply(&store, &bead, &split(&bead.id), "worker-a", None)
        .await
        .expect("split decision should apply");
    assert!(matches!(applied, AppliedDecision::Split { created: 2, .. }));
    assert_eq!(store.bead().status, BeadStatus::Blocked);
    assert_eq!(store.children().len(), 2);
}

#[tokio::test]
async fn post_pluck_invalid_resolver_response_is_a_strict_failure() {
    let roots = IsolatedRoots::new();
    let error = resolve_from_fake_agent(&roots, "printf '%s\\n' 'not-json'", 1, 1)
        .await
        .expect_err("invalid response must not become a lifecycle decision");
    assert!(error.to_string().contains("parse") || error.to_string().contains("response"));
}

#[tokio::test]
async fn post_pluck_resolver_timeout_is_a_strict_failure() {
    let roots = IsolatedRoots::new();
    let error = resolve_from_fake_agent(&roots, "while :; do :; done", 1, 1)
        .await
        .expect_err("resolver timeout must be surfaced");
    let message = error.to_string();
    assert!(
        message.contains("timed out"),
        "resolver timeout should be surfaced, got: {message}"
    );
}

#[tokio::test]
async fn post_pluck_ownership_race_does_not_mutate_the_new_owner() {
    let roots = IsolatedRoots::new();
    let store = ScenarioStore::new(roots.workspace()).with_ownership_race();
    let bead = store.bead();
    let (telemetry, _) = telemetry();
    let applied = executor(telemetry)
        .apply(&store, &bead, &retry(), "worker-a", None)
        .await
        .expect("ownership loss is a successful no-op");
    assert_eq!(applied, AppliedDecision::OwnershipLost);
    assert_eq!(store.bead().status, BeadStatus::InProgress);
    assert_eq!(store.bead().assignee.as_deref(), Some("worker-b"));
    assert!(store.notes().is_empty());
}

#[tokio::test]
async fn tradegraph_exit_zero_still_in_progress_runs_resolution() {
    let roots = IsolatedRoots::new();
    let decision = resolve_from_fake_agent(
        &roots,
        "printf '%s\\n' '{\"decision\":{\"complete\":{\"evidence\":\"tests passed\",\"commit_message\":\"ship it\"}}}'",
        1,
        0,
    )
    .await
    .expect("exit zero must still invoke Resolve");
    assert!(matches!(decision, ResolveDecision::Complete { .. }));

    let store = ScenarioStore::new(roots.workspace());
    let bead = store.bead();
    assert_eq!(bead.status, BeadStatus::InProgress);
    let (telemetry, _) = telemetry();
    let applied = executor(telemetry)
        .apply(&store, &bead, &decision, "worker-a", None)
        .await
        .expect("valid Resolve result should close the still-in-progress bead");
    assert_eq!(applied, AppliedDecision::Completed);
    assert_eq!(store.bead().status, BeadStatus::Done);
}

#[tokio::test]
async fn post_pluck_resolution_events_are_operator_queryable() {
    let _roots = IsolatedRoots::new();
    let (telemetry, events) = telemetry();
    telemetry
        .emit(
            EventKind::ResolutionApplied {
                bead_id: BeadId::from("post-pluck-1"),
                decision: "complete".to_string(),
                action: "completed".to_string(),
                duration_ms: 42,
            },
            Utc::now(),
        )
        .expect("applied event should enqueue");
    telemetry
        .emit(
            EventKind::ResolutionFailed {
                bead_id: BeadId::from("post-pluck-2"),
                reason: "resolver_timeout".to_string(),
                duration_ms: 1000,
            },
            Utc::now(),
        )
        .expect("failed event should enqueue");
    telemetry
        .force_flush_async(Duration::from_secs(1))
        .await
        .expect("telemetry should flush");
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, "bead.resolution.applied");
    assert_eq!(events[0].data["decision"], "complete");
    assert_eq!(events[0].data["action"], "completed");
    assert_eq!(events[1].event_type, "bead.resolution.failed");
    assert_eq!(events[1].data["reason"], "resolver_timeout");
}
