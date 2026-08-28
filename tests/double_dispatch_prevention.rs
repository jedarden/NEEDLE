//! Integration test for double-dispatch prevention.
//!
//! This test verifies that the atomic claim verification at dispatch time
//! prevents two workers from dispatching the same bead concurrently.
//!
//! Test scenario:
//! 1. Create a bead store with a single open bead
//! 2. Worker A claims the bead (bead becomes InProgress with assignee=worker-a)
//! 3. Worker A begins dispatch (calls verify_claim_at_dispatch - should succeed)
//! 4. Worker B attempts to dispatch the same bead (should fail verification)
//! 5. Verify Worker A succeeds, Worker B fails, and bead remains owned by Worker A

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Barrier;
use tokio::time::{sleep, Instant};

use needle::bead_store::{BeadStore, Filters};
use needle::claim::Claimer;
use needle::telemetry::Telemetry;
use needle::types::{Bead, BeadId, BeadStatus, ClaimResult};

/// Helper to create a test bead.
fn create_test_bead(id: &str) -> Bead {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "title": "Test bead for double-dispatch prevention",
        "description": "This bead tests that two workers cannot dispatch it concurrently",
        "priority": 2,
        "status": "open",
        "assignee": null,
        "labels": [],
        "source_repo": "test-repo",
        "dependencies": [],
        "dependents": [],
        "comments": [],
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z"
    }))
    .expect("valid bead JSON")
}

/// Mock bead store for testing.
struct MockStore {
    beads: Mutex<Vec<Bead>>,
}

impl MockStore {
    fn new(beads: Vec<Bead>) -> Self {
        Self {
            beads: Mutex::new(beads),
        }
    }
}

#[async_trait::async_trait]
impl BeadStore for MockStore {
    async fn ready(&self, _filters: &Filters) -> anyhow::Result<Vec<Bead>> {
        Ok(self
            .beads
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b.status == BeadStatus::Open && b.assignee.is_none())
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> anyhow::Result<Vec<Bead>> {
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn show(&self, id: &BeadId) -> anyhow::Result<Bead> {
        self.beads
            .lock()
            .unwrap()
            .iter()
            .find(|b| b.id == *id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> anyhow::Result<ClaimResult> {
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

    async fn claim_auto(&self, actor: &str) -> anyhow::Result<ClaimResult> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads
            .iter_mut()
            .find(|b| b.status == BeadStatus::Open && b.assignee.is_none())
        {
            bead.status = BeadStatus::InProgress;
            bead.assignee = Some(actor.to_string());
            Ok(ClaimResult::Claimed(bead.clone()))
        } else {
            Ok(ClaimResult::NotClaimable {
                reason: "no available beads".to_string(),
            })
        }
    }

    async fn release(&self, id: &BeadId) -> anyhow::Result<()> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::Open;
            bead.assignee = None;
        }
        Ok(())
    }

    async fn block(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn claim_status(&self, id: &BeadId) -> anyhow::Result<needle::types::ClaimStatus> {
        let beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter().find(|b| b.id == *id) {
            Ok(needle::types::ClaimStatus {
                status: bead.status.clone(),
                assignee: bead.assignee.clone(),
                revision: None,
            })
        } else {
            Err(anyhow::anyhow!("bead not found: {id}"))
        }
    }

    async fn create_bead(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> anyhow::Result<BeadId> {
        let id = BeadId::from(format!("test-{}", title.replace(' ', "-").to_lowercase()));
        let bead = serde_json::from_value::<Bead>(serde_json::json!({
            "id": id,
            "title": title,
            "description": body,
            "priority": 2,
            "status": "open",
            "assignee": null,
            "labels": labels,
            "source_repo": "test-repo",
            "dependencies": [],
            "dependents": [],
            "comments": [],
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z"
        }))?;
        self.beads.lock().unwrap().push(bead);
        Ok(id)
    }

    async fn add_dependency(
        &self,
        _blocker_id: &BeadId,
        _blocked_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_dependency(
        &self,
        _blocked_id: &BeadId,
        _blocker_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> anyhow::Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport {
            warnings: Vec::new(),
            fixed: Vec::new(),
        })
    }

    async fn doctor_check(&self) -> anyhow::Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport {
            warnings: Vec::new(),
            fixed: Vec::new(),
        })
    }

    async fn full_rebuild(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn double_dispatch_prevention_blocks_second_worker() {
    // Create a mock store with a single bead
    let test_bead = create_test_bead("test-double-dispatch-1");
    let store: Arc<dyn BeadStore> = Arc::new(MockStore::new(vec![test_bead.clone()]));

    // Create claimers for two workers
    let lock_dir = TempDir::new().expect("create temp dir");
    let telemetry_a = Telemetry::new("worker-a".to_string());
    let claimer_a = Claimer::new(
        store.clone(),
        lock_dir.path().to_path_buf(),
        5,
        100,
        telemetry_a,
    );

    let lock_dir = TempDir::new().expect("create temp dir");
    let telemetry_b = Telemetry::new("worker-b".to_string());
    let claimer_b = Claimer::new(
        store.clone(),
        lock_dir.path().to_path_buf(),
        5,
        100,
        telemetry_b,
    );

    let bead_id = BeadId::from("test-double-dispatch-1");
    let worker_a = "worker-a";
    let worker_b = "worker-b";

    // Step 1: Worker A claims the bead
    let claim_result_a = claimer_a
        .claim_auto(worker_a, "test-strand")
        .await
        .expect("worker a claim succeeds");

    assert!(
        matches!(claim_result_a, ClaimResult::Claimed(_)),
        "worker a should successfully claim the bead"
    );

    // Verify the bead is now InProgress with worker-a as assignee
    let claim_status = store
        .claim_status(&bead_id)
        .await
        .expect("get claim status");
    assert_eq!(claim_status.status, BeadStatus::InProgress);
    assert_eq!(claim_status.assignee.as_deref(), Some(worker_a));

    // Step 2: Worker A's dispatch-time verification should succeed
    let verification_a = claimer_a
        .verify_claim_at_dispatch(&bead_id, worker_a)
        .await
        .expect("worker a verification succeeds");

    assert!(
        verification_a,
        "worker a's dispatch-time verification should succeed"
    );

    // Step 3: Worker B attempts to dispatch the same bead (should fail)
    let verification_b = claimer_b
        .verify_claim_at_dispatch(&bead_id, worker_b)
        .await
        .expect("worker b verification completes");

    assert!(
        !verification_b,
        "worker b's dispatch-time verification should fail - bead is owned by worker a"
    );

    // Step 4: Verify bead state remains owned by Worker A
    let claim_status_final = store
        .claim_status(&bead_id)
        .await
        .expect("get final claim status");
    assert_eq!(claim_status_final.status, BeadStatus::InProgress);
    assert_eq!(
        claim_status_final.assignee.as_deref(),
        Some(worker_a),
        "bead should remain owned by worker a"
    );
}

#[tokio::test]
async fn double_dispatch_prevention_with_concurrent_dispatch_attempts() {
    // Create a mock store with a single bead
    let test_bead = create_test_bead("test-concurrent-dispatch-2");
    let store: Arc<dyn BeadStore> = Arc::new(MockStore::new(vec![test_bead.clone()]));

    let lock_dir = TempDir::new().expect("create temp dir");
    let telemetry_a = Telemetry::new("worker-a".to_string());
    let claimer_a = Claimer::new(
        store.clone(),
        lock_dir.path().to_path_buf(),
        5,
        100,
        telemetry_a,
    );

    let lock_dir = TempDir::new().expect("create temp dir");
    let telemetry_b = Telemetry::new("worker-b".to_string());
    let claimer_b = Claimer::new(
        store.clone(),
        lock_dir.path().to_path_buf(),
        5,
        100,
        telemetry_b,
    );

    let bead_id = BeadId::from("test-concurrent-dispatch-2");
    let worker_a = "worker-a";
    let worker_b = "worker-b";

    // Worker A claims the bead first
    let claim_result_a = claimer_a
        .claim_auto(worker_a, "test-strand")
        .await
        .expect("worker a claim succeeds");

    assert!(
        matches!(claim_result_a, ClaimResult::Claimed(_)),
        "worker a should successfully claim the bead"
    );

    // Use a barrier to synchronize both workers attempting dispatch simultaneously
    let barrier = Arc::new(Barrier::new(2));
    let barrier_clone = barrier.clone();

    let _store_clone = store.clone();
    let bead_id_clone = bead_id.clone();

    // Worker A's dispatch attempt
    let handle_a = tokio::spawn(async move {
        barrier_clone.wait().await; // Wait for both workers to be ready
        let verification = claimer_a
            .verify_claim_at_dispatch(&bead_id_clone, worker_a)
            .await
            .expect("worker a verification completes");

        // Simulate dispatch work
        sleep(Duration::from_millis(50)).await;

        (worker_a.to_string(), verification)
    });

    // Worker B's dispatch attempt (should fail verification)
    let handle_b = tokio::spawn(async move {
        barrier.wait().await; // Wait for both workers to be ready

        // Small delay to ensure worker A starts first (but still concurrent)
        sleep(Duration::from_millis(5)).await;

        let verification = claimer_b
            .verify_claim_at_dispatch(&bead_id_clone, worker_b)
            .await
            .expect("worker b verification completes");

        (worker_b.to_string(), verification)
    });

    // Wait for both dispatch attempts to complete
    let (result_a, result_b) = tokio::join!(handle_a, handle_b);
    let (worker_a_name, verification_a) = result_a.expect("worker a completes");
    let (worker_b_name, verification_b) = result_b.expect("worker b completes");

    // Verify results
    assert_eq!(worker_a_name, "worker-a");
    assert_eq!(worker_b_name, "worker-b");

    assert!(
        verification_a,
        "worker a's dispatch-time verification should succeed"
    );
    assert!(
        !verification_b,
        "worker b's dispatch-time verification should fail - bead is owned by worker a"
    );

    // Final bead state verification
    let claim_status_final = store
        .claim_status(&bead_id_clone)
        .await
        .expect("get final claim status");
    assert_eq!(claim_status_final.status, BeadStatus::InProgress);
    assert_eq!(
        claim_status_final.assignee.as_deref(),
        Some(worker_a),
        "bead should remain owned by worker a after concurrent dispatch attempts"
    );
}

#[tokio::test]
async fn double_dispatch_prevention_after_bead_reassignment() {
    // Create a mock store with a single bead
    let test_bead = create_test_bead("test-reassignment-dispatch-3");
    let store: Arc<dyn BeadStore> = Arc::new(MockStore::new(vec![test_bead.clone()]));

    let lock_dir = TempDir::new().expect("create temp dir");
    let telemetry_a = Telemetry::new("worker-a".to_string());
    let claimer_a = Claimer::new(
        store.clone(),
        lock_dir.path().to_path_buf(),
        5,
        100,
        telemetry_a,
    );

    let lock_dir = TempDir::new().expect("create temp dir");
    let telemetry_b = Telemetry::new("worker-b".to_string());
    let claimer_b = Claimer::new(
        store.clone(),
        lock_dir.path().to_path_buf(),
        5,
        100,
        telemetry_b,
    );

    let bead_id = BeadId::from("test-reassignment-dispatch-3");
    let worker_a = "worker-a";
    let worker_b = "worker-b";

    // Worker A claims the bead
    let claim_result_a = claimer_a
        .claim_auto(worker_a, "test-strand")
        .await
        .expect("worker a claim succeeds");

    assert!(
        matches!(claim_result_a, ClaimResult::Claimed(_)),
        "worker a should successfully claim the bead"
    );

    // Worker A's initial verification should succeed
    let verification_a_initial = claimer_a
        .verify_claim_at_dispatch(&bead_id, worker_a)
        .await
        .expect("worker a initial verification succeeds");

    assert!(
        verification_a_initial,
        "worker a's initial dispatch-time verification should succeed"
    );

    // Simulate bead reassignment: Worker A releases, Worker B claims
    store
        .release(&bead_id)
        .await
        .expect("release bead from worker a");

    let claim_result_b = claimer_b
        .claim_auto(worker_b, "test-strand")
        .await
        .expect("worker b claim succeeds");

    assert!(
        matches!(claim_result_b, ClaimResult::Claimed(_)),
        "worker b should successfully claim the bead after reassignment"
    );

    // Worker A's subsequent dispatch attempt should now fail (bead reassigned to B)
    let verification_a_subsequent = claimer_a
        .verify_claim_at_dispatch(&bead_id, worker_a)
        .await
        .expect("worker a subsequent verification completes");

    assert!(
        !verification_a_subsequent,
        "worker a's dispatch-time verification should fail after bead reassignment to worker b"
    );

    // Worker B's verification should succeed
    let verification_b = claimer_b
        .verify_claim_at_dispatch(&bead_id, worker_b)
        .await
        .expect("worker b verification succeeds");

    assert!(
        verification_b,
        "worker b's dispatch-time verification should succeed after reassignment"
    );

    // Final bead state verification
    let claim_status_final = store
        .claim_status(&bead_id)
        .await
        .expect("get final claim status");
    assert_eq!(claim_status_final.status, BeadStatus::InProgress);
    assert_eq!(
        claim_status_final.assignee.as_deref(),
        Some(worker_b),
        "bead should be owned by worker b after reassignment"
    );
}

#[tokio::test]
async fn double_dispatch_prevention_performance_under_load() {
    // Create a mock store with a single bead
    let test_bead = create_test_bead("test-load-dispatch-4");
    let store: Arc<dyn BeadStore> = Arc::new(MockStore::new(vec![test_bead.clone()]));

    let lock_dir = TempDir::new().expect("create temp dir");
    let telemetry_a = Telemetry::new("worker-a".to_string());
    let claimer_a = Claimer::new(
        store.clone(),
        lock_dir.path().to_path_buf(),
        5,
        100,
        telemetry_a,
    );

    let bead_id = BeadId::from("test-load-dispatch-4");
    let worker_a = "worker-a";

    // Worker A claims the bead
    let claim_result_a = claimer_a
        .claim_auto(worker_a, "test-strand")
        .await
        .expect("worker a claim succeeds");

    assert!(
        matches!(claim_result_a, ClaimResult::Claimed(_)),
        "worker a should successfully claim the bead"
    );

    // Perform multiple rapid verifications to test performance
    let iterations = 100;
    let start = Instant::now();

    for _ in 0..iterations {
        let verification = claimer_a
            .verify_claim_at_dispatch(&bead_id, worker_a)
            .await
            .expect("verification succeeds");

        assert!(
            verification,
            "worker a's verification should succeed on every attempt"
        );
    }

    let elapsed = start.elapsed();

    // Performance assertion: 100 verifications should complete in under 1 second
    assert!(
        elapsed < Duration::from_secs(1),
        "100 verifications should complete in under 1 second, took {}",
        elapsed.as_secs_f64()
    );

    // Final bead state verification
    let claim_status_final = store
        .claim_status(&bead_id)
        .await
        .expect("get final claim status");
    assert_eq!(claim_status_final.status, BeadStatus::InProgress);
    assert_eq!(
        claim_status_final.assignee.as_deref(),
        Some(worker_a),
        "bead should remain owned by worker a after load test"
    );
}
