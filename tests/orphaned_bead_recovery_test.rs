// Tests for orphaned bead recovery when worker process is killed
//
// Regression test for needle-6d76f548: postcondition release did not fire for
// exit_code=0/success dispatches because the worker was killed BEFORE
// outcome handling could run.
//
// Root cause: When a worker is killed (SIGKILL, OOM, supervisor sweep) after
// do_execute() completes but before do_handle() calls the outcome handler, the
// bead remains stuck in_progress forever. The trace file exists with
// outcome=success, but the postcondition release never happened.
//
// This test simulates that scenario and verifies the boot-time recovery mechanism.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use tempfile::TempDir;

use NEEDLE::bead_store::{BeadStore, Filters};
use NEEDLE::config::Config;
use NEEDLE::telemetry::Telemetry;
use NEEDLE::types::{Bead, BeadId, BeadStatus, WorkerId};
use NEEDLE::worker::Worker;

// ── Mock BeadStore ──

struct MockBeadStore {
    beads: std::sync::Mutex<Vec<Bead>>,
    release_count: std::sync::atomic::AtomicUsize,
}

impl MockBeadStore {
    fn new() -> Self {
        Self {
            beads: std::sync::Mutex::new(Vec::new()),
            release_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn add_orphaned_bead(&self, id: &str, worker_id: &str, workspace: PathBuf) {
        let bead = Bead {
            id: BeadId::from(id.to_string()),
            title: format!("Orphaned bead {}", id),
            body: Some("This bead was left in_progress when its worker died".to_string()),
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some(worker_id.to_string()),
            labels: vec![],
            workspace,
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        self.beads.lock().unwrap().push(bead);
    }

    fn create_trace_success(&self, bead_id: &str, workspace: &PathBuf) {
        let trace_dir = workspace.join(".beads").join("traces").join(bead_id);
        fs::create_dir_all(&trace_dir).unwrap();

        let metadata = serde_json::json!({
            "bead_id": bead_id,
            "agent": "claude-code-glm-4.7",
            "provider": null,
            "model": null,
            "exit_code": 0,
            "outcome": "success",
            "duration_ms": 125065,
            "input_tokens": null,
            "output_tokens": null,
            "cost_usd": null,
            "captured_at": "2026-08-23T13:26:11Z",
            "trace_format": "claude_json",
            "pruned": false,
            "template_version": null,
            "timeout_reason": null
        });

        fs::write(trace_dir.join("metadata.json"), metadata.to_string()).unwrap();
    }

    fn release_count(&self) -> usize {
        self.release_count.load(std::sync::atomic::Ordering::SeqCst) as usize
    }
}

#[async_trait::async_trait]
impl BeadStore for MockBeadStore {
    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
        let all = self.beads.lock().unwrap().clone();
        let filtered: Vec<Bead> = all
            .into_iter()
            .filter(|b| {
                // Filter by assignee if set
                if let Some(ref assignee) = filters.assignee {
                    if b.assignee.as_deref() != Some(assignee) {
                        return false;
                    }
                }
                // Filter by excluded labels
                for label in &filters.exclude_labels {
                    if b.labels.contains(label) {
                        return false;
                    }
                }
                // Filter by excluded IDs
                if filters.exclude_ids.contains(&b.id) {
                    return false;
                }
                true
            })
            .collect();
        Ok(filtered)
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        let beads = self.beads.lock().unwrap();
        beads
            .iter()
            .find(|b| b.id == *id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("bead not found: {}", id))
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<NEEDLE::types::ClaimResult> {
        Ok(NEEDLE::types::ClaimResult::NotClaimable {
            reason: "mock".to_string(),
        })
    }

    async fn claim_auto(&self, _actor: &str) -> Result<NEEDLE::types::ClaimResult> {
        Ok(NEEDLE::types::ClaimResult::NotClaimable {
            reason: "mock".to_string(),
        })
    }

    async fn release(&self, _id: &BeadId) -> Result<()> {
        self.release_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn block(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        let bead = self.show(id).await?;
        Ok(bead.labels)
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.labels.push(label.to_string());
        }
        Ok(())
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.labels.retain(|l| l != label);
        }
        Ok(())
    }

    async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
        Ok(BeadId::from("new-bead".to_string()))
    }

    async fn doctor_repair(&self) -> Result<NEEDLE::bead_store::RepairReport> {
        Ok(NEEDLE::bead_store::RepairReport::default())
    }

    async fn doctor_check(&self) -> Result<NEEDLE::bead_store::RepairReport> {
        Ok(NEEDLE::bead_store::RepairReport::default())
    }

    async fn full_rebuild(&self) -> Result<()> {
        Ok(())
    }

    async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn remove_dependency(&self, _blocked_id: &BeadId, _blocker_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.assignee = None;
        }
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

// ── Tests ──

#[tokio::test]
async fn orphaned_bead_recovery_releases_stuck_beads_on_boot() {
    // This test reproduces the exact scenario from needle-6d76f548:
    // 1. Worker completes do_execute() with exit_code=0
    // 2. Trace file is written with outcome=success
    // 3. Worker is killed before do_handle() can run
    // 4. Bead remains stuck in_progress
    // 5. On next boot, recovery should release the orphaned bead

    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path().to_path_buf();
    let worker_id = "test-worker-orphan-recovery";

    let store = Arc::new(MockBeadStore::new());
    let bead_id_1 = "orphan-1";
    let bead_id_2 = "orphan-2";

    // Create two orphaned beads with trace files showing success
    store.add_orphaned_bead(bead_id_1, worker_id, workspace.clone());
    store.create_trace_success(bead_id_1, &workspace);
    store.add_orphaned_bead(bead_id_2, worker_id, workspace.clone());
    store.create_trace_success(bead_id_2, &workspace);

    // Simulate worker boot - the recovery should run automatically
    let config = Config::default();
    let telemetry = Telemetry::new(worker_id.to_string(), temp_dir.path().to_path_buf());

    // Create worker with the mock store
    let mut worker = Worker::new(config, worker_id.to_string(), store.clone());

    // Boot the worker - this should trigger orphaned bead recovery
    worker.boot().await.unwrap();

    // Verify both beads were released
    assert_eq!(
        store.release_count(),
        2,
        "both orphaned beads should have been released during worker boot"
    );

    // Verify beads are no longer assigned to this worker
    let bead_1 = store.show(&BeadId::from(bead_id_1)).await.unwrap();
    assert!(
        bead_1.assignee.is_none(),
        "orphaned bead 1 should be unassigned after recovery"
    );
    assert_ne!(
        bead_1.status,
        BeadStatus::InProgress,
        "orphaned bead 1 should not be in_progress after recovery"
    );

    let bead_2 = store.show(&BeadId::from(bead_id_2)).await.unwrap();
    assert!(
        bead_2.assignee.is_none(),
        "orphaned bead 2 should be unassigned after recovery"
    );
    assert_ne!(
        bead_2.status,
        BeadStatus::InProgress,
        "orphaned bead 2 should not be in_progress after recovery"
    );
}

#[tokio::test]
async fn orphaned_bead_recovery_does_not_release_beads_without_trace() {
    // Test that recovery only releases beads that have trace files showing success
    // Beads without trace files (or with trace showing failure) should be left alone

    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path().to_path_buf();
    let worker_id = "test-worker-no-trace";

    let store = Arc::new(MockBeadStore::new());
    let bead_id = "orphan-no-trace";

    // Create an orphaned bead WITHOUT a trace file
    store.add_orphaned_bead(bead_id, worker_id, workspace.clone());

    let config = Config::default();
    let mut worker = Worker::new(config, worker_id.to_string(), store.clone());

    // Boot the worker
    worker.boot().await.unwrap();

    // Verify the bead was NOT released (no trace file exists)
    assert_eq!(
        store.release_count(),
        0,
        "bead without trace file should not be released during recovery"
    );

    let bead = store.show(&BeadId::from(bead_id)).await.unwrap();
    assert_eq!(
        bead.assignee.as_deref(),
        Some(worker_id),
        "bead without trace should still be assigned to worker"
    );
}
