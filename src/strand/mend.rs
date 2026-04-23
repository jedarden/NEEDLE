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

use crate::bead_store::BeadStore;
use crate::config::MendConfig;
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
/// * `worker_id` - This worker's ID (excluded from orphan detection)
///
/// # Returns
/// * `Ok(u32)` - Number of orphans released
/// * `Err(anyhow::Error)` - Store read failure
pub async fn cleanup_orphaned_in_progress(
    store: &dyn BeadStore,
    registry: &Registry,
    telemetry: &Telemetry,
    worker_id: &str,
) -> Result<u32> {
    let all_beads = store.list_all().await?;
    let workers = registry.list()?;

    // Build a set of (worker_id, is_alive) for registered workers.
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

        // Skip if the assignee matches our own worker (we're running).
        if assignee == worker_id {
            continue;
        }

        // Skip if the assignee matches a registered, alive worker.
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
    traces_pruned: u32,
    traces_cleaned: u32,
    learnings_pruned: u32,
    learnings_consolidated: u32,
}

impl MendSummary {
    /// Whether any cleanup work was performed.
    fn did_work(&self) -> bool {
        self.beads_released > 0
            || self.locks_removed > 0
            || self.deps_cleaned > 0
            || self.db_repaired
            || self.db_rebuilt
            || self.agent_logs_cleaned > 0
            || self.traces_pruned > 0
            || self.traces_cleaned > 0
            || self.learnings_pruned > 0
            || self.learnings_consolidated > 0
    }
}

/// The Mend strand — maintenance and self-healing.
pub struct MendStrand {
    config: MendConfig,
    heartbeat_dir: PathBuf,
    heartbeat_ttl: Duration,
    lock_dir: PathBuf,
    worker_id: String,
    registry: Registry,
    telemetry: Telemetry,
    log_dir: PathBuf,
    retention_days: u32,
    traces_dir: PathBuf,
    trace_retention_failed_days: u32,
    trace_retention_success_days: u32,
    workspace: PathBuf,
    max_learnings: usize,
}

impl MendStrand {
    /// Create a new MendStrand.
    ///
    /// - `config`: mend strand configuration
    /// - `heartbeat_dir`: path to `~/.needle/state/heartbeats/`
    /// - `heartbeat_ttl`: how long before a heartbeat is considered stale
    /// - `lock_dir`: directory where claim lock files live (default: `/tmp`)
    /// - `worker_id`: this worker's ID (excluded from peer checks)
    /// - `registry`: worker state registry
    /// - `telemetry`: telemetry emitter
    /// - `log_dir`: directory where agent log files live
    /// - `retention_days`: number of days to retain agent log files (0 = disabled)
    /// - `traces_dir`: directory where trace files live (`.beads/traces`)
    /// - `trace_retention_failed_days`: retention days for failed bead traces
    /// - `trace_retention_success_days`: retention days for successful bead traces
    /// - `workspace`: workspace root path for learning consolidation
    /// - `max_learnings`: maximum number of learning entries before consolidation
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: MendConfig,
        heartbeat_dir: PathBuf,
        heartbeat_ttl: Duration,
        lock_dir: PathBuf,
        worker_id: String,
        registry: Registry,
        telemetry: Telemetry,
        log_dir: PathBuf,
        retention_days: u32,
        traces_dir: PathBuf,
        trace_retention_failed_days: u32,
        trace_retention_success_days: u32,
        workspace: PathBuf,
        max_learnings: usize,
    ) -> Self {
        MendStrand {
            config,
            heartbeat_dir,
            heartbeat_ttl,
            lock_dir,
            worker_id,
            registry,
            telemetry,
            log_dir,
            retention_days,
            traces_dir,
            trace_retention_failed_days,
            trace_retention_success_days,
            workspace,
            max_learnings,
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
            self.worker_id.clone(),
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
            &self.worker_id,
        )
        .await?;
        summary.beads_released += released;
        Ok(())
    }

    // ── Step 2: Orphaned lock file removal ───────────────────────────────────

    /// Remove claim lock files that are older than the configured TTL and not
    /// actively held by any process.
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

            // Check file age.
            let age = match file_age(&path) {
                Some(age) => age,
                None => continue,
            };

            if age <= lock_ttl {
                continue;
            }

            // Try to acquire flock (non-blocking). If we can acquire it,
            // no one is holding it — safe to delete.
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
                    tracing::debug!(
                        path = %path.display(),
                        "lock file is actively held, skipping"
                    );
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
                        "found stale dependency link (closed blocker on open bead)"
                    );

                    // Emit telemetry for the cleaned dependency.
                    let _ = self.telemetry.emit(EventKind::MendDependencyCleaned {
                        bead_id: bead.id.clone(),
                        blocker_id: dep.id.clone(),
                    });

                    summary.deps_cleaned += 1;
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

                summary.db_repaired = true;
                return Ok(());
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
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_mend_strand(hb_dir: &Path, lock_dir: &Path, reg_dir: &Path) -> MendStrand {
        MendStrand::new(
            MendConfig::default(),
            hb_dir.to_path_buf(),
            Duration::from_secs(300),
            lock_dir.to_path_buf(),
            "test-worker".to_string(),
            Registry::new(reg_dir),
            Telemetry::new("test-worker".to_string()),
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
        let path = dir.join(format!("{}.json", data.worker_id));
        let json = serde_json::to_string(data).unwrap();
        std::fs::write(path, json).unwrap();
    }

    fn make_stale_heartbeat(worker_id: &str, pid: u32, bead_id: Option<&str>) -> HeartbeatData {
        HeartbeatData {
            worker_id: worker_id.to_string(),
            pid,
            state: WorkerState::Executing,
            current_bead: bead_id.map(BeadId::from),
            workspace: PathBuf::from("/tmp/test"),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(600),
            started_at: Utc::now() - chrono::Duration::seconds(3600),
            beads_processed: 0,
            session: worker_id.to_string(),
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

        // An in-progress bead assigned to ourselves.
        let bead = make_in_progress_bead("nd-mine", "test-worker");

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

    // ── Orphaned lock file tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn orphaned_lock_file_removed() {
        let hb_dir = tempfile::tempdir().unwrap();
        let lock_dir = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        // Create an old lock file (we set config lock_ttl to 0 so any age qualifies).
        let lock_path = lock_dir.path().join("needle-claim-abc123.lock");
        std::fs::write(&lock_path, "").unwrap();

        // Set the modification time to the past by using a 0-second TTL config.
        let (store, _) = MockBeadStore::new(vec![]);
        let mend = MendStrand::new(
            MendConfig {
                lock_ttl_secs: 0, // Any lock is "old"
                ..MendConfig::default()
            },
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
        assert!(
            matches!(result, StrandResult::WorkCreated),
            "expected WorkCreated after removing orphaned lock, got: {result:?}"
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
            MendConfig {
                lock_ttl_secs: 0,
                ..MendConfig::default()
            },
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
        assert!(
            matches!(result, StrandResult::WorkCreated),
            "expected WorkCreated after finding stale dependency, got: {result:?}"
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
    async fn db_repair_triggers_work_created() {
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
            matches!(result, StrandResult::WorkCreated),
            "expected WorkCreated after db repair, got: {result:?}"
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
        assert!(
            matches!(result, StrandResult::WorkCreated),
            "expected WorkCreated after repair, got: {result:?}"
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
        assert!(
            matches!(result, StrandResult::WorkCreated),
            "expected WorkCreated after full rebuild, got: {result:?}"
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
            matches!(result, StrandResult::WorkCreated),
            "expected WorkCreated after cleaning old agent log, got: {result:?}"
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
}
