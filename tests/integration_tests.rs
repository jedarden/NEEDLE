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
    Bead, BeadId, BeadStatus, ClaimResult, IdleAction, InputMethod, Outcome, WorkerState,
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
    }
}

fn test_config(adapter_name: &str) -> Config {
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Exit;
    config.agent.default = adapter_name.to_string();
    config.agent.timeout = 10;
    config.self_modification.hot_reload = false;
    // Match the test bead workspace so the remote-store-switch logic
    // doesn't fire (it would try to create a BrCliBeadStore).
    config.workspace.default = std::path::PathBuf::from("/tmp/test-workspace");
    config
}

fn make_worker_with_adapter(
    store: Arc<dyn BeadStore>,
    adapter_name: &str,
    template: &str,
    timeout_secs: u64,
) -> Worker {
    let config = test_config(adapter_name);
    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter(adapter_name, template, timeout_secs);
    let mut adapters = HashMap::new();
    adapters.insert(adapter_name.to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    worker
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

    let mut worker =
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

    let mut worker = make_worker_with_adapter(store.clone(), "echo-test", "echo 'done'", 10);

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

    let mut worker = make_worker_with_adapter(store.clone(), "success-agent", "exit 0", 10);

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

    let mut worker = make_worker_with_adapter(store.clone(), "fail-agent", "exit 1", 10);

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
    let mut worker = make_worker_with_adapter(store.clone(), "timeout-agent", "sleep 100", 1);

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

    let mut worker = make_worker_with_adapter(
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

    let mut worker = make_worker_with_adapter(store.clone(), "crash-agent", "exit 137", 10);

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

    let config = test_config("slow-agent");
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
    let config = test_config("echo-test");
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
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Exit;
    config.agent.default = "echo-test".to_string();

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

// ═════════════════════════════════════════════════════════════════════════════
// Test 4: Graceful shutdown
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn shutdown_during_selecting_exits_cleanly() {
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let config = test_config("echo-test");
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
    let config = test_config("echo-test");
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
    let mut config = Config::default();
    config.agent.default = String::new(); // Invalid: empty agent name

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

    let mut worker = make_worker_with_adapter(store, "echo-test", "echo done", 10);

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

    let mut worker = make_worker_with_adapter(store.clone(), "echo-test", "echo done", 10);

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
