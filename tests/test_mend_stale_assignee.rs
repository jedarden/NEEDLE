// Test to reproduce the bug scenario: worker with fresh heartbeat working on different bead
// This should clear the assignee because current_bead != assigned bead

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;

// Import all necessary types directly
use needle::bead_store::{BeadStore, Filters};
use needle::health::HeartbeatData;
use needle::registry::Registry;
use needle::strand::{mend::MendStrand, Strand};
use needle::telemetry::Telemetry;
use needle::types::{Bead, BeadId, BeadStatus, StrandResult};

#[derive(Clone)]
struct SimpleStore {
    beads: std::sync::Arc<std::sync::Mutex<Vec<Bead>>>,
    clear_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl SimpleStore {
    fn new(beads: Vec<Bead>) -> Self {
        Self {
            beads: std::sync::Arc::new(std::sync::Mutex::new(beads)),
            clear_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    fn clear_count(&self) -> u32 {
        self.clear_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl BeadStore for SimpleStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(vec![])
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn show(&self, _id: &BeadId) -> Result<Bead> {
        anyhow::bail!("not implemented")
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<needle::types::ClaimResult> {
        anyhow::bail!("not implemented")
    }

    async fn claim_auto(&self, _actor: &str) -> Result<needle::types::ClaimResult> {
        anyhow::bail!("not implemented")
    }

    async fn release(&self, _id: &BeadId) -> Result<()> {
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
        Ok(BeadId::from("test"))
    }

    async fn doctor_repair(&self) -> Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport::default())
    }

    async fn doctor_check(&self) -> Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport::default())
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
        if let Some(bead) = beads.iter_mut().find(|b| &b.id == id) {
            bead.assignee = None;
            self.clear_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            println!("CLEARED assignee for bead {}", id);
            Ok(())
        } else {
            anyhow::bail!("bead not found")
        }
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

fn make_bead(id: &str, status: BeadStatus, assignee: Option<&str>) -> Bead {
    let dt = Utc::now();
    Bead {
        id: BeadId::from(id),
        title: format!("Bead {id}"),
        body: None,
        priority: 1,
        status,
        assignee: assignee.map(|s| s.to_string()),
        labels: vec![],
        workspace: PathBuf::from("/tmp/test"),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: dt,
        updated_at: dt,
    }
}

fn write_heartbeat(dir: &Path, data: &HeartbeatData) {
    let name = if data.qualified_id.is_empty() {
        &data.worker_id
    } else {
        &data.qualified_id
    };
    let path = dir.join(format!("{}.json", name));
    let json = serde_json::to_string_pretty(data).unwrap();
    std::fs::write(path, json).unwrap();
}

fn make_fresh_heartbeat(
    worker_id: &str,
    qualified_id: &str,
    current_bead: Option<&str>,
) -> HeartbeatData {
    HeartbeatData {
        worker_id: worker_id.to_string(),
        qualified_id: qualified_id.to_string(),
        pid: std::process::id(), // Our own PID = alive
        state: needle::types::WorkerState::Executing,
        current_bead: current_bead.map(BeadId::from),
        workspace: PathBuf::from("/tmp/test"),
        last_heartbeat: Utc::now(), // Fresh
        started_at: Utc::now() - chrono::Duration::seconds(3600),
        beads_processed: 5,
        session: worker_id.to_string(),
        is_idle: false,
        current_task: current_bead.map(|s| s.to_string()),
        model: "claude-sonnet-4".to_string(),
        heartbeat_file: None,
    }
}

#[tokio::test]
async fn test_bug_scenario() {
    // Setup
    let hb_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();

    // Create heartbeat for worker "test-worker-1" working on bead "different-bead"
    let heartbeat = make_fresh_heartbeat(
        "test-worker-1",
        "claude-code-glm-5-test-worker-1",
        Some("different-bead"), // Working on DIFFERENT bead
    );
    write_heartbeat(hb_dir.path(), &heartbeat);
    println!(
        "Created heartbeat: worker={}, current_bead={:?}",
        heartbeat.qualified_id, heartbeat.current_bead
    );

    // Create an OPEN bead assigned to this worker but it's NOT the current_bead
    let bead = make_bead(
        "stale-assigned-bead",
        BeadStatus::Open,
        Some("claude-code-glm-5-test-worker-1"),
    );
    println!(
        "Created bead: id={}, status={}, assignee={:?}",
        bead.id, bead.status, bead.assignee
    );

    let store = SimpleStore::new(vec![bead]);
    let registry = Registry::new(reg_dir.path());

    let config = needle::config::MendConfig::default();
    let heartbeat_ttl = Duration::from_secs(300);
    let telemetry = Telemetry::new("test".to_string());

    let mend = MendStrand::new(
        config,
        hb_dir.path().to_path_buf(),
        heartbeat_ttl,
        lock_dir.path().to_path_buf(),
        "test-mend-worker".to_string(),
        registry,
        telemetry,
        PathBuf::from("/tmp/logs"),
        0,
        PathBuf::from("/tmp/traces"),
        30,
        7,
        PathBuf::from("/tmp/workspace"),
        80,
        tempfile::tempdir().unwrap().path().to_path_buf(),
        needle::config::LimitsConfig::default(),
    );

    // Run Mend
    let result = mend.evaluate(&store, &HashSet::new()).await;

    // Check: assignee should be CLEARED because heartbeat shows different bead
    println!("Mend result: {:?}", result);
    println!("Clear count: {}", store.clear_count());

    assert!(
        matches!(result, StrandResult::WorkCreated),
        "Expected WorkCreated after clearing stale assignee, got: {:?}",
        result
    );
    assert_eq!(
        store.clear_count(),
        1,
        "Expected exactly 1 assignee cleared, got: {}",
        store.clear_count()
    );
}

#[tokio::test]
async fn test_control_scenario() {
    // Setup
    let hb_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();

    // Create heartbeat for worker "test-worker-1" working on THIS bead
    let heartbeat = make_fresh_heartbeat(
        "test-worker-1",
        "claude-code-glm-5-test-worker-1",
        Some("this-bead"), // Working on THIS bead
    );
    write_heartbeat(hb_dir.path(), &heartbeat);
    println!(
        "Created heartbeat: worker={}, current_bead={:?}",
        heartbeat.qualified_id, heartbeat.current_bead
    );

    // Create an OPEN bead assigned to this worker AND it's the current_bead
    let bead = make_bead(
        "this-bead",
        BeadStatus::Open,
        Some("claude-code-glm-5-test-worker-1"),
    );
    println!(
        "Created bead: id={}, status={}, assignee={:?}",
        bead.id, bead.status, bead.assignee
    );

    let store = SimpleStore::new(vec![bead]);
    let registry = Registry::new(reg_dir.path());

    let config = needle::config::MendConfig::default();
    let heartbeat_ttl = Duration::from_secs(300);
    let telemetry = Telemetry::new("test".to_string());

    let mend = MendStrand::new(
        config,
        hb_dir.path().to_path_buf(),
        heartbeat_ttl,
        lock_dir.path().to_path_buf(),
        "test-mend-worker".to_string(),
        registry,
        telemetry,
        PathBuf::from("/tmp/logs"),
        0,
        PathBuf::from("/tmp/traces"),
        30,
        7,
        PathBuf::from("/tmp/workspace"),
        80,
        tempfile::tempdir().unwrap().path().to_path_buf(),
        needle::config::LimitsConfig::default(),
    );

    // Run Mend
    let result = mend.evaluate(&store, &HashSet::new()).await;

    // Check: assignee should NOT be cleared because heartbeat shows this is the active bead
    println!("Mend result: {:?}", result);
    println!("Clear count: {}", store.clear_count());

    assert!(
        matches!(result, StrandResult::NoWork),
        "Expected NoWork when assignee is actively working on this bead, got: {:?}",
        result
    );
    assert_eq!(
        store.clear_count(),
        0,
        "Expected 0 assignees cleared (worker is active on this bead), got: {}",
        store.clear_count()
    );
}

#[tokio::test]
async fn test_no_heartbeat_but_in_registry() {
    // This is the regression test for the bug where workers with --count 1
    // relaunch under the same name indefinitely, staying in the registry
    // even when their heartbeat files are missing. The old fallback logic
    // checked registry liveness and would never clear the assignee.

    // Setup
    let hb_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();

    // Register a worker (simulating it's in the registry)
    let registry = Registry::new(reg_dir.path());
    let worker_entry = needle::registry::WorkerEntry {
        id: "claude-code-glm-5-test-worker-1".to_string(),
        pid: std::process::id(), // Our own PID = alive
        workspace: PathBuf::from("/tmp/test"),
        agent: "test".to_string(),
        model: Some("claude-sonnet-4".to_string()),
        provider: Some("anthropic".to_string()),
        started_at: chrono::Utc::now(),
        beads_processed: 0,
        config_reload_generation: 0,
    };
    registry.register(worker_entry).unwrap();

    // DO NOT write a heartbeat file - this simulates the case where
    // the heartbeat is missing (deleted, not written, etc.)

    // Create an OPEN bead assigned to this worker
    let bead = make_bead(
        "orphaned-assigned-bead",
        BeadStatus::Open,
        Some("claude-code-glm-5-test-worker-1"),
    );
    println!(
        "Created bead: id={}, status={}, assignee={:?}",
        bead.id, bead.status, bead.assignee
    );

    let store = SimpleStore::new(vec![bead]);

    let config = needle::config::MendConfig::default();
    let heartbeat_ttl = Duration::from_secs(300);
    let telemetry = Telemetry::new("test".to_string());

    let mend = MendStrand::new(
        config,
        hb_dir.path().to_path_buf(),
        heartbeat_ttl,
        lock_dir.path().to_path_buf(),
        "test-mend-worker".to_string(),
        registry,
        telemetry,
        PathBuf::from("/tmp/logs"),
        0,
        PathBuf::from("/tmp/traces"),
        30,
        7,
        PathBuf::from("/tmp/workspace"),
        80,
        tempfile::tempdir().unwrap().path().to_path_buf(),
        needle::config::LimitsConfig::default(),
    );

    // Run Mend
    let result = mend.evaluate(&store, &HashSet::new()).await;

    // Check: assignee SHOULD be cleared even though worker is in registry
    // The fix treats "no heartbeat" as stale regardless of registry liveness
    println!("Mend result: {:?}", result);
    println!("Clear count: {}", store.clear_count());

    assert!(
        matches!(result, StrandResult::WorkCreated),
        "Expected WorkCreated after clearing stale assignee (no heartbeat), got: {:?}",
        result
    );
    assert_eq!(
        store.clear_count(),
        1,
        "Expected exactly 1 assignee cleared (worker in registry but no heartbeat), got: {}",
        store.clear_count()
    );
}

fn main() {
    println!("Run with: cargo test --test test_mend_stale_assignee");
}
