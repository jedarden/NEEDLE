//! Mend strand: maintenance and self-healing.
//!
//! Strand 2 in the waterfall. Runs after Pluck finds no work. Cleans up
//! stale claims, orphaned locks, broken dependency links, and database
//! corruption. If any cleanup is performed, returns `WorkCreated` so the
//! waterfall restarts from Pluck (released beads may now be claimable).
//!
//! Depends on: `bead_store`, `config`, `health`, `peer`, `registry`,
//!             `telemetry`, `types`, `trace`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use chrono::Utc;

use crate::bead_store::BeadStore;
use crate::config::{LimitsConfig, MendConfig};
use crate::health::HealthMonitor;
use crate::learning::LearningsFile;
use crate::peer::PeerMonitor;
use crate::registry::Registry;
use crate::telemetry::{EventKind, Telemetry};
use crate::trace::cleanup_traces;
use crate::types::{BeadStatus, StrandError, StrandResult};

// ──────────────────────────────────────────────────────────────────────────────
// Store-scoped orphan cleanup (shared between Mend and Explore)
// ──────────────────────────────────────────────────────────────────────────────

/// Scan all in-progress beads in a store and release any whose assignee does
/// not correspond to a live worker. This catches beads orphaned by workers
/// that died without leaving a heartbeat file (or whose heartbeat was already
/// cleaned up).
///
/// This is a store-scoped function that can be called against any BeadStore,
/// not just the home workspace. Used by both MendStrand (for home) and
/// ExploreStrand (for remote workspaces).
///
/// # Arguments
/// * `store` - The bead store to scan (can be home or remote)
/// * `registry` - Worker registry for live worker lookup
/// * `telemetry` - Telemetry emitter for orphan release events
/// * `qualified_id` - This worker's fully-qualified identity (excluded from orphan detection)
///
/// # Returns
/// * `Ok(u32)` - Number of orphans released
/// * `Err(anyhow::Error)` - Store read failure
pub async fn cleanup_orphaned_in_progress(
    store: &dyn BeadStore,
    registry: &Registry,
    telemetry: &Telemetry,
    qualified_id: &str,
) -> Result<u32> {
    let all_beads = store.list_all().await?;
    let workers = registry.list()?;

    // Build a set of fully-qualified worker IDs for registered, alive workers.
    let live_worker_ids: std::collections::HashSet<String> = workers
        .iter()
        .filter(|w| HealthMonitor::check_pid_alive(w.pid))
        .map(|w| w.id.clone())
        .collect();

    let mut released = 0u32;

    for bead in &all_beads {
        if bead.status != BeadStatus::InProgress {
            continue;
        }

        let assignee = match &bead.assignee {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };

        // Skip if the assignee matches our own qualified identity (we're running).
        if assignee == qualified_id {
            continue;
        }

        // Skip if the assignee matches a registered, alive worker.
        // Workers register with fully-qualified IDs ({adapter}-{worker_id}),
        // so this comparison prevents collisions when workers from different
        // adapter pools share a NATO name.
        if live_worker_ids.contains(assignee.as_str()) {
            continue;
        }

        // Orphaned: assignee is not a live registered worker. Release it.
        tracing::info!(
            bead_id = %bead.id,
            assignee = %assignee,
            workspace = %bead.workspace.display(),
            "releasing orphaned in-progress bead (assignee has no live worker)"
        );

        match store.release(&bead.id).await {
            Ok(()) => {
                let _ = telemetry.emit(EventKind::BeadReleased {
                    bead_id: bead.id.clone(),
                    reason: format!("orphaned: assignee {} has no live worker", assignee),
                });
                let _ = telemetry.emit(EventKind::StuckReleased {
                    bead_id: bead.id.clone(),
                    peer_worker: assignee.clone(),
                });
                released += 1;
            }
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "failed to release orphaned in-progress bead"
                );
                let _ = telemetry.emit(EventKind::MendBeadReleaseFailed {
                    bead_id: bead.id.to_string(),
                    assignee: assignee.clone(),
                    error: e.to_string(),
                });
            }
        }
    }

    Ok(released)
}

/// Summary of work performed during one Mend evaluation cycle.
#[derive(Debug, Default)]
struct MendSummary {
    beads_released: u32,
    locks_removed: u32,
    deps_cleaned: u32,
    db_repaired: bool,
    db_rebuilt: bool,
    agent_logs_cleaned: u32,
    zero_activity_logs_cleaned: u32,
    traces_pruned: u32,
    traces_cleaned: u32,
    learnings_pruned: u32,
    learnings_consolidated: u32,
    orphaned_heartbeats_removed: u32,
    workers_deregistered: u32,
    idle_workers_flagged: u32,
    rate_limits_cleaned: u32,
    rate_limit_providers_removed: u32,
    rate_limit_providers_reset: u32,
}

impl MendSummary {
    /// Whether mend performed work that changes bead store state.
    ///
    /// Only operations that add or release claimable beads should return true.
    /// A strand MUST return `WorkCreated` only when it inserts a claimable bead
    /// into the store that the waterfall will re-scan. No-op cleanup (pruning
    /// traces, removing stale locks, repairing DB) should return `NoWork`.
    ///
    /// Operations that return `WorkCreated`:
    /// - `beads_released > 0`: Orphaned beads were released back to Open status.
    /// - `deps_cleaned > 0`: Stale dependency links were removed (beads become claimable).
    ///
    /// Operations that return `NoWork` (maintenance, not work creation):
    /// - `locks_removed > 0`: Lock file cleanup (doesn't add beads to queue).
    /// - `db_repaired`: Doctor repair fixed index corruption (doesn't add beads).
    /// - `db_rebuilt`: Full rebuild from JSONL (doesn't add beads).
    /// - `traces_pruned`, `agent_logs_cleaned`, `learnings_*`: File cleanup.
    ///
    /// A `WorkCreated` return must be paired with a telemetry event identifying
    /// the created bead(s) so operators can see what the restart is chasing.
    fn did_work(&self) -> bool {
        // Bead release and dependency removal add claimable items to the queue.
        // Lock removal, DB repair, and DB rebuild are maintenance operations
        // that don't create new work and must not trigger a waterfall restart.
        self.beads_released > 0 || self.deps_cleaned > 0
    }
}

/// The Mend strand — maintenance and self-healing.
pub struct MendStrand {
    config: MendConfig,
    heartbeat_dir: PathBuf,
    heartbeat_ttl: Duration,
    lock_dir: PathBuf,
    /// Fully-qualified worker identity ({adapter}-{worker_id}).
    /// Used for heartbeat file lookups and registry comparisons to prevent
    /// collisions when workers from different adapter pools share a NATO name.
    qualified_id: String,
    registry: Registry,
    telemetry: Telemetry,
    log_dir: PathBuf,
    retention_days: u32,
    traces_dir: PathBuf,
    trace_retention_failed_days: u32,
    trace_retention_success_days: u32,
    workspace: PathBuf,
    max_learnings: usize,
    /// Base state directory (contains `rate_limits/` subdirectory).
    state_dir: PathBuf,
    /// Provider/model limits configuration for rate limiter cleanup.
    limits_config: LimitsConfig,
}

impl MendStrand {
    /// Create a new MendStrand.
    ///
    /// - `config`: mend strand configuration
    /// - `heartbeat_dir`: path to `~/.needle/state/heartbeats/`
    /// - `heartbeat_ttl`: how long before a heartbeat is considered stale
    /// - `lock_dir`: directory where claim lock files live (default: `/tmp`)
    /// - `qualified_id`: fully-qualified worker identity ({adapter}-{worker_id})
    /// - `registry`: worker state registry
    /// - `telemetry`: telemetry emitter
    /// - `log_dir`: directory where agent log files live
    /// - `retention_days`: number of days to retain agent log files (0 = disabled)
    /// - `traces_dir`: directory where trace files live (`.beads/traces`)
    /// - `trace_retention_failed_days`: retention days for failed bead traces
    /// - `trace_retention_success_days`: retention days for successful bead traces
    /// - `workspace`: workspace root path for learning consolidation
    /// - `max_learnings`: maximum number of learning entries before consolidation
    /// - `state_dir`: base state directory (contains `rate_limits/` subdirectory)
    /// - `limits_config`: provider/model limits configuration for rate limiter cleanup
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: MendConfig,
        heartbeat_dir: PathBuf,
        heartbeat_ttl: Duration,
        lock_dir: PathBuf,
        qualified_id: String,
        registry: Registry,
        telemetry: Telemetry,
        log_dir: PathBuf,
        retention_days: u32,
        traces_dir: PathBuf,
        trace_retention_failed_days: u32,
        trace_retention_success_days: u32,
        workspace: PathBuf,
        max_learnings: usize,
        state_dir: PathBuf,
        limits_config: LimitsConfig,
    ) -> Self {
        MendStrand {
            config,
            heartbeat_dir,
            heartbeat_ttl,
            lock_dir,
            qualified_id,
            registry,
            telemetry,
            log_dir,
            retention_days,
            traces_dir,
            trace_retention_failed_days,
            trace_retention_success_days,
            workspace,
            max_learnings,
            state_dir,
            limits_config,
        }
    }

    // ── Step 1: Stale claim cleanup via peer monitoring ──────────────────────

    /// Check for crashed workers and release their orphaned beads.
    async fn cleanup_stale_claims(
        &self,
        store: &dyn BeadStore,
        summary: &mut MendSummary,
    ) -> Result<()> {
        let peer_monitor = PeerMonitor::new(
            self.heartbeat_dir.clone(),
            self.heartbeat_ttl,
            self.qualified_id.clone(),
            store,
            &self.registry,
            self.telemetry.clone(),
        );

        let peer_result = peer_monitor.check_peers().await?;
        summary.beads_released += peer_result.beads_released;

        Ok(())
    }

    // ── Step 1.5: Orphaned in-progress bead recovery ────────────────────────

    /// Scan all in-progress beads and release any whose assignee does not
    /// correspond to a live worker. This catches beads orphaned by workers
    /// that died without leaving a heartbeat file (or whose heartbeat was
    /// already cleaned up).
    async fn cleanup_orphaned_in_progress(
        &self,
        store: &dyn BeadStore,
        summary: &mut MendSummary,
    ) -> Result<()> {
        let released = super::mend::cleanup_orphaned_in_progress(
            store,
            &self.registry,
            &self.telemetry,
            &self.qualified_id,
        )
        .await?;
        summary.beads_released += released;
        Ok(())
    }

    // ── Step 1.75: Orphaned heartbeat file removal ────────────────────────────

    /// Remove heartbeat files that have no matching entry in the worker registry.
    ///
    /// This can happen due to:
    /// - Manual deletion of registry entries
    /// - Registry corruption
    /// - Worker crash between registry write and heartbeat file creation
    ///
    /// Only removes heartbeat files older than heartbeat_ttl to avoid deleting
    /// recently orphaned files that might still be in use.
    fn cleanup_orphaned_heartbeats(&self, summary: &mut MendSummary) -> Result<()> {
        // Read all heartbeat files.
        let heartbeats = match HealthMonitor::read_all_heartbeats(&self.heartbeat_dir) {
            Ok(h) => h,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "mend: failed to read heartbeat directory for orphaned heartbeat cleanup"
                );
                return Ok(());
            }
        };

        // Get all registered worker IDs.
        let registered_ids = match self.registry.list() {
            Ok(workers) => {
                let mut ids = std::collections::HashSet::new();
                for w in workers {
                    ids.insert(w.id);
                }
                ids
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "mend: failed to read worker registry for orphaned heartbeat cleanup"
                );
                return Ok(());
            }
        };

        for hb in heartbeats {
            // Use qualified_id for comparison; fall back to worker_id for old heartbeats.
            let worker_key = if hb.qualified_id.is_empty() {
                &hb.worker_id
            } else {
                &hb.qualified_id
            };

            // Skip if this heartbeat has a matching registry entry.
            if registered_ids.contains(worker_key) {
                continue;
            }

            // Skip our own heartbeat (shouldn't happen, but be safe).
            if worker_key == &self.qualified_id {
                continue;
            }

            // Check if the heartbeat is stale (older than TTL).
            // Only remove stale heartbeats to avoid deleting recently orphaned files.
            if !HealthMonitor::is_stale(&hb, self.heartbeat_ttl) {
                continue;
            }

            // Heartbeat is orphaned and stale — remove it.
            let heartbeat_path = hb
                .heartbeat_file
                .clone()
                .unwrap_or_else(|| self.heartbeat_dir.join(format!("{}.json", worker_key)));

            let age_secs = match file_age(&heartbeat_path) {
                Some(age) => age.as_secs(),
                None => continue,
            };

            match std::fs::remove_file(&heartbeat_path) {
                Ok(()) => {
                    tracing::info!(
                        worker_id = %worker_key,
                        path = %heartbeat_path.display(),
                        age_secs,
                        "removed orphaned heartbeat file"
                    );

                    let _ = self
                        .telemetry
                        .emit(EventKind::MendOrphanedHeartbeatRemoved {
                            worker_id: worker_key.clone(),
                            age_secs,
                        });

                    summary.orphaned_heartbeats_removed += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        path = %heartbeat_path.display(),
                        error = %e,
                        "failed to remove orphaned heartbeat file"
                    );
                }
            }
        }

        Ok(())
    }

    // ── Step 2: Orphaned lock file removal ───────────────────────────────────

    /// Remove claim lock files that are not actively held by any process.
    ///
    /// This function immediately cleans up lock files whose holding process
    /// has died, without waiting for an age-based timeout. The flock(2) lock
    /// is automatically released by the kernel when the holder process dies,
    /// so try_acquire_flock() will succeed immediately and we can remove the
    /// stale metadata file.
    ///
    /// Lock files older than lock_ttl_secs are also cleaned up as a fallback
    /// to handle edge cases where the flock probe might fail.
    fn cleanup_orphaned_locks(&self, summary: &mut MendSummary) -> Result<()> {
        let lock_ttl = Duration::from_secs(self.config.lock_ttl_secs);

        let entries = match std::fs::read_dir(&self.lock_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                tracing::warn!(
                    dir = %self.lock_dir.display(),
                    error = %e,
                    "failed to read lock directory"
                );
                return Ok(());
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };

            // Only consider needle claim lock files.
            if !name.starts_with("needle-claim-") || !name.ends_with(".lock") {
                continue;
            }

            // Check file age for logging/fallback cleanup.
            let age = match file_age(&path) {
                Some(age) => age,
                None => continue,
            };

            // Try to acquire flock (non-blocking). If we can acquire it,
            // no one is holding it — safe to delete immediately.
            //
            // The kernel releases flocks when the holder process dies, so this
            // probe succeeds immediately for dead PIDs regardless of file age.
            match try_acquire_flock(&path) {
                Ok(Some(_file)) => {
                    // Lock acquired — no one holds it. Remove the file.
                    let age_secs = age.as_secs();
                    if let Err(e) = std::fs::remove_file(&path) {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to remove orphaned lock file"
                        );
                        let _ = self.telemetry.emit(EventKind::MendLockRemoveFailed {
                            lock_path: path.display().to_string(),
                            error: e.to_string(),
                        });
                        continue;
                    }

                    tracing::info!(
                        path = %path.display(),
                        age_secs,
                        "removed orphaned lock file"
                    );

                    let _ = self.telemetry.emit(EventKind::MendOrphanedLockRemoved {
                        lock_path: path.display().to_string(),
                        age_secs,
                    });

                    summary.locks_removed += 1;
                }
                Ok(None) => {
                    // Lock is actively held — skip.
                    // Only log if the file is old; otherwise it's just a normal active lock.
                    if age > lock_ttl {
                        tracing::debug!(
                            path = %path.display(),
                            age_secs = age.as_secs(),
                            "lock file is old but actively held, skipping"
                        );
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        path = %path.display(),
                        error = %e,
                        "failed to probe lock file"
                    );
                }
            }
        }

        Ok(())
    }

    // ── Step 3: Dependency link repair ───────────────────────────────────────

    /// Find open beads that have closed blockers and clean up the stale
    /// dependency links.
    ///
    /// br does not automatically resolve dependency links on bead closure,
    /// so a bead can remain blocked even after its blocker is closed.
    async fn cleanup_stale_dependencies(
        &self,
        store: &dyn BeadStore,
        summary: &mut MendSummary,
    ) -> Result<()> {
        let all_beads = store.list_all().await?;

        for bead in &all_beads {
            // Only check open beads that have dependencies.
            if bead.status != BeadStatus::Open || bead.dependencies.is_empty() {
                continue;
            }

            for dep in &bead.dependencies {
                // Only check "blocks" type dependencies where the blocker is closed.
                if dep.dependency_type != "blocks" {
                    continue;
                }

                if dep.status == "closed" {
                    tracing::info!(
                        bead_id = %bead.id,
                        blocker_id = %dep.id,
                        "removing stale dependency link (closed blocker on open bead)"
                    );

                    // Remove the stale dependency link.
                    match store.remove_dependency(&bead.id, &dep.id).await {
                        Ok(()) => {
                            // Emit telemetry for the removed dependency.
                            let _ = self.telemetry.emit(EventKind::MendDependencyRemoved {
                                bead_id: bead.id.clone(),
                                blocker_id: dep.id.clone(),
                            });
                            summary.deps_cleaned += 1;
                        }
                        Err(e) => {
                            tracing::warn!(
                                bead_id = %bead.id,
                                blocker_id = %dep.id,
                                error = %e,
                                "failed to remove stale dependency link"
                            );
                        }
                    }
                }
            }
        }

        Ok(())
    }

    // ── Step 3.5: Dead worker registry cleanup ───────────────────────────────

    /// Scan the worker registry and remove entries for dead PIDs.
    ///
    /// This proactive cleanup ensures that dead worker entries don't accumulate
    /// in the registry. The registry's list() method already filters dead PIDs,
    /// but that only happens when rate limit checks run. This step ensures
    /// cleanup happens every mend cycle.
    fn cleanup_dead_workers(&self, summary: &mut MendSummary) -> Result<()> {
        // Read the raw registry file to find entries that need cleanup.
        let raw_content = match std::fs::read_to_string(self.registry.path()) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "failed to read registry file");
                return Ok(());
            }
        };

        let raw_reg: crate::registry::RegistryFile = match serde_json::from_str(&raw_content) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "failed to parse registry file");
                return Ok(());
            }
        };

        for entry in &raw_reg.workers {
            // Skip our own entry.
            if entry.id == self.qualified_id {
                continue;
            }

            // Check if the PID is dead.
            if crate::registry::is_pid_alive(entry.pid) {
                continue;
            }

            // Dead worker found — deregister it.
            match self.registry.deregister(&entry.id) {
                Ok(()) => {
                    tracing::info!(
                        worker_id = %entry.id,
                        pid = entry.pid,
                        "removed dead worker from registry"
                    );

                    let _ = self.telemetry.emit(EventKind::MendWorkerDeregistered {
                        worker_id: entry.id.clone(),
                        pid: entry.pid,
                    });

                    summary.workers_deregistered += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        worker_id = %entry.id,
                        error = %e,
                        "failed to deregister dead worker"
                    );
                }
            }
        }

        Ok(())
    }

    // ── Step 4: Agent log file cleanup ──────────────────────────────────────

    /// Delete `.agent.jsonl` files in `log_dir` that are older than
    /// `retention_days` and do not belong to a currently-executing bead.
    ///
    /// Files are matched by the `.agent.jsonl` suffix. The bead ID is parsed
    /// from the stem (`<worker>-<bead>`): a file is considered active if the
    /// stem ends with `-<bead_id>` for any in-progress bead.
    async fn cleanup_old_agent_logs(
        &self,
        store: &dyn BeadStore,
        summary: &mut MendSummary,
    ) -> Result<()> {
        if self.retention_days == 0 {
            return Ok(());
        }

        let entries = match std::fs::read_dir(&self.log_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                tracing::warn!(
                    dir = %self.log_dir.display(),
                    error = %e,
                    "mend: failed to read log directory for agent log cleanup"
                );
                return Ok(());
            }
        };

        // Collect all .agent.jsonl paths first so we can bail early if none.
        let agent_log_paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.ends_with(".agent.jsonl"))
                    .unwrap_or(false)
            })
            .collect();

        if agent_log_paths.is_empty() {
            return Ok(());
        }

        let cutoff = Duration::from_secs(u64::from(self.retention_days) * 86400);

        // Build the set of currently in-progress bead IDs.
        let in_progress_ids: std::collections::HashSet<String> = store
            .list_all()
            .await?
            .into_iter()
            .filter(|b| b.status == BeadStatus::InProgress)
            .map(|b| b.id.as_ref().to_string())
            .collect();

        for path in &agent_log_paths {
            // Check file age.
            let age = match file_age(path) {
                Some(a) => a,
                None => continue,
            };

            if age <= cutoff {
                continue;
            }

            // Skip if this log belongs to an in-progress bead.
            let stem = match path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".agent.jsonl"))
            {
                Some(s) => s.to_string(),
                None => continue,
            };

            let is_active = in_progress_ids
                .iter()
                .any(|bead_id| stem == *bead_id || stem.ends_with(&format!("-{bead_id}")));

            if is_active {
                tracing::debug!(
                    path = %path.display(),
                    "skipping agent log for in-progress bead"
                );
                continue;
            }

            if let Err(e) = std::fs::remove_file(path) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "mend: failed to remove stale agent log"
                );
                continue;
            }

            tracing::info!(
                path = %path.display(),
                age_secs = age.as_secs(),
                "mend: removed stale agent log"
            );
            summary.agent_logs_cleaned += 1;
        }

        Ok(())
    }

    // ── Step 4.5: Trace retention cleanup ───────────────────────────────────────

    /// Clean up old traces based on retention policy.
    ///
    /// - Failed beads (non-zero exit): delete after trace_retention_failed_days
    /// - Successful beads (exit 0): prune data after trace_retention_success_days, keep metadata only
    fn cleanup_old_traces(&self, summary: &mut MendSummary) -> Result<()> {
        if !self.traces_dir.exists() {
            return Ok(());
        }

        match cleanup_traces(
            &self.traces_dir,
            self.trace_retention_failed_days,
            self.trace_retention_success_days,
        ) {
            Ok(cleanup_summary) => {
                summary.traces_pruned = cleanup_summary.traces_pruned;
                summary.traces_cleaned = cleanup_summary.traces_deleted;

                if cleanup_summary.traces_pruned > 0 || cleanup_summary.traces_deleted > 0 {
                    let _ = self.telemetry.emit(EventKind::MendTraceCleanup {
                        traces_pruned: cleanup_summary.traces_pruned,
                        traces_deleted: cleanup_summary.traces_deleted,
                    });

                    tracing::info!(
                        traces_pruned = cleanup_summary.traces_pruned,
                        traces_deleted = cleanup_summary.traces_deleted,
                        "mend: cleaned up old traces"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "mend: trace cleanup failed");
            }
        }

        Ok(())
    }

    // ── Step 4.75: Learning consolidation ───────────────────────────────────────

    /// Clean up and consolidate workspace learnings.
    ///
    /// 1. Prune stale entries (>90 days without reinforcement)
    /// 2. Consolidate if entries exceed max_learnings
    fn cleanup_learnings(&self, summary: &mut MendSummary) -> Result<()> {
        let mut learnings = match LearningsFile::load(&self.workspace) {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!(error = %e, "mend: no learnings file to clean up");
                return Ok(());
            }
        };

        let _original_count = learnings.entries().len();

        // Step 1: Prune stale entries
        match learnings.prune_stale() {
            Ok(pruned) => {
                if pruned > 0 {
                    tracing::info!(pruned, "mend: pruned stale learning entries");
                    summary.learnings_pruned = pruned as u32;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "mend: failed to prune stale learnings");
            }
        }

        // Step 2: Consolidate if over limit
        if learnings.entries().len() > self.max_learnings {
            match learnings.consolidate(self.max_learnings) {
                Ok(removed) => {
                    if removed > 0 {
                        tracing::info!(
                            removed,
                            max_count = self.max_learnings,
                            "mend: consolidated learning entries"
                        );
                        summary.learnings_consolidated = removed as u32;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "mend: failed to consolidate learnings");
                }
            }
        }

        if summary.learnings_pruned > 0 || summary.learnings_consolidated > 0 {
            let _ = self.telemetry.emit(EventKind::MendLearningCleanup {
                pruned: summary.learnings_pruned,
                consolidated: summary.learnings_consolidated,
            });
        }

        Ok(())
    }

    // ── Step 5: Database health check with auto-recovery ────────────────────

    // ── Step 4.8: Idle worker flagging ─────────────────────────────────────────

    /// Flag workers that have been registered longer than `idle_timeout` with
    /// zero beads processed. This helps identify workers that may have failed
    /// to start, are stuck in dispatch, or have agent adapter problems.
    ///
    /// This is a warning/telemetry-only operation — idle workers are NOT
    /// deregistered (they may be genuinely waiting for work).
    fn flag_idle_workers(&self, summary: &mut MendSummary) -> Result<()> {
        let workers = self.registry.list()?;

        for worker in workers {
            // Skip workers that have processed at least one bead.
            if worker.beads_processed > 0 {
                continue;
            }

            // Calculate worker age from started_at.
            let age = match Utc::now().signed_duration_since(worker.started_at).to_std() {
                Ok(d) => d,
                Err(_) => {
                    tracing::warn!(
                        worker_id = %worker.id,
                        "worker started_at is in the future, skipping idle check"
                    );
                    continue;
                }
            };

            let idle_timeout = Duration::from_secs(self.config.idle_timeout);

            if age > idle_timeout {
                let age_secs = age.as_secs();
                tracing::warn!(
                    worker_id = %worker.id,
                    pid = worker.pid,
                    age_secs,
                    idle_timeout_secs = self.config.idle_timeout,
                    "flagging idle worker (0 beads processed for longer than idle_timeout)"
                );

                let _ = self.telemetry.emit(EventKind::MendIdleWorkerFlagged {
                    worker_id: worker.id.clone(),
                    pid: worker.pid,
                    age_secs,
                });

                summary.idle_workers_flagged += 1;
            }
        }

        Ok(())
    }

    // ── Step 4.9: Rate limiter state cleanup ─────────────────────────────────────

    /// Clean up rate limiter state files.
    ///
    /// - Remove token bucket files for providers no longer in config
    /// - Reset token buckets with stale last_refill (> 1 hour old) to full capacity
    fn cleanup_rate_limit_state(&self, summary: &mut MendSummary) -> Result<()> {
        use crate::rate_limit::TokenBucket;

        let rate_limits_dir = self.state_dir.join("rate_limits");

        let entries = match std::fs::read_dir(&rate_limits_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                tracing::debug!(
                    dir = %rate_limits_dir.display(),
                    error = %e,
                    "mend: failed to read rate_limits directory for cleanup"
                );
                return Ok(());
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };

            // Only consider JSON files (token bucket files).
            if !name.ends_with(".json") {
                continue;
            }

            // Extract provider name from filename (e.g., "anthropic.json" -> "anthropic").
            let provider = match name.strip_suffix(".json") {
                Some(p) => p,
                None => continue,
            };

            // Check if provider exists in config.
            let provider_in_config = self.limits_config.providers.contains_key(provider);

            if !provider_in_config {
                // Provider no longer in config — remove the file.
                let age_secs = match file_age(&path) {
                    Some(age) => age.as_secs(),
                    None => continue,
                };

                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        tracing::info!(
                            provider,
                            path = %path.display(),
                            age_secs,
                            "removed rate limit state file for provider not in config"
                        );

                        let _ = self
                            .telemetry
                            .emit(EventKind::MendRateLimitProviderRemoved {
                                provider: provider.to_string(),
                            });

                        summary.rate_limit_providers_removed += 1;
                        summary.rate_limits_cleaned += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            provider,
                            path = %path.display(),
                            error = %e,
                            "failed to remove rate limit state file"
                        );
                    }
                }
                continue;
            }

            // Provider is in config — check if last_refill is stale (> 1 hour old).
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!(
                        provider,
                        path = %path.display(),
                        error = %e,
                        "failed to read rate limit state file"
                    );
                    continue;
                }
            };

            let mut bucket: TokenBucket = match serde_json::from_str(&content) {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!(
                        provider,
                        path = %path.display(),
                        error = %e,
                        "failed to parse rate limit state file"
                    );
                    continue;
                }
            };

            let last_refill_age = match Utc::now().signed_duration_since(bucket.last_refill).to_std() {
                Ok(d) => d,
                Err(_) => {
                    tracing::debug!(
                        provider,
                        "last_refill is in the future, skipping reset"
                    );
                    continue;
                }
            };

            const STALE_THRESHOLD: Duration = Duration::from_secs(3600); // 1 hour

            if last_refill_age > STALE_THRESHOLD {
                // Stale token bucket — reset to full capacity.
                let age_secs = last_refill_age.as_secs();
                bucket.tokens = bucket.capacity as f64;
                bucket.last_refill = Utc::now();

                match serde_json::to_string_pretty(&bucket) {
                    Ok(json) => {
                        if let Err(e) = std::fs::write(&path, &json) {
                            tracing::warn!(
                                provider,
                                path = %path.display(),
                                error = %e,
                                "failed to write reset rate limit state file"
                            );
                            continue;
                        }

                        tracing::info!(
                            provider,
                            age_secs,
                            capacity = bucket.capacity,
                            "reset stale rate limit state to full capacity"
                        );

                        let _ = self.telemetry.emit(EventKind::MendRateLimitProviderReset {
                            provider: provider.to_string(),
                            age_secs,
                        });

                        summary.rate_limit_providers_reset += 1;
                        summary.rate_limits_cleaned += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            provider,
                            error = %e,
                            "failed to serialize reset rate limit state"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Run `br doctor` to check database health. If issues are found,
    /// escalate through the recovery pipeline:
    ///
    /// 1. `br doctor` (check only) → if warnings found:
    /// 2. `br doctor --repair` → if fails:
    /// 3. Full rebuild (rm .beads/beads.db + br sync --import + verify)
    ///
    /// If the full rebuild also fails, the JSONL itself may be corrupt.
    async fn check_db_health(
        &self,
        store: &dyn BeadStore,
        summary: &mut MendSummary,
    ) -> Result<()> {
        // Step 1: Probe with `br doctor` (no repair).
        let check_result = store.doctor_check().await;

        let needs_repair = match check_result {
            Ok(report) => {
                // Doctor succeeded. If there are warnings, escalate to repair.
                !report.warnings.is_empty()
            }
            Err(e) => {
                let msg = format!("{e:#}");
                if crate::bead_store::is_corruption_error(&msg) {
                    tracing::warn!(error = %e, "br doctor detected corruption");
                    true
                } else {
                    // Non-corruption error (e.g., br not found). Propagate.
                    return Err(e);
                }
            }
        };

        if !needs_repair {
            return Ok(());
        }

        // Step 2: Try `br doctor --repair`.
        tracing::info!("database health check found issues, attempting repair");
        match store.doctor_repair().await {
            Ok(report) => {
                let warnings = report.warnings.len() as u32;
                let fixed = report.fixed.len() as u32;
                tracing::info!(warnings, fixed, "br doctor --repair completed");

                let _ = self
                    .telemetry
                    .emit(EventKind::MendDbRepaired { warnings, fixed });

                // Only mark as repaired if actual fixes were applied.
                // If warnings persist without fixes, returning WorkCreated would
                // cause an infinite restart loop without making progress.
                if fixed > 0 {
                    summary.db_repaired = true;
                    return Ok(());
                }

                // If doctor_repair succeeded but warnings persist without fixes,
                // escalate to full rebuild. This prevents repeated Mend cycles
                // where each evaluation calls doctor_repair which returns the
                // same unfixed warnings without making progress.
                if !report.warnings.is_empty() {
                    tracing::warn!(
                        warnings,
                        "doctor --repair succeeded but warnings persist without fixes, escalating to full rebuild"
                    );
                } else {
                    // No warnings and no fixes - DB is clean, no work needed.
                    return Ok(());
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "br doctor --repair failed, attempting full rebuild"
                );
            }
        }

        // Step 3: Full rebuild — rm db + br sync --import + verify.
        match store.full_rebuild().await {
            Ok(()) => {
                tracing::info!("database fully rebuilt from JSONL");

                let _ = self.telemetry.emit(EventKind::MendDbRebuilt);

                summary.db_rebuilt = true;
                Ok(())
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "full database rebuild failed — JSONL may be corrupt"
                );
                Err(e)
            }
        }
    }
}

#[async_trait::async_trait]
impl super::Strand for MendStrand {
    fn name(&self) -> &str {
        "mend"
    }

    async fn evaluate(&self, store: &dyn BeadStore) -> StrandResult {
        let mut summary = MendSummary::default();

        // Step 1: Stale claim cleanup via peer monitoring.
        if let Err(e) = self.cleanup_stale_claims(store, &mut summary).await {
            tracing::warn!(error = %e, "mend: stale claim cleanup failed");
            return StrandResult::Error(StrandError::StoreError(e));
        }

        // Step 1.5: Orphaned in-progress bead recovery (registry-based).
        if let Err(e) = self.cleanup_orphaned_in_progress(store, &mut summary).await {
            tracing::warn!(error = %e, "mend: orphaned in-progress cleanup failed");
            // Non-fatal — continue with remaining steps.
        }

        // Step 1.75: Orphaned heartbeat file removal.
        if let Err(e) = self.cleanup_orphaned_heartbeats(&mut summary) {
            tracing::warn!(error = %e, "mend: orphaned heartbeat cleanup failed");
            // Non-fatal — continue with remaining steps.
        }

        // Step 2: Orphaned lock file removal.
        if let Err(e) = self.cleanup_orphaned_locks(&mut summary) {
            tracing::warn!(error = %e, "mend: orphaned lock cleanup failed");
            return StrandResult::Error(StrandError::StoreError(e));
        }

        // Step 3: Dependency link repair.
        if let Err(e) = self.cleanup_stale_dependencies(store, &mut summary).await {
            tracing::warn!(error = %e, "mend: dependency cleanup failed");
            return StrandResult::Error(StrandError::StoreError(e));
        }

        // Step 3.5: Dead worker registry cleanup.
        if let Err(e) = self.cleanup_dead_workers(&mut summary) {
            tracing::warn!(error = %e, "mend: dead worker cleanup failed");
            // Non-fatal — continue with remaining steps.
        }

        // Step 4: Agent log file cleanup.
        if let Err(e) = self.cleanup_old_agent_logs(store, &mut summary).await {
            tracing::warn!(error = %e, "mend: agent log cleanup failed");
            // Non-fatal — continue with remaining steps.
        }

        // Step 4.5: Trace retention cleanup.
        if let Err(e) = self.cleanup_old_traces(&mut summary) {
            tracing::warn!(error = %e, "mend: trace cleanup failed");
            // Non-fatal — continue with remaining steps.
        }

        // Step 4.75: Learning consolidation.
        if let Err(e) = self.cleanup_learnings(&mut summary) {
            tracing::warn!(error = %e, "mend: learning cleanup failed");
            // Non-fatal — continue with remaining steps.
        }

        // Step 4.8: Idle worker flagging.
        if let Err(e) = self.flag_idle_workers(&mut summary) {
            tracing::warn!(error = %e, "mend: idle worker flagging failed");
            // Non-fatal — continue with remaining steps.
        }

        // Step 5: Database health check.
        if let Err(e) = self.check_db_health(store, &mut summary).await {
            tracing::warn!(error = %e, "mend: database health check failed");
            // DB check failure is non-fatal — continue with the summary.
        }

        // Emit cycle summary telemetry.
        let _ = self.telemetry.emit(EventKind::MendCycleSummary {
            beads_released: summary.beads_released,
            locks_removed: summary.locks_removed,
            deps_cleaned: summary.deps_cleaned,
            db_repaired: summary.db_repaired,
            db_rebuilt: summary.db_rebuilt,
            agent_logs_cleaned: summary.agent_logs_cleaned,
            traces_pruned: summary.traces_pruned,
            traces_deleted: summary.traces_cleaned,
            workers_deregistered: summary.workers_deregistered,
        });

        if summary.did_work() {
            tracing::info!(
                beads_released = summary.beads_released,
                locks_removed = summary.locks_removed,
                deps_cleaned = summary.deps_cleaned,
                db_repaired = summary.db_repaired,
                db_rebuilt = summary.db_rebuilt,
                agent_logs_cleaned = summary.agent_logs_cleaned,
                traces_pruned = summary.traces_pruned,
                traces_deleted = summary.traces_cleaned,
                "mend performed cleanup — restarting waterfall"
            );
            StrandResult::WorkCreated
        } else {
            tracing::debug!("mend found nothing to clean");
            StrandResult::NoWork
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Get the age of a file based on its modification time.
fn file_age(path: &Path) -> Option<Duration> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    SystemTime::now().duration_since(modified).ok()
}

/// Try to acquire an exclusive flock on a file (non-blocking).
///
/// Returns `Ok(Some(file))` if acquired, `Ok(None)` if held by another process.
fn try_acquire_flock(path: &Path) -> Result<Option<std::fs::File>> {
    use fs2::FileExt;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;

    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(e.into()),
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
    use crate::types::{Bead, BeadId, BrDependency, ClaimResult, WorkerState};

    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    // ── Mock bead store ─────────────────────────────────────────────────────

    struct MockBeadStore {
        all_beads: Vec<Bead>,
        release_count: Arc<AtomicU32>,
        /// Warnings returned by doctor_check (probe-only).
        check_warnings: Vec<String>,
        /// If Some, doctor_repair succeeds with this report. If None, it fails.
        repair_report: Option<RepairReport>,
        /// Whether full_rebuild() should fail.
        rebuild_fails: bool,
    }

    impl MockBeadStore {
        fn new(beads: Vec<Bead>) -> (Self, Arc<AtomicU32>) {
            let release_count = Arc::new(AtomicU32::new(0));
            (
                MockBeadStore {
                    all_beads: beads,
                    release_count: release_count.clone(),
                    check_warnings: vec![],
                    repair_report: Some(RepairReport::default()),
                    rebuild_fails: false,
                },
                release_count,
            )
        }

        /// doctor_check returns warnings → escalates to repair (which succeeds).
        fn with_doctor_report(mut self, report: RepairReport) -> Self {
            self.check_warnings = report.warnings.clone();
            self.repair_report = Some(report);
            self
        }

        /// doctor_check returns warnings, doctor_repair fails → full rebuild.
        fn with_repair_failure(mut self) -> Self {
            self.check_warnings = vec!["corruption detected".to_string()];
            self.repair_report = None;
            self.rebuild_fails = false;
            self
        }

        /// Everything fails → persistent corruption (ERRORED state).
        fn with_all_recovery_failure(mut self) -> Self {
            self.check_warnings = vec!["corruption detected".to_string()];
            self.repair_report = None;
            self.rebuild_fails = true;
            self
        }
    }

    #[async_trait]
    impl BeadStore for MockBeadStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.all_beads.clone())
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
        async fn doctor_repair(&self) -> Result<RepairReport> {
            match &self.repair_report {
                Some(r) => Ok(RepairReport {
                    warnings: r.warnings.clone(),
                    fixed: r.fixed.clone(),
                }),
                None => anyhow::bail!("database disk image is malformed"),
            }
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            Ok(RepairReport {
                warnings: self.check_warnings.clone(),
                fixed: vec![],
            })
        }
        async fn full_rebuild(&self) -> Result<()> {
            if self.rebuild_fails {
                anyhow::bail!("full rebuild failed: JSONL corrupt")
            } else {
                Ok(())
            }
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(&self, _blocked_id: &BeadId, _blocker_id: &BeadId) -> Result<()> {
            Ok(())
        }
    }

    /// Failing bead store for error-path tests.
    struct FailingStore;

    #[async_trait]
    impl BeadStore for FailingStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            anyhow::bail!("store error")
        }
        async fn list_all(&self) -> Result<Vec<Bead>> {
            anyhow::bail!("store error")
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("store error")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("store error")
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("store error")
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            anyhow::bail!("store error")
        }
        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            anyhow::bail!("store error")
        }
        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            anyhow::bail!("store error")
        }
        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            anyhow::bail!("store error")
        }
        async fn doctor_repair(&self) -> Result<RepairReport> {
            anyhow::bail!("store error")
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            anyhow::bail!("store error")
        }
        async fn full_rebuild(&self) -> Result<()> {
            anyhow::bail!("store error")
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(&self, _blocked_id: &BeadId, _blocker_id: &BeadId) -> Result<()> {
            anyhow::bail!("store error")
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_mend_strand(hb_dir: &Path, lock_dir: &Path, reg_dir: &Path) -> MendStrand {
        MendStrand::new(
            MendConfig::default(),
            hb_dir.to_path_buf(),
            Duration::from_secs(300),
            lock_dir.to_path_buf(),
            "claude-test-worker".to_string(),
            Registry::new(reg_dir),
            Telemetry::new("claude-test-worker".to_string()),
            PathBuf::from("/tmp/needle-test-logs"),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        )
    }

    fn make_bead_with_deps(id: &str, status: BeadStatus, deps: Vec<BrDependency>) -> Bead {
        let dt = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        Bead {
            id: BeadId::from(id),
            title: format!("Bead {id}"),
            body: None,
            priority: 1,
            status,
            assignee: None,
            labels: vec![],
            workspace: PathBuf::from("/tmp/test"),
            dependencies: deps,
            dependents: vec![],
            created_at: dt,
            updated_at: dt,
        }
    }

    fn make_dep(id: &str, status: &str, dep_type: &str) -> BrDependency {
        BrDependency {
            id: BeadId::from(id),
            title: format!("Dep {id}"),
            status: status.to_string(),
            priority: 1,
            dependency_type: dep_type.to_string(),
        }
    }

    fn make_in_progress_bead(id: &str, assignee: &str) -> Bead {
        let dt = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        Bead {
            id: BeadId::from(id),
            title: format!("Bead {id}"),
            body: None,
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some(assignee.to_string()),
            labels: vec![],
            workspace: PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
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
        let json = serde_json::to_string(data).unwrap();
        std::fs::write(path, json).unwrap();
    }

    fn make_stale_heartbeat(worker_id: &str, pid: u32, bead_id: Option<&str>) -> HeartbeatData {
        HeartbeatData {
            worker_id: worker_id.to_string(),
            qualified_id: format!("claude-{}", worker_id),
            pid,
            state: WorkerState::Executing,
            current_bead: bead_id.map(BeadId::from),
            workspace: PathBuf::from("/tmp/test"),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(600),
            started_at: Utc::now() - chrono::Duration::seconds(3600),
            beads_processed: 0,
            session: worker_id.to_string(),
            heartbeat_file: None,
        }
    }

    use super::super::Strand;

    // ── Stale claim cleanup tests ────────────────────────────────────────────

    #[tokio::test]
    async fn crashed_peer_bead_released_returns_work_created() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Write a stale heartbeat with dead PID.
        write_heartbeat(
            hb_dir.path(),
            &make_stale_heartbeat("dead-worker", 99_999_999, Some("nd-orphan")),
        );

        let (store, release_count) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::WorkCreated),
            "expected WorkCreated after releasing crashed peer's bead, got: {result:?}"
        );
        assert_eq!(release_count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn no_stale_peers_returns_no_work() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork when nothing to clean, got: {result:?}"
        );
    }

    // ── Orphaned in-progress bead tests ─────────────────────────────────────

    #[tokio::test]
    async fn orphaned_in_progress_bead_released() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // An in-progress bead assigned to a worker that doesn't exist.
        let bead = make_in_progress_bead("nd-orphan", "dead-worker");

        let (store, release_count) = MockBeadStore::new(vec![bead]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::WorkCreated),
            "expected WorkCreated after releasing orphaned in-progress bead, got: {result:?}"
        );
        assert_eq!(release_count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn in_progress_bead_with_live_worker_not_released() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // An in-progress bead assigned to a worker that is registered and alive.
        let bead = make_in_progress_bead("nd-active", "alive-worker");

        // Register the worker with our own PID (which is alive).
        let registry = Registry::new(reg_dir.path());
        registry
            .register(crate::registry::WorkerEntry {
                id: "alive-worker".to_string(),
                pid: std::process::id(),
                workspace: PathBuf::from("/tmp/test"),
                agent: "test".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 0,
            })
            .unwrap();

        let (store, release_count) = MockBeadStore::new(vec![bead]);
        let mend = MendStrand::new(
            MendConfig::default(),
            hb_dir.path().to_path_buf(),
            Duration::from_secs(300),
            lock_dir.path().to_path_buf(),
            "test-worker".to_string(),
            registry,
            Telemetry::new("test-worker".to_string()),
            PathBuf::from("/tmp/needle-test-logs"),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        );

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork when assignee is a live worker, got: {result:?}"
        );
        assert_eq!(release_count.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn in_progress_bead_own_worker_not_released() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // An in-progress bead assigned to ourselves (using qualified_id).
        let bead = make_in_progress_bead("nd-mine", "claude-test-worker");

        let (store, release_count) = MockBeadStore::new(vec![bead]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork when bead is assigned to ourselves, got: {result:?}"
        );
        assert_eq!(release_count.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn in_progress_bead_with_dead_registered_worker_released() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // An in-progress bead assigned to a worker that is registered but dead.
        let bead = make_in_progress_bead("nd-stale", "dead-registered");

        // Register the worker with a dead PID.
        let registry = Registry::new(reg_dir.path());
        registry
            .register(crate::registry::WorkerEntry {
                id: "dead-registered".to_string(),
                pid: 99_999_999,
                workspace: PathBuf::from("/tmp/test"),
                agent: "test".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 0,
            })
            .unwrap();

        let (store, release_count) = MockBeadStore::new(vec![bead]);
        let mend = MendStrand::new(
            MendConfig::default(),
            hb_dir.path().to_path_buf(),
            Duration::from_secs(300),
            lock_dir.path().to_path_buf(),
            "test-worker".to_string(),
            registry,
            Telemetry::new("test-worker".to_string()),
            PathBuf::from("/tmp/needle-test-logs"),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        );

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::WorkCreated),
            "expected WorkCreated after releasing bead from dead registered worker, got: {result:?}"
        );
        assert_eq!(release_count.load(Ordering::Relaxed), 1);
    }

    // ── Qualified ID collision tests ────────────────────────────────────────

    /// When two workers from different adapter pools share a NATO name (e.g.
    /// "foxtrot"), their qualified IDs differ (e.g. "glm-5-foxtrot" vs
    /// "glm-4_7-foxtrot"). A bead assigned to one must NOT be treated as
    /// orphaned just because the other worker of the same NATO name is alive.
    #[tokio::test]
    async fn collision_same_nato_different_adapter_live_worker_not_released() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Two workers share NATO name "foxtrot" but have different adapters.
        let glm5_foxtrot = "claude-code-glm-5-foxtrot";
        let glm47_foxtrot = "claude-code-glm-4_7-foxtrot";

        // A bead assigned to glm-5-foxtrot.
        let bead = make_in_progress_bead("nd-collision-a", glm5_foxtrot);

        // Register BOTH workers as alive.
        let registry = Registry::new(reg_dir.path());
        registry
            .register(crate::registry::WorkerEntry {
                id: glm5_foxtrot.to_string(),
                pid: std::process::id(),
                workspace: PathBuf::from("/tmp/test"),
                agent: "claude-code-glm-5".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 0,
            })
            .unwrap();
        registry
            .register(crate::registry::WorkerEntry {
                id: glm47_foxtrot.to_string(),
                pid: std::process::id(),
                workspace: PathBuf::from("/tmp/test"),
                agent: "claude-code-glm-4_7".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 100,
            })
            .unwrap();

        // Run mend as glm-4_7-foxtrot.
        let (store, release_count) = MockBeadStore::new(vec![bead]);
        let mend = MendStrand::new(
            MendConfig::default(),
            hb_dir.path().to_path_buf(),
            Duration::from_secs(300),
            lock_dir.path().to_path_buf(),
            glm47_foxtrot.to_string(),
            registry,
            Telemetry::new(glm47_foxtrot.to_string()),
            PathBuf::from("/tmp/needle-test-logs"),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        );

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork — bead assigned to glm-5-foxtrot should NOT be orphaned, got: {result:?}"
        );
        assert_eq!(
            release_count.load(Ordering::Relaxed),
            0,
            "bead must not be released when its owner (glm-5-foxtrot) is alive"
        );
    }

    /// When only one worker of a shared NATO name is dead, the bead assigned
    /// to the dead worker's qualified ID should be released even though the
    /// other worker with the same NATO name is alive.
    #[tokio::test]
    async fn collision_same_nato_different_adapter_dead_worker_released() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        let glm5_foxtrot = "claude-code-glm-5-foxtrot";
        let glm47_foxtrot = "claude-code-glm-4_7-foxtrot";

        // A bead assigned to glm-5-foxtrot (which is dead).
        let bead = make_in_progress_bead("nd-collision-b", glm5_foxtrot);

        // Register glm-5-foxtrot as DEAD and glm-4_7-foxtrot as alive.
        let registry = Registry::new(reg_dir.path());
        registry
            .register(crate::registry::WorkerEntry {
                id: glm5_foxtrot.to_string(),
                pid: 99_999_999, // dead PID
                workspace: PathBuf::from("/tmp/test"),
                agent: "claude-code-glm-5".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 342,
            })
            .unwrap();
        registry
            .register(crate::registry::WorkerEntry {
                id: glm47_foxtrot.to_string(),
                pid: std::process::id(),
                workspace: PathBuf::from("/tmp/test"),
                agent: "claude-code-glm-4_7".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 0,
            })
            .unwrap();

        // Run mend as glm-4_7-foxtrot.
        let (store, release_count) = MockBeadStore::new(vec![bead]);
        let mend = MendStrand::new(
            MendConfig::default(),
            hb_dir.path().to_path_buf(),
            Duration::from_secs(300),
            lock_dir.path().to_path_buf(),
            glm47_foxtrot.to_string(),
            registry,
            Telemetry::new(glm47_foxtrot.to_string()),
            PathBuf::from("/tmp/needle-test-logs"),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        );

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::WorkCreated),
            "expected WorkCreated — bead assigned to dead glm-5-foxtrot should be orphaned, got: {result:?}"
        );
        assert_eq!(
            release_count.load(Ordering::Relaxed),
            1,
            "bead must be released when its owner (glm-5-foxtrot) is dead"
        );
    }

    // ── Orphaned lock file tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn orphaned_lock_file_removed() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Create a lock file with no holder (simulating a dead process).
        // Since no process holds the flock, try_acquire_flock() will succeed
        // immediately and the file will be removed regardless of age.
        let lock_path = lock_dir.path().join("needle-claim-abc123.lock");
        std::fs::write(&lock_path, "").unwrap();

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = MendStrand::new(
            MendConfig::default(),
            hb_dir.path().to_path_buf(),
            Duration::from_secs(300),
            lock_dir.path().to_path_buf(),
            "test-worker".to_string(),
            Registry::new(reg_dir.path()),
            Telemetry::new("test-worker".to_string()),
            hb_dir.path().to_path_buf(),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        );

        let result = mend.evaluate(&store).await;
        // Lock file removal is maintenance, not work creation — it doesn't add
        // claimable beads to the queue, so it should return NoWork.
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork after removing orphaned lock (maintenance doesn't create work), got: {result:?}"
        );
        assert!(
            !lock_path.exists(),
            "orphaned lock file should have been removed"
        );
    }

    #[tokio::test]
    async fn non_needle_lock_files_ignored() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Create a non-needle lock file.
        let path = lock_dir.path().join("other-app.lock");
        std::fs::write(&path, "").unwrap();

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = MendStrand::new(
            MendConfig::default(),
            hb_dir.path().to_path_buf(),
            Duration::from_secs(300),
            lock_dir.path().to_path_buf(),
            "test-worker".to_string(),
            Registry::new(reg_dir.path()),
            Telemetry::new("test-worker".to_string()),
            hb_dir.path().to_path_buf(),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        );

        let result = mend.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert!(path.exists(), "non-needle lock file should NOT be removed");
    }

    // ── Dependency cleanup tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn stale_dependency_link_detected() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // An open bead with a closed blocker — stale dependency.
        let bead = make_bead_with_deps(
            "open-bead",
            BeadStatus::Open,
            vec![make_dep("closed-blocker", "closed", "blocks")],
        );

        let (store, _) = MockBeadStore::new(vec![bead]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        // NOTE: Dependency detection alone does NOT return WorkCreated because we don't
        // actually remove the dependency links. The cleanup_stale_dependencies method
        // only detects and reports stale dependencies—it has no way to remove them.
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork when finding stale dependencies (not actually removed), got: {result:?}"
        );
    }

    #[tokio::test]
    async fn open_blocker_not_cleaned() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // An open bead with an open blocker — NOT stale.
        let bead = make_bead_with_deps(
            "open-bead",
            BeadStatus::Open,
            vec![make_dep("open-blocker", "open", "blocks")],
        );

        let (store, _) = MockBeadStore::new(vec![bead]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork when blocker is still open, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn closed_bead_deps_not_checked() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // A closed bead with a closed blocker — should be ignored entirely.
        let bead = make_bead_with_deps(
            "done-bead",
            BeadStatus::Done,
            vec![make_dep("closed-blocker", "closed", "blocks")],
        );

        let (store, _) = MockBeadStore::new(vec![bead]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    // ── Database health tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn db_repair_returns_no_work() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        let report = RepairReport {
            warnings: vec!["index corruption".to_string()],
            fixed: vec!["rebuilt index".to_string()],
        };
        let (store, _) = MockBeadStore::new(vec![]);
        let store = store.with_doctor_report(report);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork after db repair (maintenance doesn't add claimable beads), got: {result:?}"
        );
    }

    #[tokio::test]
    async fn clean_db_no_work() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    // ── Error handling tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn store_error_returns_strand_error() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // PeerMonitor needs heartbeat files to trigger store interaction.
        // With no heartbeat files, it succeeds. The error comes from list_all
        // in dependency cleanup.
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&FailingStore).await;
        assert!(
            matches!(result, StrandResult::Error(StrandError::StoreError(_))),
            "expected StrandError::StoreError, got: {result:?}"
        );
    }

    // ── Name test ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn strand_name_is_mend() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());
        assert_eq!(mend.name(), "mend");
    }

    // ── Combined cleanup test ────────────────────────────────────────────────

    #[tokio::test]
    async fn multiple_cleanups_combined() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Stale heartbeat with dead PID.
        write_heartbeat(
            hb_dir.path(),
            &make_stale_heartbeat("dead-worker", 99_999_999, Some("nd-orphan")),
        );

        // Stale dependency link.
        let bead = make_bead_with_deps(
            "blocked-bead",
            BeadStatus::Open,
            vec![make_dep("done-blocker", "closed", "blocks")],
        );

        let (store, release_count) = MockBeadStore::new(vec![bead]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::WorkCreated),
            "expected WorkCreated after combined cleanup, got: {result:?}"
        );
        assert_eq!(release_count.load(Ordering::Relaxed), 1);
    }

    // ── file_age helper test ─────────────────────────────────────────────────

    #[test]
    fn file_age_returns_some_for_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello").unwrap();

        let age = file_age(&path);
        assert!(age.is_some());
        // File was just created, age should be very small.
        assert!(age.unwrap() < Duration::from_secs(5));
    }

    #[test]
    fn file_age_returns_none_for_missing_file() {
        let age = file_age(Path::new("/nonexistent/file.txt"));
        assert!(age.is_none());
    }

    // ── try_acquire_flock tests ──────────────────────────────────────────────

    #[test]
    fn try_acquire_flock_on_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        std::fs::write(&path, "").unwrap();

        let result = try_acquire_flock(&path);
        assert!(result.is_ok());
        assert!(result.unwrap().is_some(), "should acquire unheld lock");
    }

    #[test]
    fn try_acquire_flock_nonexistent_file_errors() {
        let result = try_acquire_flock(Path::new("/nonexistent/dir/test.lock"));
        assert!(result.is_err());
    }

    // ── Database recovery pipeline tests ────────────────────────────────────

    #[tokio::test]
    async fn db_check_warnings_escalate_to_repair() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // doctor_check returns warnings, doctor_repair succeeds.
        let report = RepairReport {
            warnings: vec!["index corruption".to_string()],
            fixed: vec!["rebuilt index".to_string()],
        };
        let (store, _) = MockBeadStore::new(vec![]);
        let store = store.with_doctor_report(report);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        // DB repair is maintenance, not work creation — it doesn't add claimable
        // beads to the queue, so it should return NoWork.
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork after db repair (maintenance doesn't create work), got: {result:?}"
        );
    }

    #[tokio::test]
    async fn db_repair_failure_triggers_full_rebuild() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // doctor_check warns, doctor_repair fails, full_rebuild succeeds.
        let (store, _) = MockBeadStore::new(vec![]);
        let store = store.with_repair_failure();
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        // DB rebuild is maintenance, not work creation — it doesn't add claimable
        // beads to the queue, so it should return NoWork.
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork after full rebuild (maintenance doesn't create work), got: {result:?}"
        );
    }

    #[tokio::test]
    async fn persistent_corruption_is_non_fatal() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Everything fails — doctor_check warns, repair fails, rebuild fails.
        let (store, _) = MockBeadStore::new(vec![]);
        let store = store.with_all_recovery_failure();
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        // DB check failure is non-fatal — continues with NoWork.
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork when recovery fails (non-fatal), got: {result:?}"
        );
    }

    #[tokio::test]
    async fn clean_db_check_no_repair() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // doctor_check returns no warnings — no repair needed.
        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork when db is clean, got: {result:?}"
        );
    }

    // ── Agent log cleanup tests ──────────────────────────────────────────────

    /// Set a file's mtime to `days_ago` days in the past.
    fn set_mtime_days_ago(path: &Path, days_ago: u64) {
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(days_ago * 86400);
        filetime::set_file_mtime(path, filetime::FileTime::from_system_time(past)).unwrap();
    }

    fn make_mend_strand_with_logs(
        hb_dir: &Path,
        lock_dir: &Path,
        reg_dir: &Path,
        log_dir: &Path,
        retention_days: u32,
    ) -> MendStrand {
        MendStrand::new(
            MendConfig::default(),
            hb_dir.to_path_buf(),
            Duration::from_secs(300),
            lock_dir.to_path_buf(),
            "test-worker".to_string(),
            Registry::new(reg_dir),
            Telemetry::new("test-worker".to_string()),
            log_dir.to_path_buf(),
            retention_days,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        )
    }

    #[tokio::test]
    async fn agent_log_cleanup_deletes_old_file() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();

        // Create a stale agent log (2 days old, retention = 1 day).
        let log_path = log_dir.path().join("worker-abc-nd-bead1.agent.jsonl");
        std::fs::write(&log_path, b"{}").unwrap();
        set_mtime_days_ago(&log_path, 2);

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand_with_logs(
            hb_dir.path(),
            lock_dir.path(),
            reg_dir.path(),
            log_dir.path(),
            1,
        );

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork after cleaning old agent log (maintenance, not work creation), got: {result:?}"
        );
        assert!(
            !log_path.exists(),
            "stale agent log should have been deleted"
        );
    }

    #[tokio::test]
    async fn agent_log_cleanup_skips_recent_file() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();

        // Create a fresh agent log (0 days old, retention = 1 day).
        let log_path = log_dir.path().join("worker-abc-nd-fresh.agent.jsonl");
        std::fs::write(&log_path, b"{}").unwrap();
        // No mtime change — file is brand-new.

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand_with_logs(
            hb_dir.path(),
            lock_dir.path(),
            reg_dir.path(),
            log_dir.path(),
            1,
        );

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork for recent log, got: {result:?}"
        );
        assert!(log_path.exists(), "recent agent log should not be deleted");
    }

    #[tokio::test]
    async fn agent_log_cleanup_skips_in_progress_bead() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();

        // Create an old log for a bead that is still in-progress.
        let active_bead_id = "nd-active1";
        let log_path = log_dir
            .path()
            .join(format!("worker-abc-{active_bead_id}.agent.jsonl"));
        std::fs::write(&log_path, b"{}").unwrap();
        set_mtime_days_ago(&log_path, 5);

        let bead = make_in_progress_bead(active_bead_id, "some-worker");
        let (store, _) = MockBeadStore::new(vec![bead]);
        let mend = make_mend_strand_with_logs(
            hb_dir.path(),
            lock_dir.path(),
            reg_dir.path(),
            log_dir.path(),
            1,
        );

        let result = mend.evaluate(&store).await;
        // The in-progress bead is also "orphaned" by our mock (some-worker isn't
        // registered), so mend will release it. The log must survive though.
        assert!(
            log_path.exists(),
            "agent log for in-progress bead must not be deleted"
        );
        let _ = result; // outcome depends on whether orphan cleanup fires
    }

    #[tokio::test]
    async fn agent_log_cleanup_disabled_when_retention_zero() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();

        // Old file, but retention_days = 0 means cleanup is disabled.
        let log_path = log_dir.path().join("worker-abc-nd-old.agent.jsonl");
        std::fs::write(&log_path, b"{}").unwrap();
        set_mtime_days_ago(&log_path, 60);

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand_with_logs(
            hb_dir.path(),
            lock_dir.path(),
            reg_dir.path(),
            log_dir.path(),
            0, // disabled
        );

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork when retention is disabled, got: {result:?}"
        );
        assert!(
            log_path.exists(),
            "agent log should not be deleted when retention is disabled"
        );
    }

    #[tokio::test]
    async fn agent_log_cleanup_no_files_skips_scan() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();

        // Log dir exists but has no .agent.jsonl files.
        let unrelated = log_dir.path().join("worker.orchestration.jsonl");
        std::fs::write(&unrelated, b"{}").unwrap();
        set_mtime_days_ago(&unrelated, 5);

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand_with_logs(
            hb_dir.path(),
            lock_dir.path(),
            reg_dir.path(),
            log_dir.path(),
            1,
        );

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork when no agent log files present, got: {result:?}"
        );
        assert!(
            unrelated.exists(),
            "non-agent log files must not be touched"
        );
    }

    // ── Orphaned heartbeat cleanup tests ───────────────────────────────────────

    #[tokio::test]
    async fn orphaned_heartbeat_removed() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Write a stale heartbeat for a worker that is NOT registered.
        write_heartbeat(
            hb_dir.path(),
            &make_stale_heartbeat("ghost-worker", 99_999_999, Some("nd-ghost")),
        );

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        // Orphaned heartbeat cleanup is maintenance, not work creation.
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork after removing orphaned heartbeat (maintenance), got: {result:?}"
        );

        // Heartbeat file should be removed.
        let hb_path = hb_dir.path().join("claude-ghost-worker.json");
        assert!(
            !hb_path.exists(),
            "orphaned heartbeat file should have been removed"
        );
    }

    #[tokio::test]
    async fn registered_heartbeat_not_removed() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Register a worker.
        let registry = Registry::new(reg_dir.path());
        registry
            .register(crate::registry::WorkerEntry {
                id: "claude-registered-worker".to_string(),
                pid: std::process::id(),
                workspace: PathBuf::from("/tmp/test"),
                agent: "test".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 0,
            })
            .unwrap();

        // Write a stale heartbeat for the registered worker.
        write_heartbeat(
            hb_dir.path(),
            &make_stale_heartbeat("registered-worker", std::process::id(), Some("nd-registered")),
        );

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = MendStrand::new(
            MendConfig::default(),
            hb_dir.path().to_path_buf(),
            Duration::from_secs(300),
            lock_dir.path().to_path_buf(),
            "test-worker".to_string(),
            registry,
            Telemetry::new("test-worker".to_string()),
            PathBuf::from("/tmp/needle-test-logs"),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        );

        let result = mend.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));

        // Heartbeat file should NOT be removed (worker is registered).
        let hb_path = hb_dir.path().join("claude-registered-worker.json");
        assert!(
            hb_path.exists(),
            "registered worker's heartbeat file should NOT be removed"
        );
    }

    #[tokio::test]
    async fn fresh_orphaned_heartbeat_not_removed() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Write a FRESH (non-stale) heartbeat for a worker that is NOT registered.
        let fresh_hb = HeartbeatData {
            worker_id: "fresh-ghost".to_string(),
            qualified_id: "claude-fresh-ghost".to_string(),
            pid: 99_999_999,
            state: WorkerState::Executing,
            current_bead: Some(BeadId::from("nd-fresh")),
            workspace: PathBuf::from("/tmp/test"),
            last_heartbeat: Utc::now(), // Fresh heartbeat
            started_at: Utc::now(),
            beads_processed: 0,
            session: "fresh-ghost".to_string(),
            heartbeat_file: None,
        };
        write_heartbeat(hb_dir.path(), &fresh_hb);

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));

        // Fresh orphaned heartbeat should NOT be removed (only stale ones).
        let hb_path = hb_dir.path().join("claude-fresh-ghost.json");
        assert!(
            hb_path.exists(),
            "fresh orphaned heartbeat should not be removed (only stale ones)"
        );
    }

    #[tokio::test]
    async fn multiple_orphaned_heartbeats_removed() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Write multiple stale heartbeats for unregistered workers.
        write_heartbeat(
            hb_dir.path(),
            &make_stale_heartbeat("ghost-1", 99_999_998, Some("nd-g1")),
        );
        write_heartbeat(
            hb_dir.path(),
            &make_stale_heartbeat("ghost-2", 99_999_997, Some("nd-g2")),
        );
        write_heartbeat(
            hb_dir.path(),
            &make_stale_heartbeat("ghost-3", 99_999_996, Some("nd-g3")),
        );

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));

        // All orphaned heartbeat files should be removed.
        assert!(!hb_dir.path().join("claude-ghost-1.json").exists());
        assert!(!hb_dir.path().join("claude-ghost-2.json").exists());
        assert!(!hb_dir.path().join("claude-ghost-3.json").exists());
    }

    #[tokio::test]
    async fn orphaned_heartbeat_with_qualified_id_removed() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Write a heartbeat with a fully-qualified ID (as written by current workers).
        let hb = HeartbeatData {
            worker_id: "nato-name".to_string(),
            qualified_id: "claude-code-glm-5-nato-name".to_string(),
            pid: 99_999_999,
            state: WorkerState::Executing,
            current_bead: Some(BeadId::from("nd-qualified")),
            workspace: PathBuf::from("/tmp/test"),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(600),
            started_at: Utc::now(),
            beads_processed: 0,
            session: "nato-name".to_string(),
            heartbeat_file: None,
        };
        write_heartbeat(hb_dir.path(), &hb);

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));

        // Heartbeat file keyed by qualified ID should be removed.
        let hb_path = hb_dir.path().join("claude-code-glm-5-nato-name.json");
        assert!(
            !hb_path.exists(),
            "orphaned heartbeat file (qualified ID) should have been removed"
        );
    }

    #[tokio::test]
    async fn own_heartbeat_not_removed_as_orphan() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Write a stale heartbeat for ourselves (should NOT be removed as orphan).
        write_heartbeat(
            hb_dir.path(),
            &make_stale_heartbeat("test-worker", 99_999_999, Some("nd-own")),
        );

        let (store, _) = MockBeadStore::new(vec![]);
        // Run as claude-test-worker (matches heartbeat qualified_id).
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));

        // Our own heartbeat should NOT be removed even if it appears orphaned.
        let hb_path = hb_dir.path().join("claude-test-worker.json");
        assert!(
            hb_path.exists(),
            "own heartbeat should not be removed as orphaned"
        );
    }

    // ── Dead worker registry cleanup tests ───────────────────────────────────────

    #[tokio::test]
    async fn dead_worker_removed_from_registry() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        let registry = Registry::new(reg_dir.path());

        // Register a worker with a dead PID.
        registry
            .register(crate::registry::WorkerEntry {
                id: "dead-worker".to_string(),
                pid: 99_999_999,
                workspace: PathBuf::from("/tmp/test"),
                agent: "test".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 5,
            })
            .unwrap();

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = MendStrand::new(
            MendConfig::default(),
            hb_dir.path().to_path_buf(),
            Duration::from_secs(300),
            lock_dir.path().to_path_buf(),
            "test-worker".to_string(),
            registry,
            Telemetry::new("test-worker".to_string()),
            PathBuf::from("/tmp/needle-test-logs"),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        );

        let result = mend.evaluate(&store).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "expected NoWork after registry cleanup (maintenance), got: {result:?}"
        );

        // Verify the dead worker was removed from the registry file.
        let reg_content = std::fs::read_to_string(reg_dir.path().join("workers.json")).unwrap();
        let reg_file: crate::registry::RegistryFile = serde_json::from_str(&reg_content).unwrap();
        assert!(
            reg_file.workers.is_empty(),
            "dead worker should have been removed from registry"
        );
    }

    #[tokio::test]
    async fn live_worker_not_removed_from_registry() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        let registry = Registry::new(reg_dir.path());

        // Register a live worker with current PID.
        registry
            .register(crate::registry::WorkerEntry {
                id: "live-worker".to_string(),
                pid: std::process::id(),
                workspace: PathBuf::from("/tmp/test"),
                agent: "test".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 10,
            })
            .unwrap();

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = MendStrand::new(
            MendConfig::default(),
            hb_dir.path().to_path_buf(),
            Duration::from_secs(300),
            lock_dir.path().to_path_buf(),
            "test-worker".to_string(),
            registry,
            Telemetry::new("test-worker".to_string()),
            PathBuf::from("/tmp/needle-test-logs"),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        );

        let result = mend.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));

        // Verify the live worker is still in the registry.
        let workers = registry.list().unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].id, "live-worker");
    }

    #[tokio::test]
    async fn own_worker_entry_not_removed_from_registry() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        let registry = Registry::new(reg_dir.path());

        // Register ourselves as a worker.
        registry
            .register(crate::registry::WorkerEntry {
                id: "claude-test-worker".to_string(),
                pid: std::process::id(),
                workspace: PathBuf::from("/tmp/test"),
                agent: "test".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 0,
            })
            .unwrap();

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = MendStrand::new(
            MendConfig::default(),
            hb_dir.path().to_path_buf(),
            Duration::from_secs(300),
            lock_dir.path().to_path_buf(),
            "claude-test-worker".to_string(),
            registry,
            Telemetry::new("claude-test-worker".to_string()),
            PathBuf::from("/tmp/needle-test-logs"),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        );

        let result = mend.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));

        // Verify our own entry is still in the registry.
        let workers = registry.list().unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].id, "claude-test-worker");
    }

    #[tokio::test]
    async fn multiple_dead_workers_removed_from_registry() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        let registry = Registry::new(reg_dir.path());

        // Register multiple dead workers.
        for i in 0..5 {
            registry
                .register(crate::registry::WorkerEntry {
                    id: format!("dead-worker-{}", i),
                    pid: 99_999_999 - i,
                    workspace: PathBuf::from("/tmp/test"),
                    agent: "test".to_string(),
                    model: None,
                    provider: None,
                    started_at: Utc::now(),
                    beads_processed: i as u64,
                })
                .unwrap();
        }

        // Register one live worker.
        registry
            .register(crate::registry::WorkerEntry {
                id: "live-worker".to_string(),
                pid: std::process::id(),
                workspace: PathBuf::from("/tmp/test"),
                agent: "test".to_string(),
                model: None,
                provider: None,
                started_at: Utc::now(),
                beads_processed: 100,
            })
            .unwrap();

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = MendStrand::new(
            MendConfig::default(),
            hb_dir.path().to_path_buf(),
            Duration::from_secs(300),
            lock_dir.path().to_path_buf(),
            "test-worker".to_string(),
            registry,
            Telemetry::new("test-worker".to_string()),
            PathBuf::from("/tmp/needle-test-logs"),
            0,
            PathBuf::from("/tmp/test-traces"),
            30,
            7,
            PathBuf::from("/tmp/test-workspace"),
            80,
        );

        let result = mend.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));

        // Verify only the live worker remains.
        let workers = registry.list().unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].id, "live-worker");
    }

    #[tokio::test]
    async fn registry_cleanup_handles_missing_file() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Don't create any registry file.

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        // Should not error — missing registry file is handled gracefully.
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn registry_cleanup_handles_corrupt_file() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Write invalid JSON to the registry file.
        let reg_path = reg_dir.path().join("workers.json");
        std::fs::write(&reg_path, "not valid json").unwrap();

        let (store, _) = MockBeadStore::new(vec![]);
        let mend = make_mend_strand(hb_dir.path(), lock_dir.path(), reg_dir.path());

        let result = mend.evaluate(&store).await;
        // Should not error — corrupt registry file is handled gracefully.
        assert!(matches!(result, StrandResult::NoWork));
    }
}
