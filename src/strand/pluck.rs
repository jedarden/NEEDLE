//! Pluck strand: primary bead selection from the assigned workspace.
//!
//! Pluck handles >90% of all bead processing. It queries the bead store for
//! unassigned, ready beads, filters by excluded labels, and sorts them in
//! deterministic priority order: `(priority ASC, created_at ASC, id ASC)`.
//!
//! Given the same queue state, every worker computes the same candidate list.

use crate::bead_store::{BeadStore, Filters};
use crate::mitosis::detects_needle_internal_config;
use crate::telemetry::Telemetry;
use crate::types::{Bead, BeadId, StrandError, StrandResult};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{atomic::AtomicUsize, atomic::Ordering, Mutex};

/// Default labels excluded from Pluck selection when not configured.
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];

/// Statistics collected during candidate filtering for starvation telemetry.
#[derive(Debug, Default)]
struct FilteringStats {
    /// Count of open beads before any filtering.
    open_count: usize,
    /// Count of beads excluded during filtering.
    excluded_count: usize,
    /// Aggregated reasons why candidates were excluded.
    exclusion_reasons: Vec<String>,
}

/// Persistent starvation record written to NEEDLE workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StarvationRecord {
    /// UTC timestamp when starvation was detected.
    timestamp: chrono::DateTime<chrono::Utc>,
    /// Target workspace that was being processed (not NEEDLE workspace).
    target_workspace: String,
    /// Count of open beads before filtering.
    open_count: usize,
    /// Count of beads excluded during filtering.
    excluded_count: usize,
    /// Reasons why beads were excluded.
    exclusion_reasons: Vec<String>,
}

/// The Pluck strand — primary work selection.
pub struct PluckStrand {
    /// Labels to exclude from candidate selection.
    exclude_labels: Vec<String>,
    /// Auto-split beads after this many consecutive failures (0 = disabled).
    split_after_failures: u32,
    /// Telemetry emitter for starvation events.
    telemetry: Telemetry,
    /// NEEDLE workspace path for persistent starvation records.
    needle_workspace: Option<PathBuf>,
    /// Whether to write persistent starvation records to NEEDLE workspace.
    persistent_starvation_records: bool,
    /// Count of open beads from the most recent evaluation.
    /// Uses AtomicUsize for thread-safe interior mutability.
    last_open_count: AtomicUsize,
    /// Count of beads excluded during the most recent evaluation.
    last_excluded_count: AtomicUsize,
    /// Aggregated exclusion reasons from the most recent evaluation.
    /// Uses Mutex for thread-safe interior mutability.
    last_exclusion_reasons: Mutex<Vec<String>>,
}

impl PluckStrand {
    /// Create a new PluckStrand with the given exclude labels and telemetry.
    ///
    /// If `exclude_labels` is empty, the default set (`deferred`, `human`,
    /// `blocked`) is used.
    pub fn new(exclude_labels: Vec<String>, telemetry: Telemetry) -> Self {
        let labels = if exclude_labels.is_empty() {
            DEFAULT_EXCLUDE_LABELS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        } else {
            exclude_labels
        };
        PluckStrand {
            exclude_labels: labels,
            split_after_failures: 3, // default threshold
            telemetry,
            needle_workspace: None,
            persistent_starvation_records: false,
            last_open_count: AtomicUsize::new(0),
            last_excluded_count: AtomicUsize::new(0),
            last_exclusion_reasons: Mutex::new(Vec::new()),
        }
    }

    /// Create a new PluckStrand with the given exclude labels, split threshold, and telemetry.
    ///
    /// If `exclude_labels` is empty, the default set (`deferred`, `human`,
    /// `blocked`) is used.
    pub fn with_split_threshold(
        exclude_labels: Vec<String>,
        split_after_failures: u32,
        telemetry: Telemetry,
    ) -> Self {
        let labels = if exclude_labels.is_empty() {
            DEFAULT_EXCLUDE_LABELS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        } else {
            exclude_labels
        };
        PluckStrand {
            exclude_labels: labels,
            split_after_failures,
            telemetry,
            needle_workspace: None,
            persistent_starvation_records: false,
            last_open_count: AtomicUsize::new(0),
            last_excluded_count: AtomicUsize::new(0),
            last_exclusion_reasons: Mutex::new(Vec::new()),
        }
    }

    /// Create a new PluckStrand with persistent starvation records enabled.
    ///
    /// When `persistent_starvation_records` is true, starvation events are
    /// written to `needle_workspace/state/starvation-records.jsonl`.
    /// Records are never written to target workspaces, only to NEEDLE workspace.
    pub fn with_persistent_records(
        exclude_labels: Vec<String>,
        split_after_failures: u32,
        telemetry: Telemetry,
        needle_workspace: PathBuf,
        persistent_starvation_records: bool,
    ) -> Self {
        let labels = if exclude_labels.is_empty() {
            DEFAULT_EXCLUDE_LABELS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        } else {
            exclude_labels
        };
        PluckStrand {
            exclude_labels: labels,
            split_after_failures,
            telemetry,
            needle_workspace: Some(needle_workspace),
            persistent_starvation_records,
            last_open_count: AtomicUsize::new(0),
            last_excluded_count: AtomicUsize::new(0),
            last_exclusion_reasons: Mutex::new(Vec::new()),
        }
    }

    /// Extract the failure count from a bead's labels.
    ///
    /// Labels follow the pattern `failure-count:N`. Returns the count if found,
    /// or 0 if no failure-count label exists.
    fn extract_failure_count(bead: &Bead) -> u32 {
        bead.labels
            .iter()
            .filter_map(|l| l.strip_prefix("failure-count:"))
            .filter_map(|s| s.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
    }

    /// Sort candidates in deterministic priority order.
    ///
    /// Sort key: `(priority ASC, failure_count ASC, created_at ASC, id ASC)`.
    ///
    /// `failure_count` sits ahead of `created_at` deliberately: without it, a
    /// bead that keeps failing is (by construction) always at least as old as
    /// its ready siblings, so it sorts to slot 1 every single cycle and the
    /// worker never even tries the other ready work sitting behind it. This
    /// does not by itself stop the retry loop — `split_after_failures` /
    /// `OutcomeConfig::quarantine_after_failures` own that — it just stops a
    /// struggling bead from starving healthier ready beads at the same
    /// priority while it climbs toward the quarantine threshold.
    /// The id tie-breaker ensures identical ordering across platforms.
    fn sort_candidates(candidates: &mut [Bead]) {
        candidates.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| Self::extract_failure_count(a).cmp(&Self::extract_failure_count(b)))
                .then_with(|| a.created_at.cmp(&b.created_at))
                .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
        });
    }

    /// Get the filtering statistics from the most recent evaluation.
    ///
    /// Returns `(open_count, excluded_count, exclusion_reasons)` where:
    /// - `open_count` is the count of beads returned by the store before filtering
    /// - `excluded_count` is the count of beads excluded during filtering
    /// - `exclusion_reasons` is a vector of strings describing why each bead was excluded
    pub fn last_filtering_stats(&self) -> (usize, usize, Vec<String>) {
        (
            self.last_open_count.load(Ordering::Relaxed),
            self.last_excluded_count.load(Ordering::Relaxed),
            self.last_exclusion_reasons.lock().unwrap().clone(),
        )
    }

    /// Write a persistent starvation record to NEEDLE workspace.
    ///
    /// Records are written in JSONL format to `~/.needle/state/starvation-records.jsonl`.
    /// This method is called only when `persistent_starvation_records` is enabled.
    fn write_starvation_record(
        &self,
        target_workspace: &str,
        open_count: usize,
        excluded_count: usize,
        exclusion_reasons: &[String],
    ) -> Result<()> {
        let needle_home = self
            .needle_workspace
            .as_ref()
            .context("needle_workspace not set - cannot write persistent record")?;

        // Create state directory if it doesn't exist
        let state_dir = needle_home.join("state");
        std::fs::create_dir_all(&state_dir).with_context(|| {
            format!("failed to create state directory: {}", state_dir.display())
        })?;

        // Write record to starvation-records.jsonl (append mode)
        let record_path = state_dir.join("starvation-records.jsonl");
        let record = StarvationRecord {
            timestamp: Utc::now(),
            target_workspace: target_workspace.to_string(),
            open_count,
            excluded_count,
            exclusion_reasons: exclusion_reasons.to_vec(),
        };

        // Serialize to JSON and append to file
        let record_json =
            serde_json::to_string(&record).context("failed to serialize starvation record")?;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&record_path)
            .with_context(|| {
                format!(
                    "failed to open starvation records file: {}",
                    record_path.display()
                )
            })?;

        use std::io::Write;
        writeln!(file, "{}", record_json).with_context(|| {
            format!(
                "failed to write starvation record to: {}",
                record_path.display()
            )
        })?;

        tracing::debug!(
            record_path = %record_path.display(),
            target_workspace = %target_workspace,
            "Wrote persistent starvation record"
        );

        Ok(())
    }
}

#[async_trait::async_trait]
impl super::Strand for PluckStrand {
    fn name(&self) -> &str {
        "pluck"
    }

    #[tracing::instrument(
        name = "strand.pluck",
        skip(self, store),
        fields(
            strand = "pluck",
            exclude_labels = ?self.exclude_labels,
            split_threshold = self.split_after_failures,
        )
    )]
    async fn evaluate(&self, store: &dyn BeadStore, exclusions: &HashSet<BeadId>) -> StrandResult {
        tracing::debug!(
            exclude_labels = ?self.exclude_labels,
            split_threshold = self.split_after_failures,
            "Pluck strand evaluation starting"
        );

        // Check if the home bead store exists. If not, skip this strand.
        // This distinguishes between "no home store configured" (expected for
        // roam-only workers) and "home store is broken" (unexpected error).
        if !store.has_valid_store() {
            tracing::info!(
                "Home workspace has no .beads/ directory — skipping Pluck strand \
                 (expected for roam-only workers)"
            );
            return StrandResult::Skipped {
                reason: "no_home_store".to_string(),
            };
        }

        // 1. Query bead store for ready, unassigned beads.
        let filters = Filters {
            assignee: None,
            exclude_labels: self.exclude_labels.clone(),
            exclude_ids: HashSet::new(),
        };

        tracing::debug!(
            filters = ?filters,
            "Querying bead store for ready candidates"
        );

        let mut candidates = match store.ready(&filters).await {
            Ok(beads) => {
                tracing::debug!(
                    count = beads.len(),
                    "Bead store returned {} candidates",
                    beads.len()
                );
                beads
            }
            Err(e) => {
                // Log the full error chain to capture stderr/exit code details.
                // The Display impl (%e) only shows the top-level message, losing
                // the underlying stderr from br/bf commands. Use {:?} or iterate
                // the chain to preserve diagnostic information.
                let error_chain: Vec<String> = std::iter::once(format!("{}", e))
                    .chain(e.chain().map(|cause| format!("  caused by: {}", cause)))
                    .collect();
                tracing::error!(
                    error = %e,
                    error_chain = ?error_chain,
                    "Bead store query failed"
                );
                // Bead store error is semantically different from NoWork.
                return StrandResult::Error(StrandError::StoreError(e));
            }
        };

        // Initialize filtering statistics for starvation telemetry.
        let mut stats = FilteringStats {
            open_count: candidates.len(),
            ..Default::default()
        };

        // 2. Filter: remove beads with excluded labels.
        //    Defensive guard — store.ready() passes exclude_labels in its Filters,
        //    but the backing CLI may not include label data in every query type.
        //    Filtering here guarantees excluded-label beads are never presented as
        //    candidates regardless of backend behaviour, preventing the
        //    SELECTING→CLAIMING→RETRYING spin loop observed when br ready --json
        //    omits label fields for some beads.
        let before_label_filter = candidates.len();

        // First pass: collect excluded beads and their reasons for telemetry.
        let excluded_beads: Vec<_> = candidates
            .iter()
            .filter(|b| b.labels.iter().any(|l| self.exclude_labels.contains(l)))
            .map(|b| {
                let excluded_labels: Vec<_> = b
                    .labels
                    .iter()
                    .filter(|l| self.exclude_labels.contains(l))
                    .cloned()
                    .collect();
                (b.id.as_ref().to_string(), excluded_labels)
            })
            .collect();

        // Second pass: perform the actual filtering.
        candidates.retain(|b| !b.labels.iter().any(|l| self.exclude_labels.contains(l)));
        let after_label_filter = candidates.len();

        if before_label_filter != after_label_filter {
            let label_excluded_count = before_label_filter - after_label_filter;
            stats.excluded_count += label_excluded_count;

            tracing::debug!(
                excluded_count = label_excluded_count,
                remaining = after_label_filter,
                excluded_labels = ?self.exclude_labels,
                "Label filtering excluded {} beads",
                label_excluded_count
            );

            // Log each excluded bead at DEBUG level and collect reasons for telemetry.
            for (id, excluded_labels) in &excluded_beads {
                tracing::debug!(
                    bead_id = %id,
                    labels = ?excluded_labels,
                    "Excluded bead due to labels"
                );

                // Add to exclusion reasons for telemetry.
                for label in excluded_labels {
                    stats.exclusion_reasons.push(format!("label:{}", label));
                }
            }
        } else {
            tracing::debug!(
                count = after_label_filter,
                "No beads excluded by label filter"
            );
        }

        // 3. Filter: remove beads that are actively in_progress (claimed by another worker)
        //    and Open beads with a stale assignee. These are never claimable — the claimer
        //    will reject them every time, causing a hot loop.
        let before_status_filter = candidates.len();

        // First pass: collect excluded beads and their reasons for telemetry.
        let excluded_by_status: Vec<_> = candidates
            .iter()
            .filter(|b| {
                matches!(b.status, crate::types::BeadStatus::InProgress)
                    || (b.status == crate::types::BeadStatus::Open && b.assignee.is_some())
            })
            .map(|b| {
                let reason = if matches!(b.status, crate::types::BeadStatus::InProgress) {
                    "status:in_progress".to_string()
                } else {
                    format!("assignee:{}", b.assignee.as_ref().unwrap())
                };
                (b.id.as_ref().to_string(), reason)
            })
            .collect();

        // Second pass: perform the actual filtering.
        candidates.retain(|b| {
            !(matches!(b.status, crate::types::BeadStatus::InProgress)
                || (b.status == crate::types::BeadStatus::Open && b.assignee.is_some()))
        });
        let after_status_filter = candidates.len();

        if before_status_filter != after_status_filter {
            let status_excluded_count = before_status_filter - after_status_filter;
            stats.excluded_count += status_excluded_count;

            tracing::debug!(
                filtered_count = status_excluded_count,
                remaining = after_status_filter,
                "Status/assignee filtering removed {} beads",
                status_excluded_count
            );

            // Log each excluded bead at DEBUG level and collect reasons for telemetry.
            for (id, reason) in &excluded_by_status {
                tracing::debug!(
                    bead_id = %id,
                    reason = %reason,
                    "Excluded bead due to status/assignee"
                );

                // Add to exclusion reasons for telemetry.
                stats.exclusion_reasons.push(reason.clone());
            }
        } else {
            tracing::debug!(
                count = after_status_filter,
                "No beads excluded by status/assignee filter"
            );
        }

        // 4. Sort: deterministic (priority, created_at, id).
        if !candidates.is_empty() {
            let first = &candidates[0];
            tracing::debug!(
                total = candidates.len(),
                first_bead_id = %first.id,
                first_priority = first.priority,
                first_created_at = %first.created_at,
                "Sorting {} candidates by (priority ASC, created_at ASC, id ASC)",
                candidates.len()
            );
        }
        Self::sort_candidates(&mut candidates);

        // 5. Check for split trigger: if the first candidate has accumulated
        //    enough consecutive failures, dispatch a SPLIT instruction instead
        //    of returning the bead for normal processing.
        if self.split_after_failures > 0 {
            if let Some(first_candidate) = candidates.first() {
                let failure_count = Self::extract_failure_count(first_candidate);
                tracing::debug!(
                    bead_id = %first_candidate.id,
                    failure_count = failure_count,
                    threshold = self.split_after_failures,
                    split_triggered = failure_count >= self.split_after_failures,
                    "Checking split trigger for first candidate"
                );

                if failure_count >= self.split_after_failures {
                    // Check if this bead references NEEDLE-internal configuration.
                    // Such beads have no legitimate resolution path from inside a target repo
                    // and should not be split into child beads there.
                    if detects_needle_internal_config(first_candidate) {
                        tracing::info!(
                            bead_id = %first_candidate.id,
                            title = %first_candidate.title,
                            failure_count = failure_count,
                            "Split skipped: bead references NEEDLE-internal configuration, out of scope for target workspace"
                        );
                        // Filter out this candidate and re-evaluate the remaining candidates.
                        let excluded_id = first_candidate.id.clone();
                        candidates.retain(|b| b.id != excluded_id);
                        if candidates.is_empty() {
                            // All remaining candidates were filtered out, return NoWork.
                            return StrandResult::NoWork;
                        }
                        // Continue with remaining candidates - do not trigger split.
                        // Jump to stats storage and return the next valid candidate.
                        // (Skip the rest of the split trigger logic for this iteration.)
                        return self.evaluate(store, exclusions).await;
                    }

                    tracing::info!(
                        bead_id = %first_candidate.id,
                        failure_count = failure_count,
                        threshold = self.split_after_failures,
                        "Split threshold reached, returning Split instruction"
                    );
                    return StrandResult::Split(Box::new(first_candidate.clone()), failure_count);
                }
            }
        } else {
            tracing::debug!("Split trigger disabled (threshold = 0)");
        }

        // 6. Store filtering stats for telemetry access and return result.
        // These fields persist on the strand instance for access by telemetry emission.
        self.last_open_count
            .store(stats.open_count, Ordering::Relaxed);
        self.last_excluded_count
            .store(stats.excluded_count, Ordering::Relaxed);
        *self.last_exclusion_reasons.lock().unwrap() = stats.exclusion_reasons.clone();

        if candidates.is_empty() {
            // Extract workspace path from store for telemetry.
            // All beads in a single store should have the same workspace.
            // Follow KnotStrand pattern (src/strand/knot.rs lines 224-233).
            let workspace_path = if let Ok(beads) = store.list_all().await {
                beads
                    .first()
                    .and_then(|b| b.workspace.to_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown".to_string())
            } else {
                "unknown".to_string()
            };

            // Emit PluckStarvationDetected telemetry event with filtering statistics.
            let _ = self
                .telemetry
                .emit(crate::telemetry::EventKind::PluckStarvationDetected {
                    workspace: workspace_path.clone(),
                    open_count: stats.open_count,
                    excluded_count: stats.excluded_count,
                    candidate_exclusion_reasons: stats.exclusion_reasons.clone(),
                });

            // Write persistent starvation record to NEEDLE workspace if enabled.
            if self.persistent_starvation_records {
                if let Err(e) = self.write_starvation_record(
                    &workspace_path,
                    stats.open_count,
                    stats.excluded_count,
                    &stats.exclusion_reasons,
                ) {
                    tracing::warn!(
                        error = %e,
                        "Failed to write persistent starvation record"
                    );
                }
            }

            tracing::debug!(
                workspace = %workspace_path,
                open_count = stats.open_count,
                excluded_count = stats.excluded_count,
                "Emitted PluckStarvationDetected telemetry, returning NoWork"
            );
            StrandResult::NoWork
        } else {
            let candidate_ids: Vec<&str> = candidates.iter().map(|b| b.id.as_ref()).collect();
            tracing::info!(
                count = candidates.len(),
                candidates = ?candidate_ids,
                "Returning {} candidates for processing",
                candidates.len()
            );
            StrandResult::BeadFound(candidates)
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::RepairReport;
    use crate::types::{BeadId, BeadStatus, ClaimResult};

    use anyhow::Result;
    use chrono::{TimeZone, Utc};
    use std::path::PathBuf;

    /// In-memory bead store for testing.
    struct MemoryStore {
        beads: Vec<Bead>,
    }

    #[async_trait::async_trait]
    impl BeadStore for MemoryStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.beads.clone())
        }
        async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
            let result: Vec<Bead> = self
                .beads
                .iter()
                .filter(|b| {
                    // Filter by assignee if set.
                    if let Some(ref a) = filters.assignee {
                        if b.assignee.as_ref() != Some(a) {
                            return false;
                        }
                    }
                    // Filter out beads with excluded labels.
                    if b.labels.iter().any(|l| filters.exclude_labels.contains(l)) {
                        return false;
                    }
                    true
                })
                .cloned()
                .collect();
            Ok(result)
        }

        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.beads
                .iter()
                .find(|b| b.id == *id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))
        }

        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
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

        async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
            let bead = self.show(id).await?;
            Ok(bead.labels)
        }

        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }

        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }

        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new-bead".to_string()))
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
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "claim_auto not supported in mock".to_string(),
            })
        }

        fn has_valid_store(&self) -> bool {
            true
        }
    }

    /// A store that returns all beads from `ready()` without any label filtering,
    /// simulating a backend that omits label data from its ready listing.
    struct UnfilteredStore {
        beads: Vec<Bead>,
    }

    #[async_trait::async_trait]
    impl BeadStore for UnfilteredStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.beads.clone())
        }

        /// Returns all beads regardless of filters — simulates a backend that
        /// does not apply label exclusion (e.g. br ready --json omitting labels).
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(self.beads.clone())
        }

        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.beads
                .iter()
                .find(|b| b.id == *id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))
        }

        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
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

        async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
            let bead = self.show(id).await?;
            Ok(bead.labels)
        }

        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }

        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }

        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new-bead".to_string()))
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
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "claim_auto not supported in mock".to_string(),
            })
        }

        fn has_valid_store(&self) -> bool {
            true
        }
    }

    /// Failing bead store for error-path tests.
    struct FailingStore;

    #[async_trait::async_trait]
    impl BeadStore for FailingStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            anyhow::bail!("store connection failed")
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            anyhow::bail!("store connection failed")
        }

        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("store connection failed")
        }

        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("store connection failed")
        }

        async fn release(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("store connection failed")
        }

        async fn block(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("store connection failed")
        }

        async fn flush(&self) -> Result<()> {
            Ok(())
        }

        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            anyhow::bail!("store connection failed")
        }

        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            anyhow::bail!("store connection failed")
        }

        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            anyhow::bail!("store connection failed")
        }

        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            anyhow::bail!("store connection failed")
        }

        async fn doctor_repair(&self) -> Result<RepairReport> {
            anyhow::bail!("store connection failed")
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            anyhow::bail!("store connection failed")
        }
        async fn full_rebuild(&self) -> Result<()> {
            anyhow::bail!("store connection failed")
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "claim_auto not supported in mock".to_string(),
            })
        }

        fn has_valid_store(&self) -> bool {
            true
        }
    }

    fn make_bead(id: &str, priority: u8, created_at: &str) -> Bead {
        let dt = chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S")
            .expect("bad test date");
        Bead {
            id: BeadId::from(id.to_string()),
            title: format!("Bead {id}"),
            body: None,
            priority,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec![],
            workspace: PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
            created_at: Utc.from_utc_datetime(&dt),
            updated_at: Utc.from_utc_datetime(&dt),
        }
    }

    fn make_bead_with_labels(id: &str, priority: u8, labels: Vec<&str>) -> Bead {
        let mut bead = make_bead(id, priority, "2026-01-01 00:00:00");
        bead.labels = labels.into_iter().map(|s| s.to_string()).collect();
        bead
    }

    fn make_bead_with_assignee(id: &str, assignee: &str) -> Bead {
        let mut bead = make_bead(id, 1, "2026-01-01 00:00:00");
        bead.assignee = Some(assignee.to_string());
        bead
    }

    fn make_bead_with_workspace_and_labels(
        id: &str,
        priority: u8,
        workspace: &str,
        labels: Vec<&str>,
    ) -> Bead {
        let mut bead = make_bead(id, priority, "2026-01-01 00:00:00");
        bead.workspace = PathBuf::from(workspace);
        bead.labels = labels.into_iter().map(|s| s.to_string()).collect();
        bead
    }

    fn make_bead_with_status(id: &str, priority: u8, status: BeadStatus) -> Bead {
        let mut bead = make_bead(id, priority, "2026-01-01 00:00:00");
        bead.status = status;
        bead
    }

    use super::super::Strand;

    // ──────────────────────────────────────────────────────────────────────────
    // Sorting
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn candidates_sorted_by_priority_then_created_at() {
        let store = MemoryStore {
            beads: vec![
                make_bead("low-pri", 2, "2026-01-01 00:00:00"),
                make_bead("high-pri", 1, "2026-01-02 00:00:00"),
                make_bead("high-pri-older", 1, "2026-01-01 00:00:00"),
            ],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                let ids: Vec<&str> = beads.iter().map(|b| b.id.as_ref()).collect();
                assert_eq!(ids, vec!["high-pri-older", "high-pri", "low-pri"]);
            }
            other => panic!("expected BeadFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tie_broken_by_bead_id() {
        // Same priority, same created_at — tie broken by id (lexicographic).
        let store = MemoryStore {
            beads: vec![
                make_bead("bbb", 1, "2026-01-01 00:00:00"),
                make_bead("aaa", 1, "2026-01-01 00:00:00"),
                make_bead("ccc", 1, "2026-01-01 00:00:00"),
            ],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                let ids: Vec<&str> = beads.iter().map(|b| b.id.as_ref()).collect();
                assert_eq!(ids, vec!["aaa", "bbb", "ccc"]);
            }
            other => panic!("expected BeadFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn candidates_sorted_by_failure_count_within_same_priority() {
        // ADR-012: a struggling bead must not monopolize slot 1 forever just
        // because it's the oldest ready bead at its priority. Both beads
        // share priority and created_at (make_bead_with_labels hardcodes the
        // latter) so only the failure-count component of the sort key can
        // explain the ordering.
        let store = MemoryStore {
            beads: vec![
                make_bead_with_labels("struggling", 1, vec!["failure-count:60"]),
                make_bead_with_labels("healthy", 1, vec![]),
            ],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                let ids: Vec<&str> = beads.iter().map(|b| b.id.as_ref()).collect();
                assert_eq!(
                    ids,
                    vec!["healthy", "struggling"],
                    "lower failure-count must sort ahead at the same priority"
                );
            }
            other => panic!("expected BeadFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn priority_still_wins_over_failure_count() {
        // failure_count is the second sort key, not the first — a healthy
        // low-priority-number... i.e. HIGHER-priority (lower number = more
        // urgent) struggling bead must still be preferred over a healthy
        // bead at a less urgent priority.
        let store = MemoryStore {
            beads: vec![
                // failure-count:2 stays below the default split_after_failures
                // threshold (3) — this test is about sort order, not the
                // separate split-trigger path (see split_triggered_when_failure_count_exceeds_threshold).
                make_bead_with_labels("urgent-but-struggling", 1, vec!["failure-count:2"]),
                make_bead_with_labels("healthy-but-low-priority", 2, vec![]),
            ],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                let ids: Vec<&str> = beads.iter().map(|b| b.id.as_ref()).collect();
                assert_eq!(
                    ids,
                    vec!["urgent-but-struggling", "healthy-but-low-priority"]
                );
            }
            other => panic!("expected BeadFound, got: {other:?}"),
        }
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Filtering
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn beads_with_excluded_labels_are_filtered() {
        let store = MemoryStore {
            beads: vec![
                make_bead_with_labels("deferred-bead", 1, vec!["deferred"]),
                make_bead_with_labels("human-bead", 1, vec!["human"]),
                make_bead_with_labels("blocked-bead", 1, vec!["blocked"]),
                make_bead_with_labels("normal-bead", 1, vec![]),
            ],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string())); // Uses default excludes
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                assert_eq!(beads.len(), 1);
                assert_eq!(beads[0].id.as_ref(), "normal-bead");
            }
            other => panic!("expected BeadFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn custom_exclude_labels_override_defaults() {
        let store = MemoryStore {
            beads: vec![
                make_bead_with_labels("deferred-bead", 1, vec!["deferred"]),
                make_bead_with_labels("custom-excluded", 1, vec!["wip"]),
                make_bead_with_labels("normal-bead", 1, vec![]),
            ],
        };

        // Custom excludes: only "wip" — "deferred" is NOT excluded.
        let strand = PluckStrand::new(
            vec!["wip".to_string()],
            Telemetry::new("test-worker".to_string()),
        );
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                let ids: Vec<&str> = beads.iter().map(|b| b.id.as_ref()).collect();
                assert!(ids.contains(&"deferred-bead"));
                assert!(ids.contains(&"normal-bead"));
                assert!(!ids.contains(&"custom-excluded"));
            }
            other => panic!("expected BeadFound, got: {other:?}"),
        }
    }

    // These tests use UnfilteredStore, which returns all beads from ready()
    // without applying label exclusion — simulating a backend (e.g. br ready
    // --json) that omits label data.  They verify that the strand's own
    // defensive retain catches excluded-label beads regardless of what the
    // store returns, preventing the SELECTING→CLAIMING→RETRYING spin loop.

    #[tokio::test]
    async fn strand_filters_excluded_labels_when_store_does_not() {
        let store = UnfilteredStore {
            beads: vec![
                make_bead_with_labels("deferred-bead", 1, vec!["deferred"]),
                make_bead_with_labels("normal-bead", 1, vec![]),
            ],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                assert_eq!(beads.len(), 1);
                assert_eq!(beads[0].id.as_ref(), "normal-bead");
            }
            other => panic!("expected BeadFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn all_excluded_labels_returns_no_work_via_strand_filter() {
        // When every candidate has an excluded label and the store doesn't
        // filter them, the strand's own retain must produce an empty list
        // and return NoWork — not BeadFound([]).  NoWork causes the worker
        // to move to Exhausted rather than spinning the claim-retry loop.
        let store = UnfilteredStore {
            beads: vec![
                make_bead_with_labels("deferred-1", 1, vec!["deferred"]),
                make_bead_with_labels("deferred-2", 2, vec!["deferred", "human"]),
            ],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::NoWork => {}
            other => panic!("expected NoWork, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn open_bead_with_stale_assignee_is_filtered() {
        // An open bead with a leftover assignee from a previous claim is NOT claimable.
        // The claimer would reject it every time, causing a hot loop, so we filter
        // these beads out at the pluck stage.
        let store = MemoryStore {
            beads: vec![
                make_bead_with_assignee("stale-assignee", "worker-1"),
                make_bead("unassigned", 1, "2026-01-01 00:00:00"),
            ],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                assert_eq!(
                    beads.len(),
                    1,
                    "only unassigned open beads should be claimable"
                );
                assert_eq!(beads[0].id.as_ref(), "unassigned");
            }
            other => panic!("expected BeadFound, got: {other:?}"),
        }
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Edge cases
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn empty_queue_returns_no_work() {
        let store = MemoryStore { beads: vec![] };
        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::NoWork => {}
            other => panic!("expected NoWork, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn store_error_returns_error_not_no_work() {
        let store = FailingStore;
        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::Error(StrandError::StoreError(_)) => {}
            other => panic!("expected Error(StoreError), got: {other:?}"),
        }
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Determinism property
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn same_queue_state_produces_same_ordering() {
        // Run twice with the same input and verify identical output.
        let beads = vec![
            make_bead("z-bead", 2, "2026-01-01 00:00:00"),
            make_bead("a-bead", 1, "2026-01-03 00:00:00"),
            make_bead("m-bead", 1, "2026-01-01 00:00:00"),
            make_bead("m-bead-2", 1, "2026-01-01 00:00:00"),
        ];

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));

        let store1 = MemoryStore {
            beads: beads.clone(),
        };
        let store2 = MemoryStore { beads };

        let r1 = strand.evaluate(&store1, &HashSet::new()).await;
        let r2 = strand.evaluate(&store2, &HashSet::new()).await;

        let ids1: Vec<String> = match r1 {
            StrandResult::BeadFound(b) => b.iter().map(|b| b.id.to_string()).collect(),
            _ => panic!("expected BeadFound"),
        };
        let ids2: Vec<String> = match r2 {
            StrandResult::BeadFound(b) => b.iter().map(|b| b.id.to_string()).collect(),
            _ => panic!("expected BeadFound"),
        };

        assert_eq!(ids1, ids2, "ordering must be deterministic");
        assert_eq!(ids1, vec!["m-bead", "m-bead-2", "a-bead", "z-bead"]);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Name
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn strand_name_is_pluck() {
        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        assert_eq!(strand.name(), "pluck");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Default exclude labels
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn default_exclude_labels_applied_when_empty() {
        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        assert_eq!(strand.exclude_labels, vec!["deferred", "human", "blocked"]);
    }

    #[test]
    fn custom_exclude_labels_used_when_provided() {
        let strand = PluckStrand::new(
            vec!["custom".to_string()],
            Telemetry::new("test-worker".to_string()),
        );
        assert_eq!(strand.exclude_labels, vec!["custom"]);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Split trigger tests
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn split_triggered_when_failure_count_exceeds_threshold() {
        let bead_with_failures = make_bead_with_labels("failing-bead", 1, vec!["failure-count:3"]);
        let store = MemoryStore {
            beads: vec![bead_with_failures],
        };

        let strand =
            PluckStrand::with_split_threshold(vec![], 3, Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::Split(bead, failure_count) => {
                assert_eq!(bead.id.as_ref(), "failing-bead");
                assert_eq!(failure_count, 3);
            }
            other => panic!("expected Split, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn split_not_triggered_when_failure_count_below_threshold() {
        let bead_with_failures = make_bead_with_labels("failing-bead", 1, vec!["failure-count:2"]);
        let store = MemoryStore {
            beads: vec![bead_with_failures],
        };

        let strand =
            PluckStrand::with_split_threshold(vec![], 3, Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                assert_eq!(beads[0].id.as_ref(), "failing-bead");
            }
            other => panic!("expected BeadFound, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn split_disabled_when_threshold_is_zero() {
        let bead_with_failures = make_bead_with_labels("failing-bead", 1, vec!["failure-count:10"]);
        let store = MemoryStore {
            beads: vec![bead_with_failures],
        };

        let strand =
            PluckStrand::with_split_threshold(vec![], 0, Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                assert_eq!(beads[0].id.as_ref(), "failing-bead");
            }
            other => panic!("expected BeadFound, got: {:?}", other),
        }
    }

    #[test]
    fn extract_failure_count_returns_max_when_multiple_labels() {
        let bead =
            make_bead_with_labels("multi-fail", 1, vec!["failure-count:1", "failure-count:5"]);
        assert_eq!(PluckStrand::extract_failure_count(&bead), 5);
    }

    #[test]
    fn extract_failure_count_returns_zero_when_no_label() {
        let bead = make_bead("normal", 1, "2026-01-01 00:00:00");
        assert_eq!(PluckStrand::extract_failure_count(&bead), 0);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // NEEDLE-internal config filter tests (ADR-002 Phase 6.1)
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn split_not_triggered_for_needle_internal_config_references() {
        // Regression test for ADR-002 Phase 6.1
        // Uses real bf-3b64 lineage text as fixture:
        // - "Starvation alert: beads invisible to worker" (bf-3b64 title)
        // - "bead discovery configuration" (bf-36co)
        // - "exclude_labels" (config being investigated)
        //
        // These beads reference NEEDLE's own dispatch configuration and have no
        // legitimate resolution path from inside a target repo. The split path
        // must recognize and reject them, not create child beads.

        // Bead matching bf-3b64: "Starvation alert: beads invisible to worker"
        let starvation_bead = make_bead_with_labels(
            "Starvation alert: beads invisible to worker",
            1,
            vec!["failure-count:3"],
        );

        let store = MemoryStore {
            beads: vec![starvation_bead],
        };

        let strand =
            PluckStrand::with_split_threshold(vec![], 3, Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should NOT trigger split - should return NoWork because the bead is filtered out
        match result {
            StrandResult::NoWork => {
                // Expected - bead filtered due to NEEDLE-internal config reference
            }
            other => panic!(
                "expected NoWork when bead references NEEDLE-internal config, got: {:?}",
                other
            ),
        }
    }

    #[tokio::test]
    async fn split_not_triggered_for_pluck_config_beads() {
        // Regression test for "Pluck configuration" and "exclude_labels" references
        let config_bead = make_bead_with_labels(
            "Fix bead discovery configuration",
            1,
            vec!["failure-count:3"],
        );

        let store = MemoryStore {
            beads: vec![config_bead],
        };

        let strand =
            PluckStrand::with_split_threshold(vec![], 3, Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should NOT trigger split
        match result {
            StrandResult::NoWork => {
                // Expected - bead filtered due to NEEDLE-internal config reference
            }
            other => panic!("expected NoWork for Pluck config bead, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn split_triggered_for_normal_failing_beads() {
        // Verify that normal beads (not referencing NEEDLE-internal config)
        // still trigger split correctly after the filter is in place.

        let normal_bead =
            make_bead_with_labels("Add authentication endpoint", 1, vec!["failure-count:3"]);

        let store = MemoryStore {
            beads: vec![normal_bead],
        };

        let strand =
            PluckStrand::with_split_threshold(vec![], 3, Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should trigger split normally for non-NEEDLE-internal beads
        match result {
            StrandResult::Split(bead, failure_count) => {
                assert_eq!(bead.id.as_ref(), "Add authentication endpoint");
                assert_eq!(failure_count, 3);
            }
            other => panic!("expected Split for normal failing bead, got: {:?}", other),
        }
    }

    // ──────────────────────────────────────────────────────────────────────────
    // PluckStarvationDetected telemetry tests
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn starvation_when_all_beads_excluded_by_labels_emits_telemetry() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        // Use UnfilteredStore to test the strand's own filtering logic
        let store = UnfilteredStore {
            beads: vec![
                make_bead_with_labels("deferred-bead", 1, vec!["deferred"]),
                make_bead_with_labels("human-bead", 2, vec!["human"]),
                make_bead_with_labels("blocked-bead", 3, vec!["blocked"]),
            ],
        };

        let strand = PluckStrand::new(vec![], helper.telemetry().clone());

        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork since all beads are excluded
        match result {
            StrandResult::NoWork => {
                // Expected - all beads excluded
            }
            other => panic!("expected NoWork when all beads excluded, got: {other:?}"),
        }

        // Wait for telemetry events to be flushed
        helper.sync().await;

        // Verify starvation event was emitted
        helper.assert_event_emitted("strand.pluck.starvation_detected");

        // Get the starvation event and verify its contents
        let starvation_events = helper.events_by_type("strand.pluck.starvation_detected");
        assert_eq!(
            starvation_events.len(),
            1,
            "should emit exactly one starvation event"
        );

        let event = &starvation_events[0];

        // Verify workspace - should be "/tmp/test" since beads exist
        if let Some(workspace) = event.data.get("workspace") {
            assert_eq!(workspace.as_str(), Some("/tmp/test"));
        } else {
            panic!("workspace field missing from starvation event");
        }

        // Verify open_count - should be 3 (all beads returned by UnfilteredStore)
        if let Some(open_count) = event.data.get("open_count") {
            assert_eq!(open_count.as_u64(), Some(3));
        } else {
            panic!("open_count field missing from starvation event");
        }

        // Verify excluded_count - should be 3 (all excluded by strand's filtering)
        if let Some(excluded_count) = event.data.get("excluded_count") {
            assert_eq!(excluded_count.as_u64(), Some(3));
        } else {
            panic!("excluded_count field missing from starvation event");
        }

        // Verify exclusion reasons - should include label:reason for each bead
        if let Some(reasons) = event.data.get("candidate_exclusion_reasons") {
            if let Some(reasons_array) = reasons.as_array() {
                assert_eq!(reasons_array.len(), 3, "should have 3 exclusion reasons");

                let reason_strings: Vec<&str> =
                    reasons_array.iter().filter_map(|r| r.as_str()).collect();

                // Should exclude all 3 beads by label
                assert!(reason_strings.contains(&"label:deferred"));
                assert!(reason_strings.contains(&"label:human"));
                assert!(reason_strings.contains(&"label:blocked"));
            } else {
                panic!("candidate_exclusion_reasons should be an array");
            }
        } else {
            panic!("candidate_exclusion_reasons field missing from starvation event");
        }
    }

    #[tokio::test]
    async fn starvation_when_all_beads_have_stale_assignees_emits_telemetry() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        let store = MemoryStore {
            beads: vec![
                make_bead_with_assignee("stale-worker-1", "worker-1"),
                make_bead_with_assignee("stale-worker-2", "worker-2"),
                make_bead_with_assignee("stale-worker-3", "worker-3"),
            ],
        };

        let strand = PluckStrand::new(vec![], helper.telemetry().clone());

        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork since all beads have stale assignees
        match result {
            StrandResult::NoWork => {
                // Expected - all beads have stale assignees
            }
            other => panic!("expected NoWork when all beads have stale assignees, got: {other:?}"),
        }

        // Wait for telemetry events to be flushed
        helper.sync().await;

        // Verify starvation event was emitted
        helper.assert_event_emitted("strand.pluck.starvation_detected");

        // Get the starvation event and verify its contents
        let starvation_events = helper.events_by_type("strand.pluck.starvation_detected");
        assert_eq!(
            starvation_events.len(),
            1,
            "should emit exactly one starvation event"
        );

        let event = &starvation_events[0];

        // Verify open_count - should be 3 (all beads are open but assigned)
        if let Some(open_count) = event.data.get("open_count") {
            assert_eq!(open_count.as_u64(), Some(3));
        } else {
            panic!("open_count field missing from starvation event");
        }

        // Verify excluded_count - should be 3 (all excluded due to stale assignees)
        if let Some(excluded_count) = event.data.get("excluded_count") {
            assert_eq!(excluded_count.as_u64(), Some(3));
        } else {
            panic!("excluded_count field missing from starvation event");
        }

        // Verify exclusion reasons - should include assignee:worker_id for each
        if let Some(reasons) = event.data.get("candidate_exclusion_reasons") {
            if let Some(reasons_array) = reasons.as_array() {
                assert_eq!(reasons_array.len(), 3, "should have 3 exclusion reasons");

                let reason_strings: Vec<&str> =
                    reasons_array.iter().filter_map(|r| r.as_str()).collect();

                // Should exclude all 3 beads by stale assignee
                assert!(reason_strings.contains(&"assignee:worker-1"));
                assert!(reason_strings.contains(&"assignee:worker-2"));
                assert!(reason_strings.contains(&"assignee:worker-3"));
            } else {
                panic!("candidate_exclusion_reasons should be an array");
            }
        } else {
            panic!("candidate_exclusion_reasons field missing from starvation event");
        }
    }

    #[tokio::test]
    async fn starvation_when_all_beads_in_progress_emits_telemetry() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        // All beads are InProgress - being processed by other workers (no executor available)
        let store = MemoryStore {
            beads: vec![
                make_bead_with_status("in-progress-1", 1, BeadStatus::InProgress),
                make_bead_with_status("in-progress-2", 2, BeadStatus::InProgress),
                make_bead_with_status("in-progress-3", 3, BeadStatus::InProgress),
            ],
        };

        let strand = PluckStrand::new(vec![], helper.telemetry().clone());

        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork since all beads are InProgress
        match result {
            StrandResult::NoWork => {
                // Expected - all beads are InProgress
            }
            other => panic!("expected NoWork when all beads are InProgress, got: {other:?}"),
        }

        // Wait for telemetry events to be flushed
        helper.sync().await;

        // Verify starvation event was emitted
        helper.assert_event_emitted("strand.pluck.starvation_detected");

        // Get the starvation event and verify its contents
        let starvation_events = helper.events_by_type("strand.pluck.starvation_detected");
        assert_eq!(
            starvation_events.len(),
            1,
            "should emit exactly one starvation event"
        );

        let event = &starvation_events[0];

        // Verify workspace - should be "/tmp/test"
        if let Some(workspace) = event.data.get("workspace") {
            assert_eq!(workspace.as_str(), Some("/tmp/test"));
        } else {
            panic!("workspace field missing from starvation event");
        }

        // Verify open_count - should be 3 (all beads returned by store)
        if let Some(open_count) = event.data.get("open_count") {
            assert_eq!(open_count.as_u64(), Some(3));
        } else {
            panic!("open_count field missing from starvation event");
        }

        // Verify excluded_count - should be 3 (all excluded due to InProgress status)
        if let Some(excluded_count) = event.data.get("excluded_count") {
            assert_eq!(excluded_count.as_u64(), Some(3));
        } else {
            panic!("excluded_count field missing from starvation event");
        }

        // Verify exclusion reasons - should include status:in_progress for each bead
        if let Some(reasons) = event.data.get("candidate_exclusion_reasons") {
            if let Some(reasons_array) = reasons.as_array() {
                assert_eq!(reasons_array.len(), 3, "should have 3 exclusion reasons");

                let reason_strings: Vec<&str> =
                    reasons_array.iter().filter_map(|r| r.as_str()).collect();

                // Should exclude all 3 beads by InProgress status
                assert_eq!(
                    reason_strings.iter().filter(|r| **r == "status:in_progress").count(),
                    3,
                    "all 3 beads should be excluded with status:in_progress reason"
                );
            } else {
                panic!("candidate_exclusion_reasons should be an array");
            }
        } else {
            panic!("candidate_exclusion_reasons field missing from starvation event");
        }
    }

    #[tokio::test]
    async fn starvation_when_queue_is_genuinely_empty_emits_telemetry() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        // Empty store - no beads at all
        let store = MemoryStore { beads: vec![] };

        let strand = PluckStrand::new(vec![], helper.telemetry().clone());

        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork since queue is empty
        match result {
            StrandResult::NoWork => {
                // Expected - queue is empty
            }
            other => panic!("expected NoWork when queue is empty, got: {other:?}"),
        }

        // Wait for telemetry events to be flushed
        helper.sync().await;

        // Verify starvation event was emitted
        helper.assert_event_emitted("strand.pluck.starvation_detected");

        // Get the starvation event and verify its contents
        let starvation_events = helper.events_by_type("strand.pluck.starvation_detected");
        assert_eq!(
            starvation_events.len(),
            1,
            "should emit exactly one starvation event"
        );

        let event = &starvation_events[0];

        // Verify workspace - should be "unknown" since no beads exist
        if let Some(workspace) = event.data.get("workspace") {
            assert_eq!(workspace.as_str(), Some("unknown"));
        } else {
            panic!("workspace field missing from starvation event");
        }

        // Verify open_count - should be 0 (no beads)
        if let Some(open_count) = event.data.get("open_count") {
            assert_eq!(open_count.as_u64(), Some(0));
        } else {
            panic!("open_count field missing from starvation event");
        }

        // Verify excluded_count - should be 0 (nothing to exclude)
        if let Some(excluded_count) = event.data.get("excluded_count") {
            assert_eq!(excluded_count.as_u64(), Some(0));
        } else {
            panic!("excluded_count field missing from starvation event");
        }

        // Verify exclusion reasons - should be empty (no beads to exclude)
        if let Some(reasons) = event.data.get("candidate_exclusion_reasons") {
            if let Some(reasons_array) = reasons.as_array() {
                assert_eq!(reasons_array.len(), 0, "should have no exclusion reasons");
            } else {
                panic!("candidate_exclusion_reasons should be an array");
            }
        } else {
            panic!("candidate_exclusion_reasons field missing from starvation event");
        }
    }

    #[tokio::test]
    async fn starvation_mixed_label_and_assignee_exclusions_emits_telemetry() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        // Use UnfilteredStore to test the strand's own filtering logic
        let store = UnfilteredStore {
            beads: vec![
                make_bead_with_labels("deferred-bead", 1, vec!["deferred"]),
                make_bead_with_assignee("stale-assignee", "worker-1"),
                make_bead_with_labels("blocked-bead", 3, vec!["blocked"]),
            ],
        };

        let strand = PluckStrand::new(vec![], helper.telemetry().clone());

        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork since all beads are excluded
        match result {
            StrandResult::NoWork => {
                // Expected - all beads excluded
            }
            other => panic!("expected NoWork when all beads excluded, got: {other:?}"),
        }

        // Wait for telemetry events to be flushed
        helper.sync().await;

        // Verify starvation event was emitted
        helper.assert_event_emitted("strand.pluck.starvation_detected");

        // Get the starvation event and verify its contents
        let starvation_events = helper.events_by_type("strand.pluck.starvation_detected");
        assert_eq!(
            starvation_events.len(),
            1,
            "should emit exactly one starvation event"
        );

        let event = &starvation_events[0];

        // Verify counts
        if let Some(open_count) = event.data.get("open_count") {
            assert_eq!(open_count.as_u64(), Some(3));
        } else {
            panic!("open_count field missing from starvation event");
        }

        if let Some(excluded_count) = event.data.get("excluded_count") {
            assert_eq!(excluded_count.as_u64(), Some(3));
        } else {
            panic!("excluded_count field missing from starvation event");
        }

        // Verify both label and assignee exclusion reasons are present
        if let Some(reasons) = event.data.get("candidate_exclusion_reasons") {
            if let Some(reasons_array) = reasons.as_array() {
                assert_eq!(reasons_array.len(), 3, "should have 3 exclusion reasons");

                let reason_strings: Vec<&str> =
                    reasons_array.iter().filter_map(|r| r.as_str()).collect();

                // Should include both label and assignee exclusions
                assert!(reason_strings.contains(&"label:deferred"));
                assert!(reason_strings.contains(&"assignee:worker-1"));
                assert!(reason_strings.contains(&"label:blocked"));
            } else {
                panic!("candidate_exclusion_reasons should be an array");
            }
        } else {
            panic!("candidate_exclusion_reasons field missing from starvation event");
        }
    }

    #[tokio::test]
    async fn starvation_persistent_record_written_to_needle_workspace() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        // Create a temporary directory to act as the NEEDLE workspace
        let needle_workspace = tempfile::tempdir().unwrap();
        let needle_workspace_path = needle_workspace.path();

        // Use UnfilteredStore to test the strand's own filtering logic
        let store = UnfilteredStore {
            beads: vec![
                make_bead_with_labels("deferred-bead", 1, vec!["deferred"]),
                make_bead_with_labels("human-bead", 2, vec!["human"]),
                make_bead_with_labels("blocked-bead", 3, vec!["blocked"]),
            ],
        };

        let strand = PluckStrand::with_persistent_records(
            vec![],
            3,
            helper.telemetry().clone(),
            needle_workspace_path.to_path_buf(),
            true, // Enable persistent records
        );

        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork since all beads are excluded
        match result {
            StrandResult::NoWork => {
                // Expected - all beads excluded
            }
            other => panic!("expected NoWork when all beads excluded, got: {other:?}"),
        }

        // Verify that a persistent record was written to the NEEDLE workspace
        let state_dir = needle_workspace_path.join("state");
        let record_path = state_dir.join("starvation-records.jsonl");

        assert!(
            record_path.exists(),
            "starvation record should exist at {:?}",
            record_path
        );

        // Read and verify the record content
        let record_content = std::fs::read_to_string(&record_path)
            .expect("should be able to read starvation record file");

        // Parse the JSONL record
        let record: serde_json::Value =
            serde_json::from_str(&record_content).expect("starvation record should be valid JSON");

        // Verify record structure
        assert!(
            record.get("timestamp").is_some(),
            "record should have timestamp"
        );
        assert!(
            record.get("target_workspace").is_some(),
            "record should have target_workspace"
        );
        assert!(
            record.get("open_count").is_some(),
            "record should have open_count"
        );
        assert!(
            record.get("excluded_count").is_some(),
            "record should have excluded_count"
        );
        assert!(
            record.get("exclusion_reasons").is_some(),
            "record should have exclusion_reasons"
        );

        // Verify the target_workspace field contains the workspace being processed
        // (not the NEEDLE workspace itself)
        let target_workspace = record["target_workspace"].as_str().unwrap();
        assert_eq!(
            target_workspace, "/tmp/test",
            "target_workspace should be the processed workspace"
        );

        // Verify counts
        let open_count = record["open_count"].as_u64().unwrap();
        let excluded_count = record["excluded_count"].as_u64().unwrap();
        assert_eq!(open_count, 3, "open_count should be 3");
        assert_eq!(excluded_count, 3, "excluded_count should be 3");

        // Verify exclusion reasons
        let exclusion_reasons = record["exclusion_reasons"].as_array().unwrap();
        assert_eq!(
            exclusion_reasons.len(),
            3,
            "should have 3 exclusion reasons"
        );
    }

    #[tokio::test]
    async fn starvation_persistent_record_disabled_when_flag_false() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        // Create a temporary directory to act as the NEEDLE workspace
        let needle_workspace = tempfile::tempdir().unwrap();
        let needle_workspace_path = needle_workspace.path();

        // Use UnfilteredStore to test the strand's own filtering logic
        let store = UnfilteredStore {
            beads: vec![make_bead_with_labels("deferred-bead", 1, vec!["deferred"])],
        };

        let strand = PluckStrand::with_persistent_records(
            vec![],
            3,
            helper.telemetry().clone(),
            needle_workspace_path.to_path_buf(),
            false, // Disable persistent records
        );

        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork since all beads are excluded
        match result {
            StrandResult::NoWork => {
                // Expected - all beads excluded
            }
            other => panic!("expected NoWork when all beads excluded, got: {other:?}"),
        }

        // Verify that NO persistent record was written
        let state_dir = needle_workspace_path.join("state");
        let record_path = state_dir.join("starvation-records.jsonl");

        assert!(
            !record_path.exists(),
            "starvation record should NOT exist when persistent records are disabled"
        );
    }

    #[tokio::test]
    async fn starvation_persistent_record_not_written_to_target_workspace() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        // Create a temporary directory to act as the NEEDLE workspace
        let needle_workspace = tempfile::tempdir().unwrap();
        let needle_workspace_path = needle_workspace.path();

        // Create a temporary directory to act as the TARGET workspace
        let target_workspace = tempfile::tempdir().unwrap();
        let target_workspace_path = target_workspace.path();

        // Create beads with the target workspace path
        let store = UnfilteredStore {
            beads: vec![make_bead_with_workspace_and_labels(
                "deferred-bead",
                1,
                target_workspace_path.to_str().unwrap(),
                vec!["deferred"],
            )],
        };

        let strand = PluckStrand::with_persistent_records(
            vec![],
            3,
            helper.telemetry().clone(),
            needle_workspace_path.to_path_buf(),
            true, // Enable persistent records
        );

        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork since all beads are excluded
        match result {
            StrandResult::NoWork => {
                // Expected - all beads excluded
            }
            other => panic!("expected NoWork when all beads excluded, got: {other:?}"),
        }

        // Verify record was written to NEEDLE workspace
        let needle_state_dir = needle_workspace_path.join("state");
        let needle_record_path = needle_state_dir.join("starvation-records.jsonl");

        assert!(
            needle_record_path.exists(),
            "starvation record should exist in NEEDLE workspace"
        );

        // Verify record was NOT written to target workspace
        let target_state_dir = target_workspace_path.join("state");
        let target_record_path = target_state_dir.join("starvation-records.jsonl");

        assert!(
            !target_record_path.exists(),
            "starvation record should NOT exist in target workspace"
        );

        // Verify the record contains the target workspace path in its content
        let record_content = std::fs::read_to_string(&needle_record_path)
            .expect("should be able to read starvation record file");

        let record: serde_json::Value =
            serde_json::from_str(&record_content).expect("starvation record should be valid JSON");

        // The record should mention the target workspace in its data
        let target_workspace_field = record["target_workspace"].as_str().unwrap();
        assert!(
            target_workspace_field.contains(target_workspace_path.to_str().unwrap()),
            "record should reference the target workspace, not the NEEDLE workspace"
        );
    }

    // ─── Starvation Scenario Tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn starvation_when_all_beads_excluded_by_labels() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        // Create a workspace with beads that all have excluded labels
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = workspace.path().to_str().unwrap();

        // Use UnfilteredStore to bypass store-level label filtering, testing the strand's filtering logic
        let store = UnfilteredStore {
            beads: vec![
                make_bead_with_workspace_and_labels("deferred-1", 1, workspace_path, vec!["deferred"]),
                make_bead_with_workspace_and_labels("human-1", 2, workspace_path, vec!["human"]),
                make_bead_with_workspace_and_labels("blocked-1", 3, workspace_path, vec!["blocked"]),
            ],
        };

        let strand = PluckStrand::new(vec![], helper.telemetry().clone());
        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork since all beads are excluded by labels
        match result {
            StrandResult::NoWork => {
                // Expected - all beads excluded by labels
            }
            other => panic!("expected NoWork when all beads excluded by labels, got: {other:?}"),
        }

        helper.sync().await;

        // Verify PluckStarvationDetected event was emitted
        helper.assert_event_emitted("strand.pluck.starvation_detected");

        // Verify the event contains correct starvation data
        let starvation_event = helper.find_event("strand.pluck.starvation_detected").unwrap();
        assert_eq!(starvation_event.data["open_count"], 3);
        assert_eq!(starvation_event.data["excluded_count"], 3);

        // Verify exclusion reasons contain label exclusions
        let reasons = starvation_event.data["candidate_exclusion_reasons"]
            .as_array()
            .expect("exclusion reasons should be an array");
        assert!(!reasons.is_empty(), "should have exclusion reasons");

        // Verify all reasons are label-based
        for reason in reasons {
            let reason_str = reason.as_str().unwrap();
            assert!(
                reason_str.starts_with("label:"),
                "exclusion reason should start with 'label:': {}",
                reason_str
            );
        }

        // Verify workspace field is set correctly
        assert!(starvation_event.data["workspace"].is_string());
    }

    #[tokio::test]
    async fn starvation_when_all_beads_have_stale_assignees() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        // Create a workspace with beads that all have stale assignees
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = workspace.path().to_str().unwrap();

        let mut bead1 = make_bead_with_workspace_and_labels("assigned-1", 1, workspace_path, vec![]);
        bead1.assignee = Some("worker-old-1".to_string());
        bead1.status = BeadStatus::Open;

        let mut bead2 = make_bead_with_workspace_and_labels("assigned-2", 2, workspace_path, vec![]);
        bead2.assignee = Some("worker-old-2".to_string());
        bead2.status = BeadStatus::Open;

        let mut bead3 = make_bead_with_workspace_and_labels("in-progress-1", 3, workspace_path, vec![]);
        bead3.status = BeadStatus::InProgress;
        bead3.assignee = Some("worker-active".to_string());

        // Use UnfilteredStore to bypass store-level label filtering
        let store = UnfilteredStore {
            beads: vec![bead1, bead2, bead3],
        };

        let strand = PluckStrand::new(vec![], helper.telemetry().clone());
        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork since all beads have stale assignees or are in progress
        match result {
            StrandResult::NoWork => {
                // Expected - all beads excluded by stale assignees or in-progress status
            }
            other => panic!("expected NoWork when all beads have stale assignees, got: {other:?}"),
        }

        helper.sync().await;

        // Verify PluckStarvationDetected event was emitted
        helper.assert_event_emitted("strand.pluck.starvation_detected");

        // Verify the event contains correct starvation data
        let starvation_event = helper.find_event("strand.pluck.starvation_detected").unwrap();
        assert_eq!(starvation_event.data["open_count"], 3);
        assert_eq!(starvation_event.data["excluded_count"], 3);

        // Verify exclusion reasons contain assignee and status exclusions
        let reasons = starvation_event.data["candidate_exclusion_reasons"]
            .as_array()
            .expect("exclusion reasons should be an array");
        assert!(!reasons.is_empty(), "should have exclusion reasons");

        // Verify reasons are either assignee-based or status-based
        for reason in reasons {
            let reason_str = reason.as_str().unwrap();
            assert!(
                reason_str.starts_with("assignee:") || reason_str.starts_with("status:"),
                "exclusion reason should start with 'assignee:' or 'status:': {}",
                reason_str
            );
        }

        // Verify workspace field is set correctly
        assert!(starvation_event.data["workspace"].is_string());
    }

    #[tokio::test]
    async fn starvation_when_queue_is_genuinely_empty() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        // Create a workspace with an empty queue (no beads at all)
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = workspace.path().to_str().unwrap();

        // Use UnfilteredStore to bypass store-level label filtering
        let store = UnfilteredStore {
            beads: vec![],
        };

        let strand = PluckStrand::new(vec![], helper.telemetry().clone());
        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork since the queue is empty
        match result {
            StrandResult::NoWork => {
                // Expected - queue is empty
            }
            other => panic!("expected NoWork when queue is empty, got: {other:?}"),
        }

        helper.sync().await;

        // Verify PluckStarvationDetected event was emitted
        helper.assert_event_emitted("strand.pluck.starvation_detected");

        // Verify the event contains correct starvation data (all zeros)
        let starvation_event = helper.find_event("strand.pluck.starvation_detected").unwrap();
        assert_eq!(starvation_event.data["open_count"], 0);
        assert_eq!(starvation_event.data["excluded_count"], 0);

        // Verify exclusion reasons is an empty array
        let reasons = starvation_event.data["candidate_exclusion_reasons"]
            .as_array()
            .expect("exclusion reasons should be an array");
        assert!(
            reasons.is_empty(),
            "exclusion reasons should be empty when queue is genuinely empty"
        );

        // Verify workspace field is set (even if empty, it should be present)
        assert!(starvation_event.data["workspace"].is_string());
    }

    #[tokio::test]
    async fn starvation_emits_no_workspace_modifications() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");

        // Create a workspace to monitor for modifications
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = workspace.path().to_str().unwrap();

        // Record initial state
        let initial_files = std::fs::read_dir(workspace.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();

        // Use UnfilteredStore to bypass store-level label filtering
        let store = UnfilteredStore {
            beads: vec![make_bead_with_workspace_and_labels(
                "deferred-bead",
                1,
                workspace_path,
                vec!["deferred"],
            )],
        };

        let strand = PluckStrand::new(vec![], helper.telemetry().clone());
        let _result = strand.evaluate(&store, &HashSet::new()).await;

        helper.sync().await;

        // Verify no files were created in the workspace
        let final_files = std::fs::read_dir(workspace.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();

        assert_eq!(
            initial_files.len(),
            final_files.len(),
            "starvation detection should not create files in workspace"
        );

        // Verify no state directory was created in the workspace
        let state_dir = workspace.path().join("state");
        assert!(
            !state_dir.exists(),
            "state directory should not exist in workspace after starvation"
        );

        // Verify telemetry was still emitted (event went to telemetry, not workspace)
        helper.assert_event_emitted("strand.pluck.starvation_detected");
    }
}
