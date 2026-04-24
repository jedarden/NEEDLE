//! Peer monitoring — stale claim release and crash cleanup.
//!
//! Detects crashed or stuck workers by reading heartbeat files and comparing
//! them against the configured TTL. For crashed workers (stale heartbeat +
//! dead PID), releases the claimed bead, removes the heartbeat file, and
//! deregisters from the worker registry.
//!
//! This module is designed to be called by the Mend strand (Strand 2). No
//! other strand should perform peer monitoring.
//!
//! Depends on: `bead_store`, `health`, `registry`, `telemetry`, `types`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::bead_store::BeadStore;
use crate::health::{HealthMonitor, StalePeer};
use crate::registry::Registry;
use crate::telemetry::{EventKind, Telemetry};

// ──────────────────────────────────────────────────────────────────────────────
// PeerCheckResult
// ──────────────────────────────────────────────────────────────────────────────

/// Summary of a peer monitoring check cycle.
#[derive(Debug)]
pub struct PeerCheckResult {
    /// Number of crashed workers detected (dead PID).
    pub crashed_count: u32,
    /// Number of stuck workers detected (alive PID, stale heartbeat).
    pub stuck_count: u32,
    /// Number of beads released from crashed workers.
    pub beads_released: u32,
}

impl PeerCheckResult {
    /// Whether any cleanup work was performed.
    pub fn did_work(&self) -> bool {
        self.beads_released > 0
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// PeerMonitor
// ──────────────────────────────────────────────────────────────────────────────

/// Monitors peer workers for crashes and stale heartbeats.
///
/// Constructed with references to shared infrastructure (bead store, registry,
/// telemetry). Call `check_peers()` to run one monitoring cycle.
pub struct PeerMonitor<'a> {
    heartbeat_dir: PathBuf,
    heartbeat_ttl: Duration,
    own_worker_id: String,
    store: &'a dyn BeadStore,
    registry: &'a Registry,
    telemetry: Telemetry,
}

impl<'a> PeerMonitor<'a> {
    /// Create a new peer monitor.
    ///
    /// - `heartbeat_dir`: path to `~/.needle/state/heartbeats/`
    /// - `heartbeat_ttl`: how long before a heartbeat is considered stale
    /// - `own_worker_id`: this worker's ID (excluded from peer checks)
    /// - `store`: bead store for releasing orphaned beads
    /// - `registry`: worker registry for deregistering crashed workers
    /// - `telemetry`: telemetry emitter for peer events
    pub fn new(
        heartbeat_dir: PathBuf,
        heartbeat_ttl: Duration,
        own_worker_id: String,
        store: &'a dyn BeadStore,
        registry: &'a Registry,
        telemetry: Telemetry,
    ) -> Self {
        PeerMonitor {
            heartbeat_dir,
            heartbeat_ttl,
            own_worker_id,
            store,
            registry,
            telemetry,
        }
    }

    /// Run one peer monitoring cycle.
    ///
    /// Algorithm:
    /// 1. Read all heartbeat files
    /// 2. For each stale peer (excluding ourselves):
    ///    a. If PID is dead → CRASHED: release bead, remove heartbeat, deregister
    ///    b. If PID is alive → STUCK: emit warning only, do NOT release
    /// 3. Return summary of actions taken
    pub async fn check_peers(&self) -> Result<PeerCheckResult> {
        let heartbeats = HealthMonitor::read_all_heartbeats(&self.heartbeat_dir)?;

        let mut result = PeerCheckResult {
            crashed_count: 0,
            stuck_count: 0,
            beads_released: 0,
        };

        for hb in &heartbeats {
            // Skip our own heartbeat (match by qualified identity).
            if hb.qualified_id == self.own_worker_id {
                continue;
            }

            // Only check stale heartbeats.
            if !HealthMonitor::is_stale(hb, self.heartbeat_ttl) {
                continue;
            }

            let pid_alive = HealthMonitor::check_pid_alive(hb.pid);
            let stale_peer = StalePeer {
                worker_id: hb.worker_id.clone(),
                qualified_id: Some(hb.qualified_id.clone()),
                pid: hb.pid,
                pid_alive,
                current_bead: hb.current_bead.clone(),
                last_heartbeat: hb.last_heartbeat,
                heartbeat_file: hb.heartbeat_file.clone().unwrap_or_else(|| {
                    self.heartbeat_dir.join(format!("{}.json", hb.qualified_id))
                }),
            };

            if pid_alive {
                // STUCK: alive PID, stale heartbeat. Warn only — do NOT release.
                self.handle_stuck_peer(&stale_peer)?;
                result.stuck_count += 1;
            } else {
                // CRASHED: dead PID. Clean up.
                let released = self.handle_crashed_peer(&stale_peer).await?;
                result.crashed_count += 1;
                if released {
                    result.beads_released += 1;
                }
            }
        }

        if result.crashed_count > 0 || result.stuck_count > 0 {
            tracing::info!(
                crashed = result.crashed_count,
                stuck = result.stuck_count,
                beads_released = result.beads_released,
                "peer monitoring cycle complete"
            );
        }

        Ok(result)
    }

    /// Handle a stuck peer: alive PID but stale heartbeat.
    ///
    /// Safety: never release the bead, never kill the process. Alert only.
    fn handle_stuck_peer(&self, peer: &StalePeer) -> Result<()> {
        let age_secs = Utc::now()
            .signed_duration_since(peer.last_heartbeat)
            .num_seconds()
            .max(0) as u64;

        tracing::warn!(
            worker = %peer.worker_id,
            pid = peer.pid,
            age_secs,
            bead = ?peer.current_bead,
            "peer is stuck (alive PID, stale heartbeat) — not releasing"
        );

        // Emit peer.stale telemetry for each stuck peer.
        if let Some(ref bead_id) = peer.current_bead {
            self.telemetry.emit(EventKind::StuckDetected {
                bead_id: bead_id.clone(),
                age_secs,
            })?;
        }

        Ok(())
    }

    /// Handle a crashed peer: dead PID, stale heartbeat.
    ///
    /// 1. Release the claimed bead (if any)
    /// 2. Remove the heartbeat file
    /// 3. Deregister from the worker registry
    /// 4. Emit peer.crashed telemetry
    ///
    /// Returns `true` if a bead was released.
    async fn handle_crashed_peer(&self, peer: &StalePeer) -> Result<bool> {
        tracing::info!(
            worker = %peer.worker_id,
            pid = peer.pid,
            bead = ?peer.current_bead,
            "peer crashed — cleaning up"
        );

        let mut bead_released = false;

        // 1. Release the claimed bead (if any).
        if let Some(ref bead_id) = peer.current_bead {
            match self.store.release(bead_id).await {
                Ok(()) => {
                    tracing::info!(
                        bead = %bead_id,
                        worker = %peer.worker_id,
                        "released orphaned bead from crashed worker"
                    );
                    bead_released = true;
                }
                Err(e) => {
                    // Release failure is non-fatal — the bead may have already
                    // been released or closed by another path.
                    tracing::warn!(
                        bead = %bead_id,
                        worker = %peer.worker_id,
                        error = %e,
                        "failed to release bead from crashed worker (may already be released)"
                    );
                }
            }

            // Emit peer.crashed telemetry.
            self.telemetry.emit(EventKind::StuckReleased {
                bead_id: bead_id.clone(),
                peer_worker: peer
                    .qualified_id
                    .as_deref()
                    .unwrap_or(&peer.worker_id)
                    .to_string(),
            })?;
        }

        // 2. Remove the heartbeat file.
        remove_heartbeat_file(&peer.heartbeat_file)?;

        // 3. Deregister from the worker registry.
        let dereg_id = peer.qualified_id.as_deref().unwrap_or(&peer.worker_id);
        if let Err(e) = self.registry.deregister(dereg_id) {
            tracing::warn!(
                worker = %peer.worker_id,
                error = %e,
                "failed to deregister crashed worker from registry"
            );
        }

        Ok(bead_released)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Remove a heartbeat file (best-effort).
fn remove_heartbeat_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::debug!(path = %path.display(), "removed stale heartbeat file");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Already gone — not an error.
            tracing::debug!(path = %path.display(), "heartbeat file already removed");
            Ok(())
        }
        Err(e) => {
            Err(e).with_context(|| format!("failed to remove heartbeat file: {}", path.display()))
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{Filters, RepairReport};
    use crate::health::HeartbeatData;
    use crate::types::{Bead, BeadId, ClaimResult, WorkerState};
    use async_trait::async_trait;
    use chrono::Utc;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    // ── Mock bead store ─────────────────────────────────────────────────────

    /// Track which beads were released.
    struct MockBeadStore {
        release_count: Arc<AtomicU32>,
    }

    impl MockBeadStore {
        fn new() -> (Self, Arc<AtomicU32>) {
            let count = Arc::new(AtomicU32::new(0));
            (
                MockBeadStore {
                    release_count: count.clone(),
                },
                count,
            )
        }
    }

    #[async_trait]
    impl BeadStore for MockBeadStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("not implemented in mock")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented in mock")
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
            self.release_count.fetch_add(1, Ordering::Relaxed);
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
        async fn doctor_repair(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    fn write_heartbeat(dir: &Path, data: &HeartbeatData) {
        let name = if data.qualified_id.is_empty() {
            &data.worker_id
        } else {
            &data.qualified_id
        };
        let path = dir.join(format!("{}.json", name));
        let json = serde_json::to_string(data).unwrap();
        std::fs::write(path, json).unwrap();
    }

    fn make_heartbeat(
        worker_id: &str,
        pid: u32,
        bead_id: Option<&str>,
        stale: bool,
    ) -> HeartbeatData {
        let last_heartbeat = if stale {
            // 10 minutes ago — well past any reasonable TTL
            Utc::now() - chrono::Duration::seconds(600)
        } else {
            Utc::now()
        };

        HeartbeatData {
            worker_id: worker_id.to_string(),
            qualified_id: format!("claude-{}", worker_id),
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

    #[tokio::test]
    async fn healthy_peers_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path();
        let reg_dir = tempfile::tempdir().unwrap();

        // Write a fresh (non-stale) heartbeat for another worker.
        write_heartbeat(
            hb_dir,
            &make_heartbeat("other-worker", std::process::id(), Some("nd-abc"), false),
        );

        let (store, release_count) = MockBeadStore::new();
        let registry = Registry::new(reg_dir.path());
        let telemetry = Telemetry::new("test-monitor".to_string());

        let monitor = PeerMonitor::new(
            hb_dir.to_path_buf(),
            Duration::from_secs(300),
            "my-worker".to_string(),
            &store,
            &registry,
            telemetry,
        );

        let result = monitor.check_peers().await.unwrap();
        assert_eq!(result.crashed_count, 0);
        assert_eq!(result.stuck_count, 0);
        assert_eq!(result.beads_released, 0);
        assert!(!result.did_work());
        assert_eq!(release_count.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn crashed_peer_bead_released() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path();
        let reg_dir = tempfile::tempdir().unwrap();

        // Write a stale heartbeat with a dead PID (99999999).
        write_heartbeat(
            hb_dir,
            &make_heartbeat("dead-worker", 99_999_999, Some("nd-orphan"), true),
        );

        let (store, release_count) = MockBeadStore::new();
        let registry = Registry::new(reg_dir.path());
        let telemetry = Telemetry::new("test-monitor".to_string());

        let monitor = PeerMonitor::new(
            hb_dir.to_path_buf(),
            Duration::from_secs(300),
            "my-worker".to_string(),
            &store,
            &registry,
            telemetry,
        );

        let result = monitor.check_peers().await.unwrap();
        assert_eq!(result.crashed_count, 1);
        assert_eq!(result.stuck_count, 0);
        assert_eq!(result.beads_released, 1);
        assert!(result.did_work());
        assert_eq!(release_count.load(Ordering::Relaxed), 1);

        // Heartbeat file should be removed.
        let hb_path = hb_dir.join("claude-dead-worker.json");
        assert!(!hb_path.exists(), "heartbeat file should be removed");
    }

    #[tokio::test]
    async fn stuck_peer_not_released() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path();
        let reg_dir = tempfile::tempdir().unwrap();

        // Write a stale heartbeat with our own PID (alive).
        // Use a different worker_id so it's not skipped as "self".
        write_heartbeat(
            hb_dir,
            &make_heartbeat("stuck-worker", std::process::id(), Some("nd-busy"), true),
        );

        let (store, release_count) = MockBeadStore::new();
        let registry = Registry::new(reg_dir.path());
        let telemetry = Telemetry::new("test-monitor".to_string());

        let monitor = PeerMonitor::new(
            hb_dir.to_path_buf(),
            Duration::from_secs(300),
            "my-worker".to_string(),
            &store,
            &registry,
            telemetry,
        );

        let result = monitor.check_peers().await.unwrap();
        assert_eq!(result.crashed_count, 0);
        assert_eq!(result.stuck_count, 1);
        assert_eq!(result.beads_released, 0);
        assert!(!result.did_work());
        assert_eq!(release_count.load(Ordering::Relaxed), 0);

        // Heartbeat file should NOT be removed for stuck workers.
        let hb_path = hb_dir.join("claude-stuck-worker.json");
        assert!(
            hb_path.exists(),
            "heartbeat file should remain for stuck worker"
        );
    }

    #[tokio::test]
    async fn own_heartbeat_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path();
        let reg_dir = tempfile::tempdir().unwrap();

        // Write a stale heartbeat for ourselves (should be skipped).
        write_heartbeat(
            hb_dir,
            &make_heartbeat("my-worker", std::process::id(), Some("nd-mine"), true),
        );

        let (store, release_count) = MockBeadStore::new();
        let registry = Registry::new(reg_dir.path());
        let telemetry = Telemetry::new("test-monitor".to_string());

        let monitor = PeerMonitor::new(
            hb_dir.to_path_buf(),
            Duration::from_secs(300),
            "claude-my-worker".to_string(),
            &store,
            &registry,
            telemetry,
        );

        let result = monitor.check_peers().await.unwrap();
        assert_eq!(result.crashed_count, 0);
        assert_eq!(result.stuck_count, 0);
        assert_eq!(result.beads_released, 0);
        assert_eq!(release_count.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn crashed_peer_no_bead_still_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path();
        let reg_dir = tempfile::tempdir().unwrap();

        // Crashed worker with no active bead.
        write_heartbeat(hb_dir, &make_heartbeat("dead-idle", 99_999_999, None, true));

        // Register the worker so we can verify deregistration.
        let registry = Registry::new(reg_dir.path());
        registry
            .register(crate::registry::WorkerEntry {
                id: "claude-dead-idle".to_string(),
                pid: 99_999_999,
                workspace: PathBuf::from("/tmp"),
                agent: "test".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 0,
            })
            .unwrap();

        let (store, release_count) = MockBeadStore::new();
        let telemetry = Telemetry::new("test-monitor".to_string());

        let monitor = PeerMonitor::new(
            hb_dir.to_path_buf(),
            Duration::from_secs(300),
            "my-worker".to_string(),
            &store,
            &registry,
            telemetry,
        );

        let result = monitor.check_peers().await.unwrap();
        assert_eq!(result.crashed_count, 1);
        assert_eq!(result.beads_released, 0); // no bead to release
        assert_eq!(release_count.load(Ordering::Relaxed), 0);

        // Heartbeat file should be removed.
        assert!(!hb_dir.join("claude-dead-idle.json").exists());

        // Worker should be deregistered.
        let workers = registry.list().unwrap();
        assert!(workers.is_empty(), "crashed worker should be deregistered");
    }

    #[tokio::test]
    async fn multiple_peers_mixed_states() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path();
        let reg_dir = tempfile::tempdir().unwrap();

        // Healthy peer (fresh heartbeat).
        write_heartbeat(
            hb_dir,
            &make_heartbeat("healthy", std::process::id(), Some("nd-1"), false),
        );
        // Stuck peer (stale heartbeat, alive PID).
        write_heartbeat(
            hb_dir,
            &make_heartbeat("stuck", std::process::id(), Some("nd-2"), true),
        );
        // Crashed peer (stale heartbeat, dead PID).
        write_heartbeat(
            hb_dir,
            &make_heartbeat("crashed", 99_999_999, Some("nd-3"), true),
        );
        // Our own heartbeat (should be skipped even if stale).
        write_heartbeat(
            hb_dir,
            &make_heartbeat("my-worker", std::process::id(), Some("nd-4"), true),
        );

        let (store, release_count) = MockBeadStore::new();
        let registry = Registry::new(reg_dir.path());
        let telemetry = Telemetry::new("test-monitor".to_string());

        let monitor = PeerMonitor::new(
            hb_dir.to_path_buf(),
            Duration::from_secs(300),
            "claude-my-worker".to_string(),
            &store,
            &registry,
            telemetry,
        );

        let result = monitor.check_peers().await.unwrap();
        assert_eq!(result.crashed_count, 1);
        assert_eq!(result.stuck_count, 1);
        assert_eq!(result.beads_released, 1);
        assert!(result.did_work());
        assert_eq!(release_count.load(Ordering::Relaxed), 1);

        // Only the crashed peer's heartbeat should be removed.
        assert!(hb_dir.join("claude-healthy.json").exists());
        assert!(hb_dir.join("claude-stuck.json").exists());
        assert!(!hb_dir.join("claude-crashed.json").exists());
        assert!(hb_dir.join("claude-my-worker.json").exists());
    }

    #[tokio::test]
    async fn empty_heartbeat_dir_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path();
        let reg_dir = tempfile::tempdir().unwrap();

        let (store, _) = MockBeadStore::new();
        let registry = Registry::new(reg_dir.path());
        let telemetry = Telemetry::new("test-monitor".to_string());

        let monitor = PeerMonitor::new(
            hb_dir.to_path_buf(),
            Duration::from_secs(300),
            "my-worker".to_string(),
            &store,
            &registry,
            telemetry,
        );

        let result = monitor.check_peers().await.unwrap();
        assert_eq!(result.crashed_count, 0);
        assert_eq!(result.stuck_count, 0);
        assert_eq!(result.beads_released, 0);
    }

    #[tokio::test]
    async fn nonexistent_heartbeat_dir_returns_zero() {
        let reg_dir = tempfile::tempdir().unwrap();
        let (store, _) = MockBeadStore::new();
        let registry = Registry::new(reg_dir.path());
        let telemetry = Telemetry::new("test-monitor".to_string());

        let monitor = PeerMonitor::new(
            PathBuf::from("/nonexistent/heartbeats"),
            Duration::from_secs(300),
            "my-worker".to_string(),
            &store,
            &registry,
            telemetry,
        );

        let result = monitor.check_peers().await.unwrap();
        assert_eq!(result.crashed_count, 0);
        assert_eq!(result.stuck_count, 0);
        assert_eq!(result.beads_released, 0);
    }

    #[test]
    fn remove_heartbeat_file_nonexistent_is_ok() {
        let result = remove_heartbeat_file(Path::new("/nonexistent/file.json"));
        assert!(result.is_ok());
    }

    #[test]
    fn remove_heartbeat_file_removes_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.json");
        std::fs::write(&path, "{}").unwrap();
        assert!(path.exists());

        remove_heartbeat_file(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn peer_check_result_did_work() {
        let no_work = PeerCheckResult {
            crashed_count: 0,
            stuck_count: 1, // stuck doesn't count as "work"
            beads_released: 0,
        };
        assert!(!no_work.did_work());

        let did_work = PeerCheckResult {
            crashed_count: 1,
            stuck_count: 0,
            beads_released: 1,
        };
        assert!(did_work.did_work());
    }
}
