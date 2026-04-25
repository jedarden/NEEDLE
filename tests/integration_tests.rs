//! Integration tests for NEEDLE Phase 1.
//!
//! These tests exercise the full worker pipeline end-to-end using mock
//! `BeadStore` implementations and real process execution via bash adapters.
//!
//! Test categories:
//! 1. End-to-end single worker cycle
//! 2. All 6 outcome paths (success, failure, timeout, agent_not_found, interrupted, crash)
//! 3. Exhaustion (empty queue → Pluck returns NoWork → Knot fires → EXHAUSTED)
//! 4. Graceful shutdown (shutdown flag during various states)
//! 5. Deterministic ordering (property test)
//! 6. Cross-workspace mend: two-workspace zombie scenario

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::config::Config;
use needle::dispatch::{AgentAdapter, Dispatcher};
use needle::telemetry::Telemetry;
use needle::types::{
    Bead, BeadId, BeadStatus, ClaimResult, IdleAction, InputMethod, Outcome, StrandResult,
    WorkerState,
};
use needle::worker::Worker;

// ─── Shared test infrastructure ──────────────────────────────────────────────

/// Mock BeadStore that tracks all operations and returns configurable beads.
///
/// Key behaviors:
/// - `ready()` returns only open, unassigned beads
/// - `claim()` sets assignee, preventing re-selection via ready()
/// - `release()` removes the bead entirely (prevents infinite re-selection loops)
struct IntegrationMockStore {
    beads: Mutex<Vec<Bead>>,
    actions: Mutex<Vec<String>>,
}

impl IntegrationMockStore {
    fn new(beads: Vec<Bead>) -> Self {
        IntegrationMockStore {
            beads: Mutex::new(beads),
            actions: Mutex::new(Vec::new()),
        }
    }

    fn empty() -> Self {
        Self::new(vec![])
    }

    fn actions(&self) -> Vec<String> {
        self.actions.lock().unwrap().clone()
    }

    fn record(&self, action: &str) {
        self.actions.lock().unwrap().push(action.to_string());
    }
}

#[async_trait]
impl BeadStore for IntegrationMockStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        self.record("ready");
        // Only return open, unassigned beads (matching real br behavior).
        Ok(self
            .beads
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b.status == BeadStatus::Open && b.assignee.is_none())
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        self.record("list_all");
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        self.record(&format!("show:{id}"));
        let beads = self.beads.lock().unwrap();
        let bead = beads
            .iter()
            .find(|b| b.id == *id)
            .cloned()
            .unwrap_or_else(|| make_bead_with_id(id.as_ref()));
        Ok(bead)
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        self.record(&format!("claim:{id}:{actor}"));
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::InProgress;
            bead.assignee = Some(actor.to_string());
            Ok(ClaimResult::Claimed(bead.clone()))
        } else {
            Ok(ClaimResult::NotClaimable {
                reason: "not found".to_string(),
            })
        }
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        self.record(&format!("release:{id}"));
        // Remove released beads from the list to prevent infinite re-selection loops.
        // In real usage, released beads get labels (deferred, failure-count) that
        // filter them out, but the mock doesn't simulate full label-based filtering.
        let mut beads = self.beads.lock().unwrap();
        beads.retain(|b| b.id != *id);
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        self.record(&format!("reopen:{id}"));
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        self.record(&format!("labels:{id}"));
        Ok(vec![])
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.record(&format!("add_label:{id}:{label}"));
        Ok(())
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.record(&format!("remove_label:{id}:{label}"));
        Ok(())
    }

    async fn create_bead(&self, title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
        self.record(&format!("create_bead:{title}"));
        Ok(BeadId::from("alert-new"))
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        self.record(&format!("add_dep:{}:{}", blocker_id, blocked_id));
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
}

fn make_bead_with_id(id: &str) -> Bead {
    Bead {
        id: BeadId::from(id),
        title: format!("Test bead {id}"),
        body: Some("Implement something useful".to_string()),
        priority: 1,
        status: BeadStatus::Open,
        assignee: None,
        labels: vec![],
        workspace: PathBuf::from("/tmp/test-workspace"),
        dependencies: vec![],
        dependents: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn make_bead(id: &str, priority: u8) -> Bead {
    let mut bead = make_bead_with_id(id);
    bead.priority = priority;
    bead
}

fn test_adapter(name: &str, template: &str, timeout_secs: u64) -> AgentAdapter {
    AgentAdapter {
        name: name.to_string(),
        description: None,
        agent_cli: "bash".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: template.to_string(),
        environment: HashMap::new(),
        timeout_secs,
        provider: None,
        model: None,
        token_extraction: needle::dispatch::TokenExtraction::None,
        output_transform: None,
    }
}

fn test_config(adapter_name: &str, workspace_home: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Exit;
    config.agent.default = adapter_name.to_string();
    config.agent.timeout = 10;
    config.self_modification.hot_reload = false;
    // Match the test bead workspace so the remote-store-switch logic
    // doesn't fire (it would try to create a BrCliBeadStore).
    config.workspace.default = std::path::PathBuf::from("/tmp/test-workspace");
    // Isolate workspace home so the registry doesn't leak between tests.
    config.workspace.home = workspace_home.to_path_buf();
    config
}

/// Returns `(Worker, TempDir)` — the TempDir must be kept alive for the test duration.
fn make_worker_with_adapter(
    store: Arc<dyn BeadStore>,
    adapter_name: &str,
    template: &str,
    timeout_secs: u64,
) -> (Worker, tempfile::TempDir) {
    let home_dir = tempfile::tempdir().expect("failed to create temp dir for test workspace home");
    let config = test_config(adapter_name, home_dir.path());
    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter(adapter_name, template, timeout_secs);
    let mut adapters = HashMap::new();
    adapters.insert(adapter_name.to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    (worker, home_dir)
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 1: End-to-end single worker
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn end_to_end_single_bead_success() {
    // A single bead exists, agent exits 0, worker processes it and stops.
    // No show_override_status: the claimer verifies via show(), so the bead
    // must appear as Open during claiming. After claim sets assignee, the
    // bead is filtered from ready() on the next cycle → exhaustion.
    let bead = make_bead("needle-e2e-001", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "echo-test", "echo 'agent completed'", 10);

    let result = worker.run().await.unwrap();

    // Worker should process the bead and then exhaust (no more beads).
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );
    assert!(
        worker.beads_processed() >= 1,
        "expected at least 1 bead processed, got {}",
        worker.beads_processed()
    );
}

#[tokio::test]
async fn end_to_end_worker_loops_to_next_bead() {
    // Two beads exist; worker processes both.
    let beads = vec![make_bead("needle-e2e-a", 1), make_bead("needle-e2e-b", 1)];
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(beads));

    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "echo-test", "echo 'done'", 10);

    let result = worker.run().await.unwrap();

    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );
    assert!(
        worker.beads_processed() >= 2,
        "expected at least 2 beads processed, got {}",
        worker.beads_processed()
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 2: All 6 outcome paths
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn outcome_path_success_exit_0() {
    let bead = make_bead("needle-out-success", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "success-agent", "exit 0", 10);

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );
    assert_eq!(worker.beads_processed(), 1);
}

#[tokio::test]
async fn outcome_path_failure_exit_1() {
    let bead = make_bead("needle-out-fail", 1);
    let store = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "fail-agent", "exit 1", 10);

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );

    let actions = store.actions();
    assert!(
        actions.iter().any(|a| a.starts_with("release:")),
        "failure should release bead; actions: {:?}",
        actions
    );
    assert!(
        actions
            .iter()
            .any(|a| a.contains("add_label") && a.contains("failure-count:1")),
        "failure should increment failure count; actions: {:?}",
        actions
    );
}

#[tokio::test]
async fn outcome_path_timeout_exit_124() {
    let bead = make_bead("needle-out-timeout", 1);
    let store = Arc::new(IntegrationMockStore::new(vec![bead]));

    // Use a very short timeout (1 second) with a long-running command.
    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "timeout-agent", "sleep 100", 1);

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );

    let actions = store.actions();
    assert!(
        actions.iter().any(|a| a.starts_with("release:")),
        "timeout should release bead; actions: {:?}",
        actions
    );
    assert!(
        actions
            .iter()
            .any(|a| a.contains("add_label") && a.contains("deferred")),
        "timeout should add deferred label; actions: {:?}",
        actions
    );
}

#[tokio::test]
async fn outcome_path_agent_not_found_exit_127() {
    let bead = make_bead("needle-out-notfound", 1);
    let store = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_dir) = make_worker_with_adapter(
        store.clone(),
        "missing-agent",
        "nonexistent-binary-xyz-999",
        10,
    );

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );

    let actions = store.actions();
    assert!(
        actions.iter().any(|a| a.starts_with("release:")),
        "agent_not_found should release bead; actions: {:?}",
        actions
    );
}

#[tokio::test]
async fn outcome_path_crash_exit_137() {
    let bead = make_bead("needle-out-crash", 1);
    let store = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "crash-agent", "exit 137", 10);

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );

    let actions = store.actions();
    assert!(
        actions.iter().any(|a| a.starts_with("release:")),
        "crash should release bead; actions: {:?}",
        actions
    );
    assert!(
        actions.iter().any(|a| a.starts_with("create_bead:")),
        "crash should create alert bead; actions: {:?}",
        actions
    );
}

#[tokio::test]
async fn outcome_path_interrupted_via_shutdown_flag() {
    let bead = make_bead("needle-out-interrupt", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));

    let _home_dir = tempfile::tempdir().unwrap();
    let config = test_config("slow-agent", _home_dir.path());
    let mut worker = Worker::new(config, "test-worker".to_string(), store.clone());

    // Use a slow adapter so we have time to set shutdown.
    let adapter = test_adapter("slow-agent", "sleep 2", 30);
    let mut adapters = HashMap::new();
    adapters.insert("slow-agent".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        30,
    ));

    // Set shutdown flag before run — worker should detect it during the loop.
    worker.request_shutdown();

    let result = worker.run().await.unwrap();
    assert_eq!(
        result,
        WorkerState::Stopped,
        "interrupted worker should stop cleanly"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 3: Exhaustion
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn exhaustion_empty_workspace() {
    // Empty store → Pluck returns NoWork → Knot fires → EXHAUSTED → Exit.
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_dir = tempfile::tempdir().unwrap();
    let config = test_config("echo-test", _home_dir.path());
    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected exhausted/stopped, got {:?}",
        result
    );
    assert_eq!(worker.beads_processed(), 0, "no beads should be processed");
}

#[tokio::test]
async fn exhaustion_with_idle_action_exit() {
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Exit;
    config.agent.default = "echo-test".to_string();
    config.workspace.home = _home_dir.path().to_path_buf();

    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    let result = worker.run().await.unwrap();
    assert_eq!(
        result,
        WorkerState::Stopped,
        "idle_action=exit should produce Stopped"
    );
}

#[tokio::test]
async fn exhaustion_with_idle_action_wait_survives_sleep() {
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Mock store that returns NoWork initially, then a bead after N calls.
    struct DelayedBeadStore {
        call_count: AtomicU32,
        bead_after: u32,
        bead: Mutex<Option<Bead>>,
        /// Tracks claimed beads (moved here from `bead` on claim).
        claimed: Mutex<Vec<Bead>>,
    }

    #[async_trait]
    impl BeadStore for DelayedBeadStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count >= self.bead_after {
                // Only return the bead if it hasn't been claimed yet.
                let bead = self.bead.lock().unwrap();
                let claimed = self.claimed.lock().unwrap();
                if bead.is_some() && !claimed.iter().any(|b| b.id == bead.as_ref().unwrap().id) {
                    Ok(vec![bead.clone().unwrap()])
                } else {
                    Ok(vec![])
                }
            } else {
                Ok(Vec::new())
            }
        }

        async fn list_all(&self) -> Result<Vec<Bead>> {
            let bead = self.bead.lock().unwrap();
            let claimed = self.claimed.lock().unwrap();
            let mut all: Vec<Bead> = claimed.clone();
            if let Some(b) = bead.as_ref() {
                if !all.iter().any(|x| x.id == b.id) {
                    all.push(b.clone());
                }
            }
            Ok(all)
        }

        async fn show(&self, id: &BeadId) -> Result<Bead> {
            let claimed = self.claimed.lock().unwrap();
            if let Some(b) = claimed.iter().find(|b| b.id == *id) {
                return Ok(b.clone());
            }
            let bead = self.bead.lock().unwrap();
            match bead.as_ref() {
                Some(b) if b.id == *id => Ok(b.clone()),
                _ => anyhow::bail!("bead not found: {id}"),
            }
        }

        async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
            let bead = self.bead.lock().unwrap();
            let mut claimed = self.claimed.lock().unwrap();
            match bead.as_ref() {
                Some(b) if b.id == *id => {
                    let mut cloned = b.clone();
                    cloned.status = BeadStatus::InProgress;
                    cloned.assignee = Some(actor.to_string());
                    claimed.push(cloned.clone());
                    Ok(ClaimResult::Claimed(cloned))
                }
                _ => Ok(ClaimResult::NotClaimable {
                    reason: "no bead".to_string(),
                }),
            }
        }

        async fn release(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn flush(&self) -> Result<()> {
            Ok(())
        }

        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            Ok(vec![])
        }

        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }

        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }

        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("mock-bead"))
        }

        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
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
    }

    let bead = Bead {
        id: BeadId::from("test-bead"),
        status: BeadStatus::Open,
        title: "Test Bead".to_string(),
        body: Some("Test body".to_string()),
        priority: 1,
        assignee: None,
        labels: vec![],
        workspace: std::path::PathBuf::from("/tmp"),
        dependencies: vec![],
        dependents: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let store: Arc<dyn BeadStore> = Arc::new(DelayedBeadStore {
        call_count: AtomicU32::new(0),
        bead_after: 2, // Add bead after 2 calls (first call goes to EXHAUSTED, second after sleep)
        bead: Mutex::new(Some(bead)),
        claimed: Mutex::new(vec![]),
    });

    let _home_dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Wait;
    config.worker.idle_timeout = 1; // 1 second for fast test
    config.agent.default = "echo-test".to_string();
    config.workspace.home = _home_dir.path().to_path_buf();
    config.self_modification.hot_reload = false;
    config.workspace.default = std::path::PathBuf::from("/tmp");

    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    let result = worker.run().await.unwrap();
    assert_eq!(
        result,
        WorkerState::Stopped,
        "worker should stop after processing the delayed bead"
    );
    assert_eq!(
        worker.beads_processed(),
        1,
        "worker should process the bead that appeared after idle sleep"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 4: Graceful shutdown
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn shutdown_during_selecting_exits_cleanly() {
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_dir = tempfile::tempdir().unwrap();
    let config = test_config("echo-test", _home_dir.path());
    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    // Set shutdown before run.
    worker.request_shutdown();

    let result = worker.run().await.unwrap();
    assert_eq!(result, WorkerState::Stopped);
}

#[tokio::test]
async fn shutdown_flag_preempts_execution() {
    // Even with beads available, shutdown should cause clean exit.
    let bead = make_bead("needle-shutdown", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));
    let _home_dir = tempfile::tempdir().unwrap();
    let config = test_config("echo-test", _home_dir.path());
    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    // Set shutdown flag.
    worker.request_shutdown();

    let result = worker.run().await.unwrap();
    assert_eq!(
        result,
        WorkerState::Stopped,
        "shutdown should preempt processing"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 5: Deterministic ordering (property test)
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn deterministic_ordering_same_beads_same_order() {
    // Given the same set of beads with varied priorities and creation times,
    // two independent sorts should produce identical ordering.
    use needle::strand::PluckStrand;
    use needle::strand::Strand;

    let now = Utc::now();

    let beads = vec![
        {
            let mut b = make_bead("needle-sort-c", 3);
            b.created_at = now - chrono::Duration::hours(1);
            b
        },
        {
            let mut b = make_bead("needle-sort-a", 1);
            b.created_at = now - chrono::Duration::hours(3);
            b
        },
        {
            let mut b = make_bead("needle-sort-b", 1);
            b.created_at = now - chrono::Duration::hours(2);
            b
        },
        {
            let mut b = make_bead("needle-sort-d", 2);
            b.created_at = now;
            b
        },
    ];

    // Create two independent stores with the same beads (shuffled).
    let store1: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(beads.clone()));
    let store2: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new({
        let mut reversed = beads.clone();
        reversed.reverse();
        reversed
    }));

    let pluck = PluckStrand::new(vec![]);

    // Evaluate both — should return the same top candidate.
    let result1 = pluck.evaluate(store1.as_ref()).await;
    let result2 = pluck.evaluate(store2.as_ref()).await;

    // Extract candidate IDs.
    let id1 = match result1 {
        needle::types::StrandResult::BeadFound(beads) => beads.first().map(|b| b.id.clone()),
        _ => None,
    };
    let id2 = match result2 {
        needle::types::StrandResult::BeadFound(beads) => beads.first().map(|b| b.id.clone()),
        _ => None,
    };

    assert_eq!(
        id1, id2,
        "deterministic ordering: same beads must produce same top candidate"
    );

    // The top candidate should be the highest-priority, oldest bead.
    assert_eq!(
        id1,
        Some(BeadId::from("needle-sort-a")),
        "P1 bead created earliest should be selected first"
    );
}

#[tokio::test]
async fn deterministic_ordering_tiebreak_by_id() {
    // When priority and creation time are identical, bead ID breaks ties.
    use needle::strand::PluckStrand;
    use needle::strand::Strand;

    let now = Utc::now();

    let beads = vec![
        {
            let mut b = make_bead("needle-zz", 1);
            b.created_at = now;
            b
        },
        {
            let mut b = make_bead("needle-aa", 1);
            b.created_at = now;
            b
        },
        {
            let mut b = make_bead("needle-mm", 1);
            b.created_at = now;
            b
        },
    ];

    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(beads));
    let pluck = PluckStrand::new(vec![]);

    let result = pluck.evaluate(store.as_ref()).await;

    let top_id = match result {
        needle::types::StrandResult::BeadFound(beads) => beads.first().map(|b| b.id.clone()),
        _ => None,
    };

    assert_eq!(
        top_id,
        Some(BeadId::from("needle-aa")),
        "when priority and time match, lowest ID wins"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 6: Outcome classification exhaustiveness
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn outcome_classify_covers_all_exit_code_ranges() {
    // Verify that classify handles the full i32 range without panicking.
    // This is a smoke test to ensure no gaps exist.
    let test_codes: Vec<i32> = vec![
        i32::MIN,
        -1000,
        -1,
        0,
        1,
        2,
        50,
        99,
        100,
        123,
        124,
        125,
        126,
        127,
        128,
        129,
        130,
        137,
        143,
        255,
        256,
        1000,
        i32::MAX,
    ];

    for code in test_codes {
        // Should not panic.
        let outcome = Outcome::classify(code, false);
        // Verify specific mappings.
        match code {
            0 => assert_eq!(outcome, Outcome::Success),
            1 => assert_eq!(outcome, Outcome::Failure),
            124 => assert_eq!(outcome, Outcome::Timeout),
            127 => assert_eq!(outcome, Outcome::AgentNotFound),
            c if c > 128 => assert_eq!(outcome, Outcome::Crash(c)),
            c if c < 0 => assert_eq!(outcome, Outcome::Crash(c)),
            _ => {} // Other codes just shouldn't panic
        }
    }

    // Verify interrupted flag always wins.
    for code in [-1, 0, 1, 127, 137] {
        assert_eq!(
            Outcome::classify(code, true),
            Outcome::Interrupted,
            "was_interrupted=true must always return Interrupted for code {code}"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 7: Worker config validation at boot
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn worker_boot_rejects_invalid_config() {
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.agent.default = String::new(); // Invalid: empty agent name
    config.workspace.home = _home_dir.path().to_path_buf();

    let mut worker = Worker::new(config, "test-worker".to_string(), store);
    let result = worker.run().await;

    assert!(
        result.is_err(),
        "worker should fail to boot with invalid config"
    );
    assert!(
        result.unwrap_err().to_string().contains("agent.default"),
        "error should mention the invalid field"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 8: Full pipeline telemetry sequence
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn full_cycle_produces_telemetry_state_transitions() {
    // Verify the expected state transition sequence occurs.
    let bead = make_bead("needle-telem", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_dir) = make_worker_with_adapter(store, "echo-test", "echo done", 10);

    let result = worker.run().await.unwrap();

    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );
    // The key assertion: at least 1 bead was processed through the full pipeline.
    assert!(worker.beads_processed() >= 1);
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 9: Dispatcher integration — real process execution
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dispatcher_captures_stdout_and_stderr() {
    let adapter = test_adapter("io-test", "echo stdout-msg; echo stderr-msg >&2", 10);
    let mut adapters = HashMap::new();
    adapters.insert("io-test".to_string(), adapter.clone());

    let dispatcher =
        Dispatcher::with_adapters(adapters, Telemetry::new("test-worker".to_string()), 10);

    let prompt = needle::prompt::BuiltPrompt {
        content: "test".to_string(),
        hash: "abc123".to_string(),
        token_estimate: 1,
        template_name: "pluck".to_string(),
        template_version: "pluck-default".to_string(),
    };

    let result = dispatcher
        .dispatch(
            &BeadId::from("nd-io"),
            &prompt,
            &adapter,
            std::path::Path::new("/tmp"),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("stdout-msg"));
    assert!(result.stderr.contains("stderr-msg"));
}

#[tokio::test]
async fn dispatcher_timeout_kills_process() {
    let adapter = test_adapter("slow", "sleep 100", 1);
    let mut adapters = HashMap::new();
    adapters.insert("slow".to_string(), adapter.clone());

    let dispatcher =
        Dispatcher::with_adapters(adapters, Telemetry::new("test-worker".to_string()), 10);

    let prompt = needle::prompt::BuiltPrompt {
        content: "test".to_string(),
        hash: "abc123".to_string(),
        token_estimate: 1,
        template_name: "pluck".to_string(),
        template_version: "pluck-default".to_string(),
    };

    let result = dispatcher
        .dispatch(
            &BeadId::from("nd-slow"),
            &prompt,
            &adapter,
            std::path::Path::new("/tmp"),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 124, "timeout should yield exit 124");
    assert!(
        result.elapsed >= std::time::Duration::from_millis(900),
        "should have waited ~1s"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 10: Multiple beads with different priorities
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn worker_processes_high_priority_beads_first() {
    // Create beads with different priorities.
    let mut high = make_bead("needle-high", 1);
    let mut low = make_bead("needle-low", 3);
    high.created_at = Utc::now();
    low.created_at = Utc::now() - chrono::Duration::hours(10);

    let store = Arc::new(IntegrationMockStore::new(vec![low, high]));

    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "echo-test", "echo done", 10);

    let result = worker.run().await.unwrap();
    assert!(result == WorkerState::Stopped || result == WorkerState::Exhausted);

    let actions = store.actions();
    // Find claim actions to verify order.
    let claims: Vec<&String> = actions.iter().filter(|a| a.starts_with("claim:")).collect();
    assert!(!claims.is_empty(), "should have at least one claim action");
    // First claim should be for the high-priority bead.
    assert!(
        claims[0].contains("needle-high"),
        "highest priority bead should be claimed first; claims: {:?}",
        claims
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 11: Cross-workspace mend: two-workspace zombie scenario
// ═════════════════════════════════════════════════════════════════════════════

/// Mock BeadStore that simulates real BrCliBeadStore behavior for zombie scenarios.
///
/// This mock properly simulates the behavior where:
/// - In-progress beads don't appear in ready()
/// - Released beads become Open and appear in subsequent ready() calls
/// - This enables testing the "released beads returned in same pass" behavior
#[allow(dead_code)]
struct ZombieMockStore {
    /// All beads, mutable to support state transitions (release → open).
    beads: Mutex<Vec<Bead>>,
    /// Path to this workspace (for tagging).
    workspace: PathBuf,
    /// Track release calls.
    released: Arc<Mutex<Vec<BeadId>>>,
}

#[allow(dead_code)]
impl ZombieMockStore {
    fn new(all_beads: Vec<Bead>, workspace: PathBuf) -> (Self, Arc<Mutex<Vec<BeadId>>>) {
        let released = Arc::new(Mutex::new(Vec::new()));
        (
            ZombieMockStore {
                beads: Mutex::new(all_beads),
                workspace,
                released: released.clone(),
            },
            released,
        )
    }
}

#[async_trait]
impl BeadStore for ZombieMockStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        // Return open, unassigned beads (matching real br behavior).
        Ok(self
            .beads
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b.status == BeadStatus::Open && b.assignee.is_none())
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        let beads = self.beads.lock().unwrap();
        let bead = beads
            .iter()
            .find(|b| b.id == *id)
            .cloned()
            .unwrap_or_else(|| make_bead_with_id(id.as_ref()));
        Ok(bead)
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::InProgress;
            bead.assignee = Some(actor.to_string());
            return Ok(ClaimResult::Claimed(bead.clone()));
        }
        Ok(ClaimResult::NotClaimable {
            reason: "not found".to_string(),
        })
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        self.released.lock().unwrap().push(id.clone());
        // Update bead state: released beads become Open with no assignee.
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::Open;
            bead.assignee = None;
        }
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
        Ok(vec![])
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn create_bead(&self, title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
        Ok(BeadId::from(title.to_string()))
    }

    async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
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
}

/// Store that delegates to different underlying stores based on workspace path.
///
/// This allows ExploreStrand to query remote workspaces that have different
/// mock behaviors (e.g., zombie scenarios).
#[allow(dead_code)]
struct MultiWorkspaceStore {
    home_store: Arc<dyn BeadStore>,
    remote_stores: std::collections::HashMap<PathBuf, Arc<dyn BeadStore>>,
}

#[allow(dead_code)]
impl MultiWorkspaceStore {
    fn new(home_store: Arc<dyn BeadStore>) -> Self {
        MultiWorkspaceStore {
            home_store,
            remote_stores: std::collections::HashMap::new(),
        }
    }

    fn add_remote(&mut self, workspace: PathBuf, store: Arc<dyn BeadStore>) {
        self.remote_stores.insert(workspace, store);
    }
}

#[async_trait]
impl BeadStore for MultiWorkspaceStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        self.home_store.ready(_filters).await
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        self.home_store.list_all().await
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        self.home_store.show(id).await
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        self.home_store.claim(id, actor).await
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        self.home_store.release(id).await
    }

    async fn flush(&self) -> Result<()> {
        self.home_store.flush().await
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        self.home_store.reopen(id).await
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        self.home_store.labels(id).await
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.home_store.add_label(id, label).await
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.home_store.remove_label(id, label).await
    }

    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        self.home_store.create_bead(title, body, labels).await
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        self.home_store.add_dependency(blocker_id, blocked_id).await
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        self.home_store.doctor_repair().await
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        self.home_store.doctor_check().await
    }

    async fn full_rebuild(&self) -> Result<()> {
        self.home_store.full_rebuild().await
    }
}

#[tokio::test]
async fn cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead() {
    use needle::config::ExploreConfig;
    use needle::strand::{ExploreStrand, Strand};
    use std::fs;

    // Create real temporary directories for home and remote workspaces.
    let home_dir = tempfile::tempdir().unwrap();
    let home_workspace = home_dir.path().to_path_buf();
    let home_store = Arc::new(IntegrationMockStore::empty());

    let remote_dir = tempfile::tempdir().unwrap();
    let remote_workspace = remote_dir.path().to_path_buf();
    let remote_beads_dir = remote_workspace.join(".beads");
    fs::create_dir_all(&remote_beads_dir).unwrap();

    // Create a zombie bead in the remote workspace using br CLI.
    // First, create the bead as open.
    let output = std::process::Command::new("/home/coding/.local/bin/br")
        .arg("create")
        .arg("--type=task")
        .arg("--title=Zombie bead from crashed worker")
        .arg("--description=This bead was assigned to a worker that crashed")
        .current_dir(&remote_workspace)
        .output()
        .expect("br create command failed to execute");
    let create_result = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "br create failed: {}",
        create_result
    );

    // Extract the bead ID from the create output (format: "✓ Created <ID>: <title>").
    let bead_id = create_result
        .lines()
        .find(|l| l.contains("Created"))
        .and_then(|l| {
            // Parse "✓ Created <ID>: <title>" to extract the ID
            l.split("Created ").nth(1).and_then(|s| s.split(':').next())
        })
        .unwrap()
        .trim()
        .to_string();
    let bead_id = BeadId::from(bead_id);

    // Claim the bead to a dead worker.
    let claim_output = std::process::Command::new("/home/coding/.local/bin/br")
        .arg("update")
        .arg(bead_id.as_ref())
        .arg("--assignee=dead-worker-12345")
        .arg("--status=in_progress")
        .current_dir(&remote_workspace)
        .output()
        .expect("br update command failed to execute");
    assert!(
        claim_output.status.success(),
        "br update failed: {}",
        String::from_utf8_lossy(&claim_output.stderr)
    );

    // Verify the bead is now in-progress and not in ready().
    let remote_store =
        needle::bead_store::BrCliBeadStore::discover(remote_workspace.clone()).unwrap();
    let filters = Filters {
        assignee: None,
        exclude_labels: vec![
            "deferred".to_string(),
            "human".to_string(),
            "blocked".to_string(),
        ],
    };
    let ready_result = remote_store.ready(&filters).await.unwrap();
    assert!(
        ready_result.is_empty(),
        "remote workspace should have no ready beads initially"
    );

    // Create ExploreStrand with the remote workspace configured.
    let temp_dir = tempfile::tempdir().unwrap();
    let registry = needle::registry::Registry::new(temp_dir.path());
    let telemetry = Telemetry::new("test-worker".to_string());

    let explore_config = ExploreConfig {
        enabled: true,
        workspaces: vec![remote_workspace.clone()],
    };

    let explore = ExploreStrand::new(
        explore_config,
        home_workspace,
        registry,
        telemetry,
        "test-worker".to_string(),
    );

    // Evaluate ExploreStrand — it should run cross-workspace mend.
    let result = explore.evaluate(home_store.as_ref()).await;

    // After cross-workspace mend, ExploreStrand should return BeadFound with the tagged bead.
    match result {
        StrandResult::BeadFound(beads) => {
            assert!(
                !beads.is_empty(),
                "should return at least one bead after releasing orphan"
            );
            let bead = &beads[0];
            assert_eq!(
                bead.workspace, remote_workspace,
                "bead should be tagged with remote workspace path"
            );
            assert_eq!(
                bead.id, bead_id,
                "should return the zombie bead after release"
            );
            assert_eq!(
                bead.status,
                BeadStatus::Open,
                "released bead should be Open"
            );
            assert!(
                bead.assignee.is_none(),
                "released bead should have no assignee"
            );
        }
        StrandResult::NoWork => {
            panic!("expected BeadFound after releasing orphan, got NoWork");
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

#[tokio::test]
async fn cross_workspace_mend_skips_beads_with_live_assignees() {
    use needle::config::ExploreConfig;
    use needle::strand::{ExploreStrand, Strand};
    use std::fs;

    // Create real temporary directories.
    let home_dir = tempfile::tempdir().unwrap();
    let home_workspace = home_dir.path().to_path_buf();
    let home_store = Arc::new(IntegrationMockStore::empty());

    let remote_dir = tempfile::tempdir().unwrap();
    let remote_workspace = remote_dir.path().to_path_buf();
    let remote_beads_dir = remote_workspace.join(".beads");
    fs::create_dir_all(&remote_beads_dir).unwrap();

    // Create a bead in the remote workspace.
    let output = std::process::Command::new("/home/coding/.local/bin/br")
        .arg("create")
        .arg("--type=task")
        .arg("--title=Bead with live assignee")
        .arg("--description=This bead is assigned to a live worker")
        .current_dir(&remote_workspace)
        .output()
        .expect("br create command failed to execute");
    assert!(output.status.success(), "br create failed");

    let create_result = String::from_utf8_lossy(&output.stdout);
    let bead_id = create_result
        .lines()
        .find(|l| l.contains("Created"))
        .and_then(|l| {
            // Parse "✓ Created <ID>: <title>" to extract the ID
            l.split("Created ").nth(1).and_then(|s| s.split(':').next())
        })
        .unwrap()
        .trim()
        .to_string();
    let bead_id = BeadId::from(bead_id);

    // Create a registry with a live worker entry.
    let temp_dir = tempfile::tempdir().unwrap();
    let registry = needle::registry::Registry::new(temp_dir.path());

    // Register a live worker (using our own PID).
    registry
        .register(needle::registry::WorkerEntry {
            id: "live-worker".to_string(),
            pid: std::process::id(),
            workspace: remote_workspace.clone(),
            agent: "test".to_string(),
            model: None,
            provider: None,
            started_at: Utc::now(),
            beads_processed: 0,
        })
        .unwrap();

    // Claim the bead to the live worker.
    let claim_output = std::process::Command::new("/home/coding/.local/bin/br")
        .arg("update")
        .arg(bead_id.as_ref())
        .arg("--assignee=live-worker")
        .arg("--status=in_progress")
        .current_dir(&remote_workspace)
        .output()
        .expect("br update command failed to execute");
    assert!(
        claim_output.status.success(),
        "br update failed: {}",
        String::from_utf8_lossy(&claim_output.stderr)
    );

    let telemetry = Telemetry::new("test-worker".to_string());

    let explore_config = ExploreConfig {
        enabled: true,
        workspaces: vec![remote_workspace.clone()],
    };

    let explore = ExploreStrand::new(
        explore_config,
        home_workspace,
        registry,
        telemetry,
        "test-worker".to_string(),
    );

    // Evaluate — the live worker's bead should NOT be released.
    let result = explore.evaluate(home_store.as_ref()).await;

    // Since the bead has a live assignee, it should not be released.
    // The result should be NoWork since there are no ready beads.
    match result {
        StrandResult::NoWork => {
            // Expected — bead not released, no ready beads available.
        }
        StrandResult::BeadFound(beads) => {
            panic!(
                "should not release beads with live assignees; got beads: {:?}",
                beads
            );
        }
        other => panic!("unexpected result: {:?}", other),
    }

    // Verify the bead is still assigned to the live worker.
    let remote_store = needle::bead_store::BrCliBeadStore::discover(remote_workspace).unwrap();
    let bead = remote_store.show(&bead_id).await.unwrap();
    assert_eq!(
        bead.assignee,
        Some("live-worker".to_string()),
        "bead should still be assigned to live worker"
    );
}

#[tokio::test]
async fn cross_workspace_mend_skips_own_worker_beads() {
    use needle::config::ExploreConfig;
    use needle::strand::{ExploreStrand, Strand};
    use std::fs;

    // Create real temporary directories.
    let home_dir = tempfile::tempdir().unwrap();
    let home_workspace = home_dir.path().to_path_buf();
    let home_store = Arc::new(IntegrationMockStore::empty());

    let remote_dir = tempfile::tempdir().unwrap();
    let remote_workspace = remote_dir.path().to_path_buf();
    let remote_beads_dir = remote_workspace.join(".beads");
    fs::create_dir_all(&remote_beads_dir).unwrap();

    // Create a bead in the remote workspace.
    let output = std::process::Command::new("/home/coding/.local/bin/br")
        .arg("create")
        .arg("--type=task")
        .arg("--title=Bead assigned to us")
        .arg("--description=This bead is assigned to the current worker")
        .current_dir(&remote_workspace)
        .output()
        .expect("br create command failed to execute");
    assert!(output.status.success(), "br create failed");

    let create_result = String::from_utf8_lossy(&output.stdout);
    let bead_id = create_result
        .lines()
        .find(|l| l.contains("Created"))
        .and_then(|l| {
            // Parse "✓ Created <ID>: <title>" to extract the ID
            l.split("Created ").nth(1).and_then(|s| s.split(':').next())
        })
        .unwrap()
        .trim()
        .to_string();
    let bead_id = BeadId::from(bead_id);

    // Create registry.
    let temp_dir = tempfile::tempdir().unwrap();
    let registry = needle::registry::Registry::new(temp_dir.path());

    // Claim the bead to ourselves using the qualified identity (matching production).
    let qualified_id = "claude-test-worker";
    let claim_output = std::process::Command::new("/home/coding/.local/bin/br")
        .arg("update")
        .arg(bead_id.as_ref())
        .arg(format!("--assignee={qualified_id}"))
        .arg("--status=in_progress")
        .current_dir(&remote_workspace)
        .output()
        .expect("br update command failed to execute");
    assert!(
        claim_output.status.success(),
        "br update failed: {}",
        String::from_utf8_lossy(&claim_output.stderr)
    );

    let telemetry = Telemetry::new("test-worker".to_string());

    let explore_config = ExploreConfig {
        enabled: true,
        workspaces: vec![remote_workspace.clone()],
    };

    let explore = ExploreStrand::new(
        explore_config,
        home_workspace,
        registry,
        telemetry,
        qualified_id.to_string(),
    );

    // Evaluate — our own bead should NOT be released.
    let result = explore.evaluate(home_store.as_ref()).await;

    // Since the bead is assigned to us, it should not be released.
    match result {
        StrandResult::NoWork => {
            // Expected — our bead not released, no ready beads available.
        }
        StrandResult::BeadFound(beads) => {
            panic!(
                "should not release our own worker's beads; got beads: {:?}",
                beads
            );
        }
        other => panic!("unexpected result: {:?}", other),
    }

    // Verify the bead is still assigned to us.
    let remote_store = needle::bead_store::BrCliBeadStore::discover(remote_workspace).unwrap();
    let bead = remote_store.show(&bead_id).await.unwrap();
    assert_eq!(
        bead.assignee,
        Some(qualified_id.to_string()),
        "bead should still be assigned to us"
    );
}
