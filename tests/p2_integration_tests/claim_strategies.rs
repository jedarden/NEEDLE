use async_trait::async_trait;
use needle::bead_store::{
    execute_claim_auto_strategy, execute_claim_strategy, ClaimAutoStrategy, ClaimStrategy,
    ClaimStrategyOperations, CompareAndSetOutcome,
};
use needle::types::{Bead, BeadId, BeadStatus, ClaimResult};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct MockOperations {
    bead: Bead,
    ready: Vec<Bead>,
    cas_outcomes: Mutex<VecDeque<CompareAndSetOutcome>>,
    events: Mutex<Vec<&'static str>>,
    scan_at: Mutex<Option<Instant>>,
    mutation_at: Mutex<Option<Instant>>,
    mutation_delay: Duration,
}

impl MockOperations {
    fn new(bead: Bead) -> Self {
        Self {
            ready: vec![bead.clone()],
            bead,
            cas_outcomes: Mutex::new(VecDeque::new()),
            events: Mutex::new(Vec::new()),
            scan_at: Mutex::new(None),
            mutation_at: Mutex::new(None),
            mutation_delay: Duration::ZERO,
        }
    }
}

#[async_trait]
impl ClaimStrategyOperations for MockOperations {
    async fn show_for_claim(&self, _bead_id: &BeadId) -> anyhow::Result<Bead> {
        self.events.lock().unwrap().push("show");
        Ok(self.bead.clone())
    }

    async fn compare_and_set_claim(
        &self,
        _bead_id: &BeadId,
        _actor: &str,
        _expected_version: &str,
    ) -> anyhow::Result<CompareAndSetOutcome> {
        if !self.mutation_delay.is_zero() {
            tokio::time::sleep(self.mutation_delay).await;
        }
        *self.mutation_at.lock().unwrap() = Some(Instant::now());
        self.events.lock().unwrap().push("compare_and_set");
        Ok(self
            .cas_outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| CompareAndSetOutcome::Claimed(Box::new(claimed_bead("worker")))))
    }

    async fn batch_claim(&self, _bead_id: &BeadId, _actor: &str) -> anyhow::Result<ClaimResult> {
        self.events.lock().unwrap().push("batch_claim");
        Ok(ClaimResult::Claimed(claimed_bead("worker")))
    }

    async fn atomic_claim_auto(&self, _actor: &str) -> anyhow::Result<ClaimResult> {
        self.events.lock().unwrap().push("atomic_claim_auto");
        Ok(ClaimResult::Claimed(claimed_bead("worker")))
    }

    async fn ready_for_claim(&self) -> anyhow::Result<Vec<Bead>> {
        *self.scan_at.lock().unwrap() = Some(Instant::now());
        self.events.lock().unwrap().push("ready");
        Ok(self.ready.clone())
    }
}

fn open_bead() -> Bead {
    serde_json::from_value(serde_json::json!({
        "id": "test-1",
        "title": "test",
        "description": null,
        "priority": 2,
        "status": "open",
        "assignee": null,
        "labels": [],
        "source_repo": "",
        "dependencies": [],
        "dependents": [],
        "comments": [],
        "created_at": "2026-08-12T00:00:00Z",
        "updated_at": "2026-08-12T00:00:00Z"
    }))
    .unwrap()
}

fn claimed_bead(actor: &str) -> Bead {
    let mut bead = open_bead();
    bead.status = BeadStatus::InProgress;
    bead.assignee = Some(actor.to_string());
    bead
}

#[tokio::test]
async fn compare_and_set_reports_a_lost_race() {
    let operations = MockOperations::new(open_bead());
    operations
        .cas_outcomes
        .lock()
        .unwrap()
        .push_back(CompareAndSetOutcome::RaceLost {
            claimed_by: "other-worker".to_string(),
        });

    let result = execute_claim_strategy(
        &operations,
        ClaimStrategy::CompareAndSet,
        &BeadId::from("test-1"),
        "this-worker",
    )
    .await
    .unwrap();

    assert!(matches!(
        result,
        ClaimResult::RaceLost { claimed_by } if claimed_by == "other-worker"
    ));
}

#[tokio::test]
async fn compare_and_set_retries_a_version_change() {
    let operations = MockOperations::new(open_bead());
    operations.cas_outcomes.lock().unwrap().extend([
        CompareAndSetOutcome::VersionChanged,
        CompareAndSetOutcome::Claimed(Box::new(claimed_bead("this-worker"))),
    ]);

    let result = execute_claim_strategy(
        &operations,
        ClaimStrategy::CompareAndSet,
        &BeadId::from("test-1"),
        "this-worker",
    )
    .await
    .unwrap();

    assert!(matches!(result, ClaimResult::Claimed(_)));
    assert_eq!(
        *operations.events.lock().unwrap(),
        ["show", "compare_and_set", "show", "compare_and_set"]
    );
}

#[tokio::test]
async fn batch_and_atomic_strategies_each_use_one_backend_primitive() {
    let operations = MockOperations::new(open_bead());
    execute_claim_strategy(
        &operations,
        ClaimStrategy::BatchOp,
        &BeadId::from("test-1"),
        "worker",
    )
    .await
    .unwrap();
    execute_claim_auto_strategy(
        &operations,
        ClaimAutoStrategy::AtomicSubcommand,
        ClaimStrategy::BatchOp,
        "worker",
    )
    .await
    .unwrap();

    assert_eq!(
        *operations.events.lock().unwrap(),
        ["batch_claim", "atomic_claim_auto"]
    );
}

#[tokio::test]
async fn non_atomic_scan_exposes_and_survives_the_toctou_window() {
    let mut operations = MockOperations::new(open_bead());
    operations.mutation_delay = Duration::from_millis(10);
    operations
        .cas_outcomes
        .lock()
        .unwrap()
        .push_back(CompareAndSetOutcome::RaceLost {
            claimed_by: "faster-worker".to_string(),
        });

    let result = execute_claim_auto_strategy(
        &operations,
        ClaimAutoStrategy::NonAtomicScan,
        ClaimStrategy::CompareAndSet,
        "slower-worker",
    )
    .await
    .unwrap();

    assert!(matches!(result, ClaimResult::NotClaimable { .. }));
    assert_eq!(
        *operations.events.lock().unwrap(),
        ["ready", "show", "compare_and_set"]
    );
    let scan_at = operations.scan_at.lock().unwrap().unwrap();
    let mutation_at = operations.mutation_at.lock().unwrap().unwrap();
    assert!(mutation_at.duration_since(scan_at) >= Duration::from_millis(10));
}
