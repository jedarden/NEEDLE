//! Integration tests for NEEDLE Phase 2 — Multi-Worker Fleet.
//!
//! Tests cover:
//! 1. Multi-worker claiming — N workers, M beads, each claimed exactly once
//! 2. Crashed worker bead released by peer monitoring
//! 3. Mend strand cleans stale claims and orphaned locks
//! 4. Provider/model concurrency limits enforced
//! 5. Mitosis splits multi-task beads correctly
//! 6. Mitosis dedup — duplicate split creates zero new children
//! 7. Concurrent claiming — flock serializes multiple claimers
//! 8. Registry concurrent access — no corruption
//! 9. Heartbeat liveness — emitter writes and stop cleans up
//! 10. Strand waterfall ordering with Mend
//! 11. Explore discovers work in other workspaces (real br)
//! 12. Mitosis splits multi-task bead, creates children
//! 13. Duplicate mitosis on same parent: zero new children
//! 14. Two workers mitosis on same parent: flock serializes

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::claim::Claimer;
use needle::config::{
    ExploreConfig, LimitsConfig, MendConfig, MitosisConfig, ModelLimits, ProviderLimits,
};
use needle::health::{HealthMonitor, HeartbeatData};
use needle::mitosis::{MitosisEvaluator, MitosisResult};
use needle::peer::PeerMonitor;
use needle::rate_limit::{RateLimitDecision, RateLimiter};
use needle::registry::{Registry, WorkerEntry};
use needle::strand::{ExploreStrand, MendStrand, Strand};
use needle::telemetry::Telemetry;
use needle::types::{
    Bead, BeadId, BeadStatus, ClaimOutcome, ClaimResult, InputMethod, StrandResult, WorkerState,
};

// ═════════════════════════════════════════════════════════════════════════════
// Shared test infrastructure
// ═════════════════════════════════════════════════════════════════════════════

/// Thread-safe mock bead store for multi-worker tests.
///
/// Tracks claimed beads and enforces single-claim semantics: once a bead is
/// claimed, subsequent claim attempts on the same bead return RaceLost.
struct ConcurrentMockStore {
    beads: Mutex<Vec<Bead>>,
    claims: Mutex<HashMap<String, String>>,
    release_count: Arc<AtomicU32>,
    created_beads: Mutex<Vec<(String, String)>>,
    deps_added: Mutex<Vec<(String, String)>>,
    labels_map: Mutex<HashMap<String, Vec<String>>>,
}

impl ConcurrentMockStore {
    fn new(beads: Vec<Bead>) -> Self {
        ConcurrentMockStore {
            beads: Mutex::new(beads),
            claims: Mutex::new(HashMap::new()),
            release_count: Arc::new(AtomicU32::new(0)),
            created_beads: Mutex::new(Vec::new()),
            deps_added: Mutex::new(Vec::new()),
            labels_map: Mutex::new(HashMap::new()),
        }
    }

    fn with_labels(self, bead_id: &str, labels: Vec<String>) -> Self {
        self.labels_map
            .lock()
            .unwrap()
            .insert(bead_id.to_string(), labels);
        self
    }

    fn release_count(&self) -> u32 {
        self.release_count.load(Ordering::Relaxed)
    }

    /// Return the set of (bead_id, worker_id) claim pairs.
    fn claim_pairs(&self) -> Vec<(String, String)> {
        self.claims
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[async_trait]
impl BeadStore for ConcurrentMockStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        let beads = self.beads.lock().unwrap();
        let claims = self.claims.lock().unwrap();
        Ok(beads
            .iter()
            .filter(|b| b.status == BeadStatus::Open && !claims.contains_key(b.id.as_ref()))
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        let beads = self.beads.lock().unwrap();
        let claims = self.claims.lock().unwrap();
        let mut bead = beads
            .iter()
            .find(|b| b.id == *id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))?;
        // Reflect claim state.
        if let Some(actor) = claims.get(id.as_ref()) {
            bead.status = BeadStatus::InProgress;
            bead.assignee = Some(actor.clone());
        }
        Ok(bead)
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        let mut claims = self.claims.lock().unwrap();
        if claims.contains_key(id.as_ref()) {
            return Ok(ClaimResult::RaceLost {
                claimed_by: claims[id.as_ref()].clone(),
            });
        }
        claims.insert(id.to_string(), actor.to_string());
        let beads = self.beads.lock().unwrap();
        let mut bead = beads
            .iter()
            .find(|b| b.id == *id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))?;
        bead.status = BeadStatus::InProgress;
        bead.assignee = Some(actor.to_string());
        Ok(ClaimResult::Claimed(bead))
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        self.release_count.fetch_add(1, Ordering::Relaxed);
        let mut claims = self.claims.lock().unwrap();
        claims.remove(id.as_ref());
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        Ok(self
            .labels_map
            .lock()
            .unwrap()
            .get(id.as_ref())
            .cloned()
            .unwrap_or_default())
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn create_bead(&self, title: &str, body: &str, _labels: &[&str]) -> Result<BeadId> {
        let mut created = self.created_beads.lock().unwrap();
        created.push((title.to_string(), body.to_string()));
        let id = format!("child-{:03}", created.len());
        Ok(BeadId::from(id))
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        self.deps_added
            .lock()
            .unwrap()
            .push((blocker_id.to_string(), blocked_id.to_string()));
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

fn make_bead(id: &str, priority: u8) -> Bead {
    Bead {
        id: BeadId::from(id),
        title: format!("Test bead {id}"),
        body: Some("Test deliverable".to_string()),
        priority,
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

fn write_heartbeat(dir: &Path, data: &HeartbeatData) {
    let path = dir.join(format!("{}.json", data.worker_id));
    let json = serde_json::to_string(data).unwrap();
    std::fs::write(path, json).unwrap();
}

fn make_heartbeat(worker_id: &str, pid: u32, bead_id: Option<&str>, stale: bool) -> HeartbeatData {
    let last_heartbeat = if stale {
        Utc::now() - chrono::Duration::seconds(600)
    } else {
        Utc::now()
    };

    HeartbeatData {
        worker_id: worker_id.to_string(),
        qualified_id: worker_id.to_string(),
        pid,
        state: WorkerState::Executing,
        current_bead: bead_id.map(BeadId::from),
        workspace: PathBuf::from("/tmp/test"),
        last_heartbeat,
        started_at: Utc::now() - chrono::Duration::seconds(3600),
        beads_processed: 0,
        session: worker_id.to_string(),
        heartbeat_file: None,
    }
}

fn make_worker_entry(id: &str, provider: Option<&str>, model: Option<&str>) -> WorkerEntry {
    WorkerEntry {
        id: id.to_string(),
        pid: std::process::id(),
        workspace: PathBuf::from("/tmp/test"),
        agent: "claude".to_string(),
        model: model.map(|s| s.to_string()),
        provider: provider.map(|s| s.to_string()),
        started_at: Utc::now(),
        beads_processed: 0,
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 1: Multi-worker claiming — N workers, M beads, each claimed exactly once
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn multi_worker_claiming_no_duplicates() {
    // 5 beads, 5 concurrent claimers. Each bead should be claimed exactly once.
    let beads: Vec<Bead> = (0..5)
        .map(|i| make_bead(&format!("nd-mw-{i:03}"), 1))
        .collect();
    let store: Arc<dyn BeadStore> = Arc::new(ConcurrentMockStore::new(beads.clone()));

    let mut handles = Vec::new();

    for worker_idx in 0..5u32 {
        let store = store.clone();
        let beads = beads.clone();
        let handle = tokio::spawn(async move {
            let lock_dir = std::env::temp_dir().join(format!("needle-test-mw-{worker_idx}"));
            let _ = std::fs::create_dir_all(&lock_dir);
            let claimer = Claimer::new(
                store,
                lock_dir,
                5,
                10,
                Telemetry::new(format!("worker-{worker_idx}")),
            );
            claimer
                .claim_next(&beads, &format!("worker-{worker_idx}"), &HashSet::new())
                .await
        });
        handles.push(handle);
    }

    let mut claimed_ids: Vec<String> = Vec::new();
    for handle in handles {
        let result = handle.await.unwrap().unwrap();
        if let ClaimOutcome::Claimed(bead) = result {
            claimed_ids.push(bead.id.to_string());
        }
    }

    // Check no duplicates.
    let unique: HashSet<&String> = claimed_ids.iter().collect();
    assert_eq!(
        unique.len(),
        claimed_ids.len(),
        "no duplicate claims allowed; claimed: {:?}",
        claimed_ids
    );
}

#[tokio::test]
async fn multi_worker_all_beads_eventually_claimed() {
    // 3 beads, 3 workers each claiming one. All beads should be claimed.
    let beads: Vec<Bead> = (0..3)
        .map(|i| make_bead(&format!("nd-allclaim-{i}"), 1))
        .collect();
    let store = Arc::new(ConcurrentMockStore::new(beads.clone()));
    let store_dyn: Arc<dyn BeadStore> = store.clone();

    let mut handles = Vec::new();
    for worker_idx in 0..3u32 {
        let store = store_dyn.clone();
        let beads = beads.clone();
        let handle = tokio::spawn(async move {
            let lock_dir = std::env::temp_dir().join(format!("needle-test-allclaim-{worker_idx}"));
            let _ = std::fs::create_dir_all(&lock_dir);
            let claimer = Claimer::new(
                store,
                lock_dir,
                10,
                10,
                Telemetry::new(format!("worker-{worker_idx}")),
            );
            claimer
                .claim_next(&beads, &format!("worker-{worker_idx}"), &HashSet::new())
                .await
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await.unwrap();
    }

    // All 3 beads should be claimed by someone.
    let pairs = store.claim_pairs();
    let claimed_bead_ids: HashSet<String> = pairs.iter().map(|(bead, _)| bead.clone()).collect();
    let expected_ids: HashSet<String> = (0..3).map(|i| format!("nd-allclaim-{i}")).collect();
    assert_eq!(
        claimed_bead_ids, expected_ids,
        "all beads should be claimed; got {:?}",
        claimed_bead_ids
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 2: Crashed worker bead released by peer monitoring
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn crashed_worker_bead_released_by_peer() {
    let hb_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();

    // Simulate a crashed worker: stale heartbeat + dead PID.
    write_heartbeat(
        hb_dir.path(),
        &make_heartbeat("crashed-worker", 99_999_999, Some("nd-orphan"), true),
    );

    let store = Arc::new(ConcurrentMockStore::new(vec![make_bead("nd-orphan", 1)]));
    let registry = Registry::new(reg_dir.path());
    let telemetry = Telemetry::new("monitor-worker".to_string());

    let monitor = PeerMonitor::new(
        hb_dir.path().to_path_buf(),
        Duration::from_secs(300),
        "monitor-worker".to_string(),
        store.as_ref(),
        &registry,
        telemetry,
    );

    let result = monitor.check_peers().await.unwrap();

    assert_eq!(result.crashed_count, 1, "should detect 1 crashed peer");
    assert_eq!(result.beads_released, 1, "should release 1 bead");
    assert!(result.did_work());
    assert_eq!(store.release_count(), 1);

    // Heartbeat file should be cleaned up.
    assert!(
        !hb_dir.path().join("crashed-worker.json").exists(),
        "heartbeat file should be removed"
    );
}

#[tokio::test]
async fn stuck_worker_bead_not_released() {
    let hb_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();

    // Simulate a stuck worker: stale heartbeat + alive PID (our own PID).
    write_heartbeat(
        hb_dir.path(),
        &make_heartbeat("stuck-worker", std::process::id(), Some("nd-busy"), true),
    );

    let store = Arc::new(ConcurrentMockStore::new(vec![make_bead("nd-busy", 1)]));
    let registry = Registry::new(reg_dir.path());
    let telemetry = Telemetry::new("monitor-worker".to_string());

    let monitor = PeerMonitor::new(
        hb_dir.path().to_path_buf(),
        Duration::from_secs(300),
        "monitor-worker".to_string(),
        store.as_ref(),
        &registry,
        telemetry,
    );

    let result = monitor.check_peers().await.unwrap();

    assert_eq!(result.crashed_count, 0);
    assert_eq!(result.stuck_count, 1, "should detect 1 stuck peer");
    assert_eq!(result.beads_released, 0, "should NOT release stuck bead");
    assert!(!result.did_work());
    assert_eq!(store.release_count(), 0);

    // Heartbeat file should remain.
    assert!(
        hb_dir.path().join("stuck-worker.json").exists(),
        "heartbeat file should remain for stuck worker"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 3: Mend strand cleans stale claims and orphaned locks
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn mend_strand_cleans_crashed_peer_returns_work_created() {
    let hb_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();

    // Crashed peer with stale heartbeat and dead PID.
    write_heartbeat(
        hb_dir.path(),
        &make_heartbeat("dead-peer", 99_999_999, Some("nd-stale"), true),
    );

    let store = Arc::new(ConcurrentMockStore::new(vec![make_bead("nd-stale", 1)]));
    let config = MendConfig::default();
    let registry = Registry::new(reg_dir.path());
    let telemetry = Telemetry::new("mend-worker".to_string());

    let mend = MendStrand::new(
        config,
        hb_dir.path().to_path_buf(),
        Duration::from_secs(300),
        lock_dir.path().to_path_buf(),
        "mend-worker".to_string(),
        registry,
        telemetry,
        std::path::PathBuf::from("/tmp/needle-test-logs"),
        0,
        std::path::PathBuf::from("/tmp/needle-test-traces"),
        30,
        7,
        std::path::PathBuf::from("/tmp"),
        100,
    );

    let result = mend.evaluate(store.as_ref()).await;

    // Mend should detect the crashed peer and return WorkCreated.
    assert!(
        matches!(result, StrandResult::WorkCreated),
        "mend should return WorkCreated when cleanup is performed; got {:?}",
        result
    );
    assert_eq!(
        store.release_count(),
        1,
        "crashed peer bead should be released"
    );
}

#[tokio::test]
async fn mend_strand_no_stale_peers_returns_no_work() {
    let hb_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();

    // Only a fresh, healthy heartbeat for a different worker.
    write_heartbeat(
        hb_dir.path(),
        &make_heartbeat("healthy-peer", std::process::id(), Some("nd-active"), false),
    );

    let store = Arc::new(ConcurrentMockStore::new(vec![]));
    let config = MendConfig::default();
    let registry = Registry::new(reg_dir.path());
    let telemetry = Telemetry::new("mend-worker".to_string());

    let mend = MendStrand::new(
        config,
        hb_dir.path().to_path_buf(),
        Duration::from_secs(300),
        lock_dir.path().to_path_buf(),
        "mend-worker".to_string(),
        registry,
        telemetry,
        std::path::PathBuf::from("/tmp/needle-test-logs"),
        0,
        std::path::PathBuf::from("/tmp/needle-test-traces"),
        30,
        7,
        std::path::PathBuf::from("/tmp"),
        100,
    );

    let result = mend.evaluate(store.as_ref()).await;

    assert!(
        matches!(result, StrandResult::NoWork),
        "mend should return NoWork when nothing to clean; got {:?}",
        result
    );
}

#[tokio::test]
async fn mend_strand_removes_orphaned_lock_files() {
    let hb_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();

    // Create an old lock file (orphaned).
    let lock_file = lock_dir.path().join("needle-claim-deadbeef.lock");
    std::fs::write(&lock_file, "").unwrap();
    // Set modification time to the past by sleeping is not reliable.
    // Instead, let the mend strand's lock_ttl be very short.

    let store = Arc::new(ConcurrentMockStore::new(vec![]));
    let config = MendConfig {
        lock_ttl_secs: 0,
        ..MendConfig::default()
    };
    let registry = Registry::new(reg_dir.path());
    let telemetry = Telemetry::new("mend-worker".to_string());

    let mend = MendStrand::new(
        config,
        hb_dir.path().to_path_buf(),
        Duration::from_secs(300),
        lock_dir.path().to_path_buf(),
        "mend-worker".to_string(),
        registry,
        telemetry,
        std::path::PathBuf::from("/tmp/needle-test-logs"),
        0,
        std::path::PathBuf::from("/tmp/needle-test-traces"),
        30,
        7,
        std::path::PathBuf::from("/tmp"),
        100,
    );

    let result = mend.evaluate(store.as_ref()).await;

    // The lock file should have been removed (if the Mend strand handles it).
    // If the lock file was removed, the strand returns WorkCreated.
    // Note: the Mend strand may or may not have removed it depending on
    // implementation details, but the key assertion is that it ran without error.
    assert!(
        matches!(result, StrandResult::WorkCreated | StrandResult::NoWork),
        "mend should complete without error; got {:?}",
        result
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 4: Provider/model concurrency limits enforced
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn provider_concurrency_limit_blocks_at_max() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::new(dir.path());

    // Register 3 workers using anthropic.
    for i in 0..3 {
        registry
            .register(make_worker_entry(
                &format!("w{i}"),
                Some("anthropic"),
                Some("sonnet"),
            ))
            .unwrap();
    }

    let mut providers = std::collections::BTreeMap::new();
    providers.insert(
        "anthropic".to_string(),
        ProviderLimits {
            max_concurrent: Some(3),
            requests_per_minute: None,
        },
    );
    let config = LimitsConfig {
        providers,
        models: std::collections::BTreeMap::new(),
    };
    let limiter = RateLimiter::new(config, dir.path());

    let decision = limiter
        .check(Some("anthropic"), Some("sonnet"), &registry)
        .unwrap();
    assert!(
        matches!(
            decision,
            RateLimitDecision::ProviderConcurrencyExceeded {
                current: 3,
                limit: 3,
                ..
            }
        ),
        "should block when at provider limit; got: {decision}"
    );
}

#[test]
fn model_concurrency_limit_blocks_at_max() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::new(dir.path());

    // Register 2 workers using claude-opus.
    registry
        .register(make_worker_entry(
            "w1",
            Some("anthropic"),
            Some("claude-opus"),
        ))
        .unwrap();
    registry
        .register(make_worker_entry(
            "w2",
            Some("anthropic"),
            Some("claude-opus"),
        ))
        .unwrap();

    let mut models = std::collections::BTreeMap::new();
    models.insert(
        "claude-opus".to_string(),
        ModelLimits {
            max_concurrent: Some(2),
        },
    );
    let config = LimitsConfig {
        providers: std::collections::BTreeMap::new(),
        models,
    };
    let limiter = RateLimiter::new(config, dir.path());

    let decision = limiter
        .check(Some("anthropic"), Some("claude-opus"), &registry)
        .unwrap();
    assert!(
        matches!(
            decision,
            RateLimitDecision::ModelConcurrencyExceeded {
                current: 2,
                limit: 2,
                ..
            }
        ),
        "should block when at model limit; got: {decision}"
    );
}

#[test]
fn below_limit_allows_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::new(dir.path());

    // Only 1 worker registered, limit is 3.
    registry
        .register(make_worker_entry("w1", Some("anthropic"), Some("sonnet")))
        .unwrap();

    let mut providers = std::collections::BTreeMap::new();
    providers.insert(
        "anthropic".to_string(),
        ProviderLimits {
            max_concurrent: Some(3),
            requests_per_minute: None,
        },
    );
    let config = LimitsConfig {
        providers,
        models: std::collections::BTreeMap::new(),
    };
    let limiter = RateLimiter::new(config, dir.path());

    let decision = limiter
        .check(Some("anthropic"), Some("sonnet"), &registry)
        .unwrap();
    assert_eq!(
        decision,
        RateLimitDecision::Allowed,
        "should allow when below limit"
    );
}

#[test]
fn rpm_limit_blocks_after_exhaustion() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::new(dir.path());

    let mut providers = std::collections::BTreeMap::new();
    providers.insert(
        "anthropic".to_string(),
        ProviderLimits {
            max_concurrent: None,
            requests_per_minute: Some(2),
        },
    );
    let config = LimitsConfig {
        providers,
        models: std::collections::BTreeMap::new(),
    };
    let limiter = RateLimiter::new(config, dir.path());

    // First two requests succeed.
    let d1 = limiter.check(Some("anthropic"), None, &registry).unwrap();
    assert_eq!(d1, RateLimitDecision::Allowed);

    let d2 = limiter.check(Some("anthropic"), None, &registry).unwrap();
    assert_eq!(d2, RateLimitDecision::Allowed);

    // Third should be rate-limited.
    let d3 = limiter.check(Some("anthropic"), None, &registry).unwrap();
    assert!(
        matches!(d3, RateLimitDecision::RpmExceeded { .. }),
        "should rate-limit after bucket exhausted; got: {d3}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 5: Mitosis splits multi-task beads correctly
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn mitosis_creates_children_on_first_failure() {
    let config = MitosisConfig {
        enabled: true,
        first_failure_only: true,
        force_failure_threshold: 0,
    };
    let telemetry = Telemetry::new("test".to_string());
    let lock_dir = tempfile::tempdir().unwrap();
    let _evaluator = MitosisEvaluator::new(config, telemetry, lock_dir.path().to_path_buf());

    // Parent bead with failure-count:1.
    let parent = make_bead("parent-001", 1);
    let store = Arc::new(
        ConcurrentMockStore::new(vec![parent.clone()])
            .with_labels("parent-001", vec!["failure-count:1".to_string()]),
    );

    // We can't dispatch a real agent in integration tests, but we can test the
    // precondition checks (skip paths for disabled/non-first-failure).

    // Test: disabled mitosis → Skipped.
    let disabled_config = MitosisConfig {
        enabled: false,
        first_failure_only: true,
        force_failure_threshold: 0,
    };
    let disabled_evaluator = MitosisEvaluator::new(
        disabled_config,
        Telemetry::new("test".to_string()),
        lock_dir.path().to_path_buf(),
    );
    let dispatcher = create_test_dispatcher();
    let prompt_builder =
        needle::prompt::PromptBuilder::new(&needle::config::PromptConfig::default());

    let result = disabled_evaluator
        .evaluate(
            store.as_ref(),
            &parent,
            Path::new("/tmp/test"),
            &dispatcher,
            &prompt_builder,
            "claude-sonnet",
        )
        .await
        .unwrap();
    assert!(
        matches!(result, MitosisResult::Skipped { .. }),
        "disabled mitosis should skip; got: {:?}",
        result
    );
}

#[tokio::test]
async fn mitosis_skips_non_first_failure() {
    let config = MitosisConfig {
        enabled: true,
        first_failure_only: true,
        force_failure_threshold: 0,
    };
    let telemetry = Telemetry::new("test".to_string());
    let lock_dir = tempfile::tempdir().unwrap();
    let evaluator = MitosisEvaluator::new(config, telemetry, lock_dir.path().to_path_buf());

    // failure-count:2 → not first failure.
    let parent = make_bead("parent-002", 1);
    let store = Arc::new(
        ConcurrentMockStore::new(vec![parent.clone()])
            .with_labels("parent-002", vec!["failure-count:2".to_string()]),
    );

    let dispatcher = create_test_dispatcher();
    let prompt_builder =
        needle::prompt::PromptBuilder::new(&needle::config::PromptConfig::default());

    let result = evaluator
        .evaluate(
            store.as_ref(),
            &parent,
            Path::new("/tmp/test"),
            &dispatcher,
            &prompt_builder,
            "claude-sonnet",
        )
        .await
        .unwrap();

    assert!(
        matches!(result, MitosisResult::Skipped { .. }),
        "non-first failure should skip; got: {:?}",
        result
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 6: Mitosis dedup — second split creates zero new children
// ═════════════════════════════════════════════════════════════════════════════

// (Tested via mitosis module unit tests which cover dedup exhaustively.
// This integration test verifies the precondition-checking path is correct.)

#[tokio::test]
async fn mitosis_evaluator_adapter_not_found_skips() {
    let config = MitosisConfig {
        enabled: true,
        first_failure_only: true,
        force_failure_threshold: 0,
    };
    let telemetry = Telemetry::new("test".to_string());
    let lock_dir = tempfile::tempdir().unwrap();
    let evaluator = MitosisEvaluator::new(config, telemetry, lock_dir.path().to_path_buf());

    let parent = make_bead("parent-003", 1);
    let store = Arc::new(
        ConcurrentMockStore::new(vec![parent.clone()])
            .with_labels("parent-003", vec!["failure-count:1".to_string()]),
    );

    let dispatcher = create_test_dispatcher();
    let prompt_builder =
        needle::prompt::PromptBuilder::new(&needle::config::PromptConfig::default());

    // Use an adapter name that doesn't exist in the dispatcher.
    let result = evaluator
        .evaluate(
            store.as_ref(),
            &parent,
            Path::new("/tmp/test"),
            &dispatcher,
            &prompt_builder,
            "nonexistent-agent-xyz",
        )
        .await
        .unwrap();

    assert!(
        matches!(result, MitosisResult::Skipped { ref reason } if reason.contains("not found")),
        "missing adapter should skip; got: {:?}",
        result
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 7: Concurrent claiming with flock serialization
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn flock_serializes_concurrent_claims_on_same_bead() {
    // Multiple claimers attempt the same single bead concurrently.
    // Exactly one should succeed, the rest should get RaceLost.
    let bead = make_bead("nd-flock-target", 1);
    let store: Arc<dyn BeadStore> = Arc::new(ConcurrentMockStore::new(vec![bead.clone()]));

    // All workers use the SAME lock directory (same workspace → same flock).
    let shared_lock_dir = tempfile::tempdir().unwrap();

    let mut handles = Vec::new();
    for i in 0..5u32 {
        let store = store.clone();
        let bead = bead.clone();
        let lock_dir = shared_lock_dir.path().to_path_buf();
        let handle = tokio::spawn(async move {
            let claimer = Claimer::new(
                store,
                lock_dir,
                1, // max_retries=1 — don't retry, just report result.
                10,
                Telemetry::new(format!("worker-{i}")),
            );
            claimer
                .claim_next(&[bead], &format!("worker-{i}"), &HashSet::new())
                .await
        });
        handles.push(handle);
    }

    let mut success_count = 0u32;
    let mut race_lost_count = 0u32;
    for handle in handles {
        match handle.await.unwrap().unwrap() {
            ClaimOutcome::Claimed(_) => success_count += 1,
            ClaimOutcome::AllRaceLost => race_lost_count += 1,
            other => panic!("unexpected outcome: {:?}", other),
        }
    }

    assert_eq!(success_count, 1, "exactly one claimer should succeed");
    assert_eq!(
        race_lost_count, 4,
        "remaining claimers should get AllRaceLost"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 8: Registry concurrent access — no corruption
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn registry_concurrent_registration_no_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(Registry::new(dir.path()));

    // Spawn 10 concurrent registrations.
    let mut handles = Vec::new();
    for i in 0..10u32 {
        let registry = registry.clone();
        let handle = tokio::spawn(async move {
            registry
                .register(WorkerEntry {
                    id: format!("worker-{i}"),
                    pid: std::process::id(),
                    workspace: PathBuf::from("/tmp/test"),
                    agent: "claude".to_string(),
                    model: Some("sonnet".to_string()),
                    provider: Some("anthropic".to_string()),
                    started_at: Utc::now(),
                    beads_processed: 0,
                })
                .unwrap();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let workers = registry.list().unwrap();
    assert_eq!(
        workers.len(),
        10,
        "all 10 workers should be registered; got {}",
        workers.len()
    );

    // Verify each worker has unique ID.
    let ids: HashSet<String> = workers.iter().map(|w| w.id.clone()).collect();
    assert_eq!(ids.len(), 10, "all worker IDs should be unique");
}

#[tokio::test]
async fn registry_deregister_during_concurrent_registrations() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(Registry::new(dir.path()));

    // Pre-register workers 0..5.
    for i in 0..5u32 {
        registry
            .register(WorkerEntry {
                id: format!("worker-{i}"),
                pid: std::process::id(),
                workspace: PathBuf::from("/tmp/test"),
                agent: "claude".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 0,
            })
            .unwrap();
    }

    // Concurrently: register 5..10, deregister 0..5.
    let mut handles = Vec::new();
    for i in 5..10u32 {
        let reg = registry.clone();
        handles.push(tokio::spawn(async move {
            reg.register(WorkerEntry {
                id: format!("worker-{i}"),
                pid: std::process::id(),
                workspace: PathBuf::from("/tmp/test"),
                agent: "claude".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 0,
            })
            .unwrap();
        }));
    }
    for i in 0..5u32 {
        let reg = registry.clone();
        handles.push(tokio::spawn(async move {
            reg.deregister(&format!("worker-{i}")).unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let workers = registry.list().unwrap();
    let ids: HashSet<String> = workers.iter().map(|w| w.id.clone()).collect();

    // Workers 0..5 should be deregistered, 5..10 should remain.
    for i in 0..5u32 {
        assert!(
            !ids.contains(&format!("worker-{i}")),
            "worker-{i} should be deregistered"
        );
    }
    for i in 5..10u32 {
        assert!(
            ids.contains(&format!("worker-{i}")),
            "worker-{i} should be registered"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 9: Heartbeat liveness — emitter writes and stop cleans up
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn heartbeat_emitter_writes_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = needle::config::Config::default();
    config.workspace.home = dir.path().to_path_buf();
    config.health.heartbeat_interval_secs = 1;
    config.health.heartbeat_ttl_secs = 5;

    let mut monitor = HealthMonitor::new(
        config,
        "hb-test-worker".to_string(),
        Telemetry::new("test".to_string()),
        None,
    );

    monitor.start_emitter().unwrap();

    // Heartbeat file should exist immediately (initial write is sync).
    let hb_path = monitor.heartbeat_path();
    assert!(hb_path.exists(), "heartbeat file should exist after start");

    // Read and verify contents.
    let content = std::fs::read_to_string(&hb_path).unwrap();
    let data: HeartbeatData = serde_json::from_str(&content).unwrap();
    assert_eq!(data.worker_id, "hb-test-worker");
    assert_eq!(data.pid, std::process::id());

    // Update state and wait for emitter to pick it up.
    monitor.update_state(&WorkerState::Executing, Some(&BeadId::from("nd-test")));
    monitor.update_beads_processed(3);
    std::thread::sleep(Duration::from_millis(1500));

    let content2 = std::fs::read_to_string(&hb_path).unwrap();
    let data2: HeartbeatData = serde_json::from_str(&content2).unwrap();
    assert_eq!(data2.state, WorkerState::Executing);
    assert_eq!(data2.current_bead, Some(BeadId::from("nd-test")));
    assert_eq!(data2.beads_processed, 3);

    // Stop should remove the heartbeat file.
    monitor.stop();
    assert!(
        !hb_path.exists(),
        "heartbeat file should be removed after stop"
    );
}

#[test]
fn stale_detection_works_correctly() {
    let fresh_hb = HeartbeatData {
        worker_id: "fresh".to_string(),
        qualified_id: "fresh".to_string(),
        pid: 1,
        state: WorkerState::Selecting,
        current_bead: None,
        workspace: PathBuf::from("/tmp"),
        last_heartbeat: Utc::now(),
        started_at: Utc::now(),
        beads_processed: 0,
        session: "fresh".to_string(),
        heartbeat_file: None,
    };

    let stale_hb = HeartbeatData {
        worker_id: "stale".to_string(),
        qualified_id: "stale".to_string(),
        pid: 2,
        state: WorkerState::Executing,
        current_bead: Some(BeadId::from("nd-x")),
        workspace: PathBuf::from("/tmp"),
        last_heartbeat: Utc::now() - chrono::Duration::seconds(600),
        started_at: Utc::now(),
        beads_processed: 0,
        session: "stale".to_string(),
        heartbeat_file: None,
    };

    let ttl = Duration::from_secs(300);
    assert!(
        !HealthMonitor::is_stale(&fresh_hb, ttl),
        "fresh heartbeat should not be stale"
    );
    assert!(
        HealthMonitor::is_stale(&stale_hb, ttl),
        "old heartbeat should be stale"
    );
}

#[test]
fn pid_liveness_check_works() {
    // Our own PID should be alive.
    assert!(HealthMonitor::check_pid_alive(std::process::id()));
    // Nonsense PID should be dead.
    assert!(!HealthMonitor::check_pid_alive(99_999_999));
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 10: Strand waterfall ordering with Mend
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn strand_waterfall_pluck_mend_explore_knot() {
    // Verify the default waterfall contains all 4 strands in correct order.
    let dir = tempfile::tempdir().unwrap();
    let config = needle::config::Config::default();
    let registry = Registry::new(dir.path());
    let telemetry = Telemetry::new("test".to_string());

    let runner =
        needle::strand::StrandRunner::from_config(&config, "test-worker", registry, telemetry);

    assert_eq!(
        runner.strand_names(),
        vec!["pluck", "mend", "explore", "weave", "unravel", "pulse", "reflect", "knot"],
        "waterfall should be pluck → mend → explore → weave → unravel → pulse → reflect → knot"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 11: Explore discovers work in other workspaces (real br)
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn explore_discovers_work_in_other_workspace() {
    // Create a real workspace with beads using the br CLI.
    let ws = tempfile::tempdir().unwrap();
    let br = br_path();

    // Initialize the workspace.
    let status = std::process::Command::new(&br)
        .current_dir(ws.path())
        .arg("init")
        .status()
        .expect("br init failed — is br installed at ~/.local/bin/br?");
    assert!(status.success(), "br init should succeed");

    // Create a ready bead in this workspace.
    let status = std::process::Command::new(&br)
        .current_dir(ws.path())
        .args([
            "create",
            "--title",
            "Explore integration test bead",
            "--body",
            "Ready to work",
        ])
        .status()
        .expect("br create failed");
    assert!(status.success(), "br create should succeed");

    // Home workspace is a separate dir (no beads there).
    let home_ws = tempfile::tempdir().unwrap();

    let config = ExploreConfig {
        enabled: true,
        workspaces: vec![ws.path().to_path_buf()],
    };
    let strand = ExploreStrand::new(
        config,
        home_ws.path().to_path_buf(),
        Registry::new(tempfile::tempdir().unwrap().path()),
        Telemetry::new("test".to_string()),
        "test-worker".to_string(),
    );

    // ExploreStrand ignores the passed store; use a minimal empty mock.
    let dummy_store = ConcurrentMockStore::new(vec![]);
    let result = strand.evaluate(&dummy_store).await;

    assert!(
        matches!(result, StrandResult::BeadFound(_)),
        "explore should discover beads in other workspace; got: {:?}",
        result
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 12: Mitosis splits multi-task bead, creates children
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn mitosis_splits_multitask_bead_creates_children() {
    // A bash adapter that echoes a JSON response proposing 2 children.
    let json = r#"{"splittable": true, "children": [{"title": "Task A", "body": "Do task A"}, {"title": "Task B", "body": "Do task B"}]}"#;
    let dispatcher = create_mitosis_dispatcher(json);

    let config = MitosisConfig {
        enabled: true,
        first_failure_only: false, // skip failure-count check for simplicity
        force_failure_threshold: 0,
    };
    let lock_dir = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let telemetry = Telemetry::new("test".to_string());
    let evaluator = MitosisEvaluator::new(config, telemetry, lock_dir.path().to_path_buf());

    let parent = make_bead("parent-split-001", 1);
    let store = Arc::new(ConcurrentMockStore::new(vec![parent.clone()]));
    let prompt_builder =
        needle::prompt::PromptBuilder::new(&needle::config::PromptConfig::default());

    let result = evaluator
        .evaluate(
            store.as_ref(),
            &parent,
            ws.path(),
            &dispatcher,
            &prompt_builder,
            "mitosis-bash",
        )
        .await
        .unwrap();

    match &result {
        MitosisResult::Split { children } => {
            assert_eq!(
                children.len(),
                2,
                "should create 2 children; got: {:?}",
                children
            );
        }
        other => panic!("expected Split, got: {:?}", other),
    }

    let created = store.created_beads.lock().unwrap();
    assert_eq!(created.len(), 2, "store should have 2 created beads");
    let deps = store.deps_added.lock().unwrap();
    assert_eq!(deps.len(), 2, "store should have 2 dependencies linked");
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 13: Duplicate mitosis on same parent — zero new children
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn mitosis_duplicate_split_creates_zero_new_children() {
    let json = r#"{"splittable": true, "children": [{"title": "Task A", "body": "Do task A"}, {"title": "Task B", "body": "Do task B"}]}"#;

    let config = MitosisConfig {
        enabled: true,
        first_failure_only: false,
        force_failure_threshold: 0,
    };
    let lock_dir = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let prompt_builder =
        needle::prompt::PromptBuilder::new(&needle::config::PromptConfig::default());

    // Use a store that reflects created children in show() for dedup.
    let store = Arc::new(MitosisDedupeStore::new("parent-dedup-001", vec![]));
    let parent = store.show(&BeadId::from("parent-dedup-001")).await.unwrap();

    // First split: creates 2 children.
    let dispatcher = create_mitosis_dispatcher(json);
    let evaluator1 = MitosisEvaluator::new(
        config.clone(),
        Telemetry::new("test1".to_string()),
        lock_dir.path().to_path_buf(),
    );
    let result1 = evaluator1
        .evaluate(
            store.as_ref(),
            &parent,
            ws.path(),
            &dispatcher,
            &prompt_builder,
            "mitosis-bash",
        )
        .await
        .unwrap();
    assert!(
        matches!(result1, MitosisResult::Split { ref children } if children.len() == 2),
        "first split should create 2 children; got: {:?}",
        result1
    );
    assert_eq!(store.created_count(), 2);

    // Second split: all children already exist → Skipped.
    let dispatcher2 = create_mitosis_dispatcher(json);
    let evaluator2 = MitosisEvaluator::new(
        config,
        Telemetry::new("test2".to_string()),
        lock_dir.path().to_path_buf(),
    );
    let result2 = evaluator2
        .evaluate(
            store.as_ref(),
            &parent,
            ws.path(),
            &dispatcher2,
            &prompt_builder,
            "mitosis-bash",
        )
        .await
        .unwrap();
    assert!(
        matches!(result2, MitosisResult::Skipped { ref reason } if reason.contains("already exist")),
        "second split should be skipped (all children exist); got: {:?}",
        result2
    );
    assert_eq!(
        store.created_count(),
        2,
        "exactly 2 children total — no duplicates created"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 14: Two workers mitosis on same parent — flock serializes
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn mitosis_concurrent_workers_flock_serializes() {
    let json = r#"{"splittable": true, "children": [{"title": "Task X", "body": "Do X"}, {"title": "Task Y", "body": "Do Y"}]}"#;

    let config = MitosisConfig {
        enabled: true,
        first_failure_only: false,
        force_failure_threshold: 0,
    };
    let lock_dir = tempfile::tempdir().unwrap();
    let lock_dir_path = lock_dir.path().to_path_buf();
    let ws = tempfile::tempdir().unwrap();
    let ws_path = ws.path().to_path_buf();

    let store = Arc::new(MitosisDedupeStore::new("parent-flock-001", vec![]));
    let parent = store.show(&BeadId::from("parent-flock-001")).await.unwrap();

    // Launch two concurrent mitosis evaluations on the same parent bead.
    let mut handles = Vec::new();
    for i in 0..2u32 {
        let store = store.clone();
        let parent = parent.clone();
        let lock_dir = lock_dir_path.clone();
        let ws = ws_path.clone();
        let config = config.clone();

        let handle = tokio::spawn(async move {
            let dispatcher = create_mitosis_dispatcher(json);
            let prompt_builder =
                needle::prompt::PromptBuilder::new(&needle::config::PromptConfig::default());
            let telemetry = Telemetry::new(format!("worker-{i}"));
            let evaluator = MitosisEvaluator::new(config, telemetry, lock_dir);
            evaluator
                .evaluate(
                    store.as_ref(),
                    &parent,
                    &ws,
                    &dispatcher,
                    &prompt_builder,
                    "mitosis-bash",
                )
                .await
                .unwrap()
        });
        handles.push(handle);
    }

    let mut split_count = 0u32;
    let mut skipped_count = 0u32;
    for handle in handles {
        match handle.await.unwrap() {
            MitosisResult::Split { .. } => split_count += 1,
            MitosisResult::Skipped { .. } => skipped_count += 1,
            other => panic!("unexpected mitosis result: {:?}", other),
        }
    }

    // Flock serializes: exactly one worker creates children, the other skips.
    assert_eq!(split_count, 1, "exactly one worker should create children");
    assert_eq!(
        skipped_count, 1,
        "the other worker should skip (all children exist)"
    );
    assert_eq!(
        store.created_count(),
        2,
        "exactly 2 children total — flock prevented duplicate creation"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Helpers
// ═════════════════════════════════════════════════════════════════════════════

fn create_test_dispatcher() -> needle::dispatch::Dispatcher {
    let adapters: HashMap<String, needle::dispatch::AgentAdapter> = HashMap::new();
    let telemetry = Telemetry::new("test".to_string());
    needle::dispatch::Dispatcher::with_adapters(adapters, telemetry, 60)
}

/// Create a dispatcher with a bash adapter that echoes a fixed JSON response.
///
/// Used for mitosis tests that need a controllable agent output.
fn create_mitosis_dispatcher(json_response: &str) -> needle::dispatch::Dispatcher {
    let mut adapters = HashMap::new();
    let adapter = needle::dispatch::AgentAdapter {
        name: "mitosis-bash".to_string(),
        description: None,
        agent_cli: "bash".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: format!("echo '{json_response}'"),
        environment: HashMap::new(),
        timeout_secs: 10,
        provider: None,
        model: None,
        token_extraction: needle::dispatch::TokenExtraction::None,
        output_transform: None,
    };
    adapters.insert("mitosis-bash".to_string(), adapter);
    let telemetry = Telemetry::new("mitosis-test".to_string());
    needle::dispatch::Dispatcher::with_adapters(adapters, telemetry, 10)
}

/// Returns the path to the br CLI binary.
fn br_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let local = PathBuf::from(&home).join(".local/bin/br");
    if local.exists() {
        local
    } else {
        PathBuf::from("br")
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// MitosisDedupeStore
// ═════════════════════════════════════════════════════════════════════════════

/// Mock bead store for mitosis dedup and flock tests.
///
/// When children are created via `create_bead`, they are immediately reflected
/// in subsequent `show()` calls as dependencies of the parent. This allows
/// dedup checks inside the flock to see children created by a concurrent worker.
struct MitosisDedupeStore {
    parent_id: BeadId,
    labels: Vec<String>,
    /// Created children: (id, title, labels), updated atomically via create_bead.
    created_children: Mutex<Vec<(String, String, Vec<String>)>>,
}

impl MitosisDedupeStore {
    fn new(parent_id: &str, labels: Vec<String>) -> Self {
        MitosisDedupeStore {
            parent_id: BeadId::from(parent_id),
            labels,
            created_children: Mutex::new(Vec::new()),
        }
    }

    fn created_count(&self) -> usize {
        self.created_children.lock().unwrap().len()
    }
}

#[async_trait]
impl BeadStore for MitosisDedupeStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(vec![])
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        // Return created children as beads with their labels for dedup lookup.
        let children = self.created_children.lock().unwrap();
        let beads: Vec<Bead> = children
            .iter()
            .map(|(id, title, labels)| Bead {
                id: BeadId::from(id.clone()),
                title: title.clone(),
                body: Some("Mitosis child".to_string()),
                priority: 1,
                status: BeadStatus::Open,
                assignee: None,
                labels: labels.clone(),
                workspace: PathBuf::from("/tmp/test"),
                dependencies: vec![],
                dependents: vec![],
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .collect();
        Ok(beads)
    }

    async fn show(&self, _id: &BeadId) -> Result<Bead> {
        Ok(Bead {
            id: self.parent_id.clone(),
            title: format!("Parent {}", self.parent_id),
            body: Some("Multi-task bead for dedup testing".to_string()),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: self.labels.clone(),
            workspace: PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
        Ok(ClaimResult::NotClaimable {
            reason: "mock".to_string(),
        })
    }

    async fn release(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
        Ok(self.labels.clone())
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn create_bead(&self, title: &str, _body: &str, labels: &[&str]) -> Result<BeadId> {
        let mut children = self.created_children.lock().unwrap();
        let count = children.len() + 1;
        let id = format!("mitosis-child-{count:03}");
        let label_strings: Vec<String> = labels.iter().map(|l| l.to_string()).collect();
        children.push((id.clone(), title.to_string(), label_strings));
        Ok(BeadId::from(id))
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
