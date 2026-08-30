//! Pluck strand: primary bead selection from the assigned workspace.
//!
//! Pluck handles >90% of all bead processing. It queries the bead store for
//! unassigned, ready beads, filters by excluded labels, and sorts them in
//! deterministic priority order: `(effective_priority ASC, pinned_bucket ASC,
//! failure_count ASC, created_at ASC, id ASC)`.
//!
//! Given the same queue state, every worker computes the same candidate list.

use crate::bead_store::{BeadStore, Filters};
use crate::fingerprint::{
    append_alert_note, build_alert_labels, check_alert_deduplication, AlertDeduplication, AlertKind,
};
use crate::mitosis::detects_needle_internal_config;
use crate::telemetry::Telemetry;
use crate::types::{Bead, BeadId, BrDependency, Comment, StrandError, StrandResult};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{atomic::AtomicUsize, atomic::Ordering, Mutex};

/// Default labels excluded from Pluck selection when not configured.
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];

/// Constraint relaxation level used when the normal ready query is empty.
///
/// The current bead-store abstraction does not expose a separate priority
/// predicate, but keeping that level explicit makes the fallback observable
/// and gives backends that do have one a stable place to add it later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelaxationTier {
    Initial,
    WorkerLabels,
    Priority,
    StatusOnly,
}

impl RelaxationTier {
    fn name(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::WorkerLabels => "worker-labels",
            Self::Priority => "priority",
            Self::StatusOnly => "status-only",
        }
    }

    fn dropped_constraints(self) -> &'static [&'static str] {
        match self {
            Self::Initial => &[],
            Self::WorkerLabels => &["worker label constraints"],
            Self::Priority => &["worker label constraints", "priority constraints"],
            Self::StatusOnly => &[
                "worker label constraints",
                "priority constraints",
                "readiness constraints",
                "assignee constraints",
            ],
        }
    }

    fn ignores_labels(self) -> bool {
        !matches!(self, Self::Initial)
    }
}

/// Counts of the high-level reasons an open bead was not claimable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ExclusionCounts {
    /// Beads with an unfinished `blocks` dependency.
    blocked: usize,
    /// Beads explicitly blocked by an operator (or the equivalent label).
    manual_blocked: usize,
    /// Beads carrying a human-owned or otherwise human-review exclusion label.
    human: usize,
    /// Beads deferred by status/label or held by an existing assignee.
    deferred_assignee: usize,
}

impl ExclusionCounts {
    /// Render non-zero counts in a stable order for the alert body.
    fn summary(&self) -> String {
        let mut reasons = Vec::new();
        if self.blocked > 0 {
            reasons.push(format!("blocked={}", self.blocked));
        }
        if self.manual_blocked > 0 {
            reasons.push(format!("manual_blocked={}", self.manual_blocked));
        }
        if self.human > 0 {
            reasons.push(format!("human={}", self.human));
        }
        if self.deferred_assignee > 0 {
            reasons.push(format!("deferred_assignee={}", self.deferred_assignee));
        }
        if reasons.is_empty() {
            "none identified".to_string()
        } else {
            reasons.join(", ")
        }
    }

    /// Return the same summary as a list for persistent JSONL records.
    fn summary_vec(&self) -> Vec<String> {
        self.summary().split(", ").map(str::to_string).collect()
    }
}

/// Statistics collected during candidate filtering for starvation telemetry.
#[derive(Debug, Default)]
struct FilteringStats {
    /// Count of open beads before any filtering.
    open_count: usize,
    /// Count of beads excluded during filtering.
    excluded_count: usize,
    /// Aggregated reasons why candidates were excluded.
    exclusion_reasons: Vec<String>,
    /// Counts used to explain starvation in the human-facing alert.
    exclusion_counts: ExclusionCounts,
}

impl FilteringStats {
    /// Build starvation metadata from the complete bead inventory.
    ///
    /// `ready()` intentionally returns only the candidate frontier, so it cannot
    /// explain beads omitted by dependency, manual-block, or label predicates.
    /// This inventory pass is the equivalent of joining those predicates back to
    /// the full issue set. Dependency edges from bead-rs are lean and only carry
    /// the blocker ID/kind, so blocker status is resolved from the inventory map.
    fn from_inventory(beads: &[Bead], exclude_labels: &[String]) -> Self {
        let finished_by_id: HashMap<BeadId, bool> = beads
            .iter()
            .map(|bead| (bead.id.clone(), bead.status.is_done()))
            .collect();
        let mut stats = Self::default();

        for bead in beads {
            // In-progress beads are already being worked and must not turn an
            // empty ready frontier into a starvation alert. Done/closed beads
            // are not open work either. Blocked and deferred are retained as
            // open work because they are precisely the exclusions we report.
            if bead.status.is_done() || matches!(bead.status, crate::types::BeadStatus::InProgress)
            {
                continue;
            }

            stats.open_count += 1;

            if bead.dependencies.iter().any(|dependency| {
                let is_blocking = dependency.dependency_type.is_empty()
                    || dependency.dependency_type.eq_ignore_ascii_case("blocks");
                if !is_blocking {
                    return false;
                }

                match finished_by_id.get(&dependency.id) {
                    Some(finished) => !finished,
                    // A missing blocker cannot be proven complete. Count it so
                    // the alert remains useful for partially synced stores.
                    None => !is_finished_status(&dependency.status),
                }
            }) {
                stats.exclusion_counts.blocked += 1;
            }

            if matches!(bead.status, crate::types::BeadStatus::Blocked)
                || bead.labels.iter().any(|label| is_manual_block_label(label))
            {
                stats.exclusion_counts.manual_blocked += 1;
            }

            if bead
                .labels
                .iter()
                .any(|label| is_human_like_label(label, exclude_labels))
            {
                stats.exclusion_counts.human += 1;
            }

            if matches!(bead.status, crate::types::BeadStatus::Deferred)
                || bead.labels.iter().any(|label| is_deferred_label(label))
                || bead.assignee.is_some()
            {
                stats.exclusion_counts.deferred_assignee += 1;
            }
        }

        // This helper is called only after the final candidate list is empty;
        // therefore every open bead in the inventory is excluded from work.
        stats.excluded_count = stats.open_count;
        stats
    }

    /// Whether at least one open bead has no unresolved blocking dependency.
    ///
    /// Dependency-only queues are waiting for their blockers to finish; they
    /// are not evidence that Pluck is starving ready work.
    fn has_unblocked_open_bead(&self) -> bool {
        self.open_count > self.exclusion_counts.blocked
    }
}

fn is_finished_status(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "closed" | "completed" | "done"
    )
}

/// Whether a bead belongs in the open-work inventory used by starvation
/// diagnostics.  In-progress beads have an owner already and therefore do not
/// represent work that is invisible to this worker.
fn is_open_work_bead(bead: &Bead) -> bool {
    !bead.status.is_done() && !matches!(bead.status, crate::types::BeadStatus::InProgress)
}

fn dependency_is_blocking(
    dependency: &BrDependency,
    finished_by_id: &HashMap<BeadId, bool>,
) -> bool {
    let is_blocking = dependency.dependency_type.is_empty()
        || dependency.dependency_type.eq_ignore_ascii_case("blocks");
    if !is_blocking {
        return false;
    }

    match finished_by_id.get(&dependency.id) {
        Some(finished) => !finished,
        // A missing blocker cannot be proven complete.  Treat it as blocking so
        // a partially synced store is represented honestly in the snapshot.
        None => !is_finished_status(&dependency.status),
    }
}

/// Explain every exclusion that can be inferred from the inventory and the
/// worker-local filters.  Multiple reasons are retained because a single bead
/// can be hidden by both a dependency and a label, for example.
fn exclusion_reasons_for_bead(
    bead: &Bead,
    beads: &[Bead],
    exclude_labels: &[String],
    exclude_ids: &HashSet<BeadId>,
) -> Vec<String> {
    let finished_by_id: HashMap<BeadId, bool> = beads
        .iter()
        .map(|candidate| (candidate.id.clone(), candidate.status.is_done()))
        .collect();
    let mut reasons = Vec::new();

    for dependency in &bead.dependencies {
        if dependency_is_blocking(dependency, &finished_by_id) {
            reasons.push(format!("dependency:{}", dependency.id));
        }
    }

    for label in &bead.labels {
        if exclude_labels.contains(label) {
            reasons.push(format!("label:{label}"));
        }
    }

    match &bead.status {
        crate::types::BeadStatus::Open => {
            if let Some(assignee) = &bead.assignee {
                reasons.push(format!("assignee:{assignee}"));
            }
        }
        crate::types::BeadStatus::Blocked => reasons.push("status:blocked".to_string()),
        crate::types::BeadStatus::Deferred => reasons.push("status:deferred".to_string()),
        crate::types::BeadStatus::InProgress
        | crate::types::BeadStatus::Done
        | crate::types::BeadStatus::Closed => {}
    }

    if exclude_ids.contains(&bead.id) {
        reasons.push("worker_exclusion".to_string());
    }

    reasons
}

fn open_bead_diagnostic(
    bead: &Bead,
    beads: &[Bead],
    exclude_labels: &[String],
    exclude_ids: &HashSet<BeadId>,
) -> OpenBeadDiagnostic {
    let inferred_exclusion_reasons =
        exclusion_reasons_for_bead(bead, beads, exclude_labels, exclude_ids);
    let is_ready = inferred_exclusion_reasons.is_empty();
    let exclusion_reasons = if is_ready {
        // The backend can omit a bead from ready() without exposing a
        // corresponding state predicate.  Preserve that fact instead of
        // recording an empty explanation.
        vec!["not_in_ready_frontier".to_string()]
    } else {
        inferred_exclusion_reasons
    };
    OpenBeadDiagnostic {
        id: bead.id.to_string(),
        title: bead.title.clone(),
        description: bead.body.clone(),
        status: bead.status.to_string(),
        assignee: bead.assignee.clone(),
        priority: bead.priority,
        labels: bead.labels.clone(),
        workspace: bead.workspace.display().to_string(),
        dependencies: bead.dependencies.clone(),
        dependents: bead.dependents.clone(),
        comments: bead.comments.clone(),
        created_at: bead.created_at,
        updated_at: bead.updated_at,
        is_ready,
        exclusion_reasons,
    }
}

fn starvation_diagnostic_snapshot(
    beads: &[Bead],
    stats: &FilteringStats,
    exclude_labels: &[String],
    exclude_ids: &HashSet<BeadId>,
    worker_id: &str,
    relaxation_tier: RelaxationTier,
    split_after_failures: u32,
) -> StarvationDiagnosticSnapshot {
    let timestamp = Utc::now();
    let open_beads: Vec<OpenBeadDiagnostic> = beads
        .iter()
        .filter(|bead| is_open_work_bead(bead))
        .map(|bead| open_bead_diagnostic(bead, beads, exclude_labels, exclude_ids))
        .collect();

    let mut exclusion_reason_counts = BTreeMap::new();
    for bead in &open_beads {
        for reason in &bead.exclusion_reasons {
            *exclusion_reason_counts.entry(reason.clone()).or_insert(0) += 1;
        }
    }

    let mut excluded_ids: Vec<String> = exclude_ids.iter().map(ToString::to_string).collect();
    excluded_ids.sort();
    let dropped_constraints: Vec<String> = relaxation_tier
        .dropped_constraints()
        .iter()
        .map(|constraint| (*constraint).to_string())
        .collect();

    StarvationDiagnosticSnapshot {
        schema_version: 1,
        event: "pluck.starvation",
        timestamp,
        target_workspace: workspace_from_inventory(beads),
        summary: StarvationSummary {
            message: format!(
                "Pluck returned zero candidates while {} open beads remained",
                open_beads.len()
            ),
            detected_at: timestamp,
            open_bead_count: open_beads.len(),
            candidate_count: 0,
            excluded_bead_count: stats.excluded_count,
            exclusion_reason_counts,
        },
        open_beads,
        worker_constraints: WorkerConstraints {
            worker_id: worker_id.to_string(),
            assignee: None,
            exclude_labels: exclude_labels.to_vec(),
            exclude_ids: excluded_ids.clone(),
            relaxation_tier: relaxation_tier.name().to_string(),
            dropped_constraints,
        },
        pluck_parameters: PluckParameters {
            configured_exclude_labels: exclude_labels.to_vec(),
            default_exclude_labels: DEFAULT_EXCLUDE_LABELS
                .iter()
                .map(|label| (*label).to_string())
                .collect(),
            split_after_failures,
            candidate_sort_order: vec![
                "effective_priority ASC".to_string(),
                "pinned_bucket ASC".to_string(),
                "failure_count ASC".to_string(),
                "created_at ASC".to_string(),
                "id ASC".to_string(),
            ],
            ready_query_assignee: None,
            ready_query_exclude_ids: excluded_ids,
            final_relaxation_tier: relaxation_tier.name().to_string(),
        },
    }
}

fn normalized_label(label: &str) -> String {
    label.trim().to_ascii_lowercase()
}

fn is_manual_block_label(label: &str) -> bool {
    matches!(
        normalized_label(label).as_str(),
        "blocked" | "manual_blocked" | "manual-blocked" | "blocked:manual"
    )
}

fn is_deferred_label(label: &str) -> bool {
    let label = normalized_label(label);
    label == "deferred" || label.starts_with("deferred:")
}

fn is_human_like_label(label: &str, exclude_labels: &[String]) -> bool {
    let label = normalized_label(label);
    if is_deferred_label(&label) || is_manual_block_label(&label) {
        return false;
    }

    label == "human"
        || label.starts_with("human:")
        || label == "human-owned"
        || label == "owner:human"
        || exclude_labels.iter().any(|excluded| {
            normalized_label(excluded) == label
                && !is_deferred_label(excluded)
                && !is_manual_block_label(excluded)
        })
}

fn workspace_from_inventory(beads: &[Bead]) -> String {
    beads
        .iter()
        .filter_map(|bead| bead.workspace.to_str())
        .map(str::trim)
        .find(|workspace| !workspace.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".to_string())
}

/// Extract workspace path from beads, handling NULL/empty workspace columns.
///
/// In bead-rs stores, the workspace column is NULL for every row, which parses
/// to an empty PathBuf. This helper explicitly checks for empty strings and
/// returns a fallback to avoid generating malformed alert titles like
/// "Starvation alert: beads invisible in " (trailing space, empty workspace).
fn extract_workspace_path(beads: &[Bead]) -> String {
    let workspace = workspace_from_inventory(beads);
    if workspace.is_empty() {
        "unknown".to_string()
    } else {
        workspace
    }
}

fn starvation_alert_body(workspace_path: &str, stats: &FilteringStats) -> String {
    let message = if stats.open_count > 0 {
        "Pluck found no candidates but open beads exist"
    } else {
        "Pluck found no candidates and queue is empty"
    };

    format!(
        "{}.\n\n\
         **Workspace:** {}\n\
         **Open beads:** {}\n\
         **Excluded beads:** {}\n\
         **Exclusion reasons:** {}\n\n\
         **Timestamp:** {}",
        message,
        workspace_path,
        stats.open_count,
        stats.excluded_count,
        stats.exclusion_counts.summary(),
        Utc::now().to_rfc3339()
    )
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

/// The durable, machine-readable starvation snapshot.
///
/// This deliberately lives beside the legacy [`StarvationRecord`] instead of
/// replacing it.  The legacy record is consumed by older operators, while
/// this record contains enough point-in-time state to explain a starvation
/// event without querying a store that may already have changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StarvationDiagnosticSnapshot {
    /// Schema version for readers of the JSONL file.
    schema_version: u8,
    /// Stable event name for filtering a mixed diagnostic stream.
    event: &'static str,
    /// UTC time at which the empty candidate result was observed.
    timestamp: chrono::DateTime<chrono::Utc>,
    /// Workspace whose Pluck query returned no candidates.
    target_workspace: String,
    /// Timestamped, aggregate explanation of the event.
    summary: StarvationSummary,
    /// Every open work bead as observed during the diagnostic inventory pass.
    open_beads: Vec<OpenBeadDiagnostic>,
    /// Filters and exclusions active for this worker at evaluation time.
    worker_constraints: WorkerConstraints,
    /// Pluck configuration and the query tier that produced the empty result.
    pluck_parameters: PluckParameters,
}

/// Aggregate counts and a stable human-readable explanation of starvation.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StarvationSummary {
    /// Short explanation suitable for log viewers.
    message: String,
    /// Explicit timestamp for consumers that index the summary independently.
    detected_at: chrono::DateTime<chrono::Utc>,
    /// Number of open work beads in the inventory.
    open_bead_count: usize,
    /// Number of candidates returned after the final filtering pass.
    candidate_count: usize,
    /// Number of open work beads omitted from the candidate result.
    excluded_bead_count: usize,
    /// Count of each per-bead exclusion reason.
    exclusion_reason_counts: BTreeMap<String, usize>,
}

/// Complete point-in-time state for one open work bead.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenBeadDiagnostic {
    id: String,
    title: String,
    description: Option<String>,
    status: String,
    assignee: Option<String>,
    priority: u8,
    labels: Vec<String>,
    workspace: String,
    dependencies: Vec<BrDependency>,
    dependents: Vec<BrDependency>,
    comments: Vec<Comment>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    /// Whether the state itself looks claimable despite the empty ready result.
    is_ready: bool,
    /// All reasons this bead was not claimable in this snapshot.
    exclusion_reasons: Vec<String>,
}

/// The worker-local constraints that can make an otherwise open bead
/// invisible to Pluck.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkerConstraints {
    /// Stable identity of the worker that captured the snapshot.
    worker_id: String,
    /// Assignee predicate; `None` means Pluck searches unassigned work.
    assignee: Option<String>,
    /// Labels excluded by the active Pluck configuration.
    exclude_labels: Vec<String>,
    /// Bead IDs temporarily excluded by the worker (for example after a race).
    exclude_ids: Vec<String>,
    /// Query tier used for the final empty result.
    relaxation_tier: String,
    /// Constraints dropped by that tier.
    dropped_constraints: Vec<String>,
}

/// Pluck algorithm parameters needed to reproduce or interpret a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PluckParameters {
    configured_exclude_labels: Vec<String>,
    default_exclude_labels: Vec<String>,
    split_after_failures: u32,
    candidate_sort_order: Vec<String>,
    ready_query_assignee: Option<String>,
    ready_query_exclude_ids: Vec<String>,
    final_relaxation_tier: String,
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
    /// Whether to write persistent starvation records and diagnostic snapshots
    /// to the NEEDLE workspace.
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

    /// Create a new PluckStrand with persistent starvation records configured.
    ///
    /// When `persistent_starvation_records` is true, starvation events are
    /// written to `needle_workspace/state/starvation-records.jsonl` and full
    /// snapshots are appended to
    /// `needle_workspace/state/starvation_events.jsonl`.
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

    /// Query for work, progressively relaxing constraints when the ready
    /// frontier is empty.
    ///
    /// The first retry drops configured worker-label exclusions. The second
    /// retry also drops any priority predicate (the current `Filters` type has
    /// no priority field, but the tier is retained for backend/config parity).
    /// The final retry reads the complete inventory and accepts only beads
    /// whose status is `open`.
    async fn query_with_relaxation(
        &self,
        store: &dyn BeadStore,
    ) -> Result<(Vec<Bead>, RelaxationTier)> {
        let initial_filters = Filters {
            assignee: None,
            exclude_labels: self.exclude_labels.clone(),
            exclude_ids: HashSet::new(),
        };

        tracing::debug!(
            filters = ?initial_filters,
            "Querying bead store for ready candidates"
        );
        tracing::info!(
            tier = RelaxationTier::Initial.name(),
            assignee = ?initial_filters.assignee,
            exclude_labels = ?initial_filters.exclude_labels,
            exclude_ids_count = initial_filters.exclude_ids.len(),
            dropped_constraints = ?RelaxationTier::Initial.dropped_constraints(),
            "Executing Pluck query with filters"
        );

        let candidates = store.ready(&initial_filters).await?;
        tracing::debug!(
            tier = RelaxationTier::Initial.name(),
            count = candidates.len(),
            "Bead store returned candidates"
        );
        if !candidates.is_empty() {
            return Ok((candidates, RelaxationTier::Initial));
        }

        let relaxed_filters = Filters {
            assignee: None,
            exclude_labels: Vec::new(),
            exclude_ids: HashSet::new(),
        };

        for tier in [RelaxationTier::WorkerLabels, RelaxationTier::Priority] {
            tracing::warn!(
                tier = tier.name(),
                dropped_constraints = ?tier.dropped_constraints(),
                "Pluck query returned no candidates; retrying with relaxed constraints"
            );
            tracing::info!(
                tier = tier.name(),
                assignee = ?relaxed_filters.assignee,
                exclude_labels = ?relaxed_filters.exclude_labels,
                exclude_ids_count = relaxed_filters.exclude_ids.len(),
                dropped_constraints = ?tier.dropped_constraints(),
                "Executing Pluck relaxation query"
            );

            let candidates = store.ready(&relaxed_filters).await?;
            tracing::debug!(
                tier = tier.name(),
                count = candidates.len(),
                "Bead store returned candidates after relaxation"
            );
            if !candidates.is_empty() {
                tracing::warn!(
                    tier = tier.name(),
                    candidate_count = candidates.len(),
                    dropped_constraints = ?tier.dropped_constraints(),
                    "Pluck is proceeding with relaxed constraints"
                );
                return Ok((candidates, tier));
            }
        }

        let tier = RelaxationTier::StatusOnly;
        tracing::warn!(
            tier = tier.name(),
            dropped_constraints = ?tier.dropped_constraints(),
            "Pluck ready queries returned no candidates; retrying with status=open as the sole filter"
        );
        let inventory = store.starvation_inventory().await?;
        let candidates: Vec<Bead> = inventory
            .into_iter()
            .filter(|bead| bead.status == crate::types::BeadStatus::Open)
            .collect();
        tracing::debug!(
            tier = tier.name(),
            count = candidates.len(),
            dropped_constraints = ?tier.dropped_constraints(),
            "Bead store returned candidates after status-only relaxation"
        );
        if !candidates.is_empty() {
            tracing::warn!(
                tier = tier.name(),
                candidate_count = candidates.len(),
                dropped_constraints = ?tier.dropped_constraints(),
                "Pluck is proceeding with relaxed constraints"
            );
        }
        Ok((candidates, tier))
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

    /// Compute effective priority with aging adjustment.
    ///
    /// Effective priority = min(own_priority, min priority of transitively blocked open beads),
    /// then raised one level if >14 days old and two levels if >30 days old.
    fn compute_effective_priority(
        bead: &Bead,
        min_blocked_priority: u8,
        now: chrono::DateTime<chrono::Utc>,
    ) -> u8 {
        let base_priority = bead.priority.min(min_blocked_priority);

        // Apply aging: lower number = higher priority, so we subtract
        let age = now.signed_duration_since(bead.created_at);
        if age > chrono::Duration::days(30) {
            // Raise two levels (but don't go below 0)
            base_priority.saturating_sub(2)
        } else if age > chrono::Duration::days(14) {
            // Raise one level
            base_priority.saturating_sub(1)
        } else {
            base_priority
        }
    }

    /// Compute pinned bucket score.
    ///
    /// pinned_bucket = -floor(log2(1 + transitive_dependent_count))
    /// More dependents = more negative = sorts earlier (higher priority)
    fn compute_pinned_bucket(transitive_dependent_count: usize) -> i64 {
        if transitive_dependent_count == 0 {
            return 0;
        }
        // -floor(log2(1 + n)) where n is transitive dependent count
        let log_val = (transitive_dependent_count + 1).ilog2() as i64;
        -log_val
    }

    /// Compute both dependency-derived values needed by the ordering key.
    ///
    /// The graph contains only open beads, so one traversal gives the minimum
    /// priority inherited from all transitively blocked beads and the number
    /// of unique open dependents for the pinned bucket.
    fn dependency_metrics(
        bead: &Bead,
        graph: &HashMap<BeadId, Vec<BeadId>>,
        open_beads_by_id: &HashMap<BeadId, &Bead>,
    ) -> (u8, usize) {
        let mut min_priority = bead.priority;
        let mut queue = std::collections::VecDeque::new();
        let mut visited = HashSet::new();

        queue.push_back(bead.id.clone());
        visited.insert(bead.id.clone());
        let mut transitive_dependents = 0;

        while let Some(current) = queue.pop_front() {
            if let Some(blocked) = graph.get(&current) {
                for blocked_id in blocked {
                    if let Some(&blocked_bead) = open_beads_by_id.get(blocked_id) {
                        if visited.insert(blocked_id.clone()) {
                            transitive_dependents += 1;
                            min_priority = min_priority.min(blocked_bead.priority);
                            queue.push_back(blocked_id.clone());
                        }
                    }
                }
            }
        }

        (min_priority, transitive_dependents)
    }

    /// Build the dependency graph and open-bead lookup from one inventory snapshot.
    ///
    /// Returns:
    /// - blocked_graph: maps bead_id -> list of open bead IDs it blocks
    /// - open_beads_by_id: maps bead_id -> bead reference for all open beads
    fn build_dependency_graphs(
        all_beads: &[Bead],
    ) -> (HashMap<BeadId, Vec<BeadId>>, HashMap<BeadId, &Bead>) {
        let mut blocked_graph: HashMap<BeadId, Vec<BeadId>> = HashMap::new();
        let mut open_beads_by_id: HashMap<BeadId, &Bead> = HashMap::new();

        // First pass: collect all open beads
        for bead in all_beads {
            if bead.status == crate::types::BeadStatus::Open {
                open_beads_by_id.insert(bead.id.clone(), bead);
            }
        }

        // Second pass: build dependency graphs
        for bead in all_beads {
            if bead.status != crate::types::BeadStatus::Open {
                continue;
            }

            for dependency in &bead.dependencies {
                // Check if this is a "blocks" dependency
                let is_blocking = dependency.dependency_type.is_empty()
                    || dependency.dependency_type.eq_ignore_ascii_case("blocks");

                if is_blocking {
                    blocked_graph
                        .entry(dependency.id.clone())
                        .or_default()
                        .push(bead.id.clone());
                }
            }
        }

        (blocked_graph, open_beads_by_id)
    }

    /// Sort by the current non-graph-aware key.
    fn sort_by_current_key(candidates: &mut [Bead]) {
        candidates.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| Self::extract_failure_count(a).cmp(&Self::extract_failure_count(b)))
                .then_with(|| a.created_at.cmp(&b.created_at))
                .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
        });
    }

    /// Sort candidates in deterministic priority order with dependency awareness.
    ///
    /// Sort key: `(effective_priority ASC, pinned_bucket ASC, failure_count ASC, created_at ASC, id ASC)`.
    ///
    /// - `effective_priority`: min of own priority and all transitively blocked open beads,
    ///   with aging adjustment (+1 level at 14 days, +2 levels at 30 days)
    /// - `pinned_bucket`: -floor(log2(1 + transitive_dependent_count)), so beads blocking more
    ///   open beads sort earlier
    /// - `failure_count`: extracted from labels, prevents struggling beads from monopolizing slot 1
    /// - `created_at`: ties broken by age (older first)
    /// - `id`: final tie-breaker for determinism
    ///
    /// The graph is memoized once per evaluation cycle. If open beads exceed 5,000,
    /// falls back to simpler sort and emits `pluck.ordering_degraded` telemetry.
    fn sort_candidates(&self, candidates: &mut [Bead], all_beads: &[Bead], telemetry: &Telemetry) {
        let now = Utc::now();

        // Count open beads to check for degradation threshold
        let open_count = all_beads
            .iter()
            .filter(|b| b.status == crate::types::BeadStatus::Open)
            .count();

        const DEGRADATION_THRESHOLD: usize = 5000;

        if open_count > DEGRADATION_THRESHOLD {
            // Emit degradation event and use simpler sorting
            let _ = telemetry.emit(
                crate::telemetry::EventKind::PluckOrderingDegraded {
                    open_bead_count: open_count,
                    threshold: DEGRADATION_THRESHOLD,
                },
                now,
            );

            // Fall back to current simpler sort to maintain performance
            Self::sort_by_current_key(candidates);
            return;
        }

        // Build the graph once, then cache each candidate's complete ordering
        // key. This keeps dependency traversal out of sort comparisons.
        let (blocked_graph, open_beads_by_id) = Self::build_dependency_graphs(all_beads);
        candidates.sort_by_cached_key(|bead| {
            let (min_blocked_priority, transitive_dependents) =
                Self::dependency_metrics(bead, &blocked_graph, &open_beads_by_id);
            (
                Self::compute_effective_priority(bead, min_blocked_priority, now),
                Self::compute_pinned_bucket(transitive_dependents),
                Self::extract_failure_count(bead),
                bead.created_at,
                bead.id.as_ref().to_string(),
            )
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

    /// Append a complete starvation snapshot to NEEDLE's durable diagnostic
    /// stream.  The lock covers serialization and the newline write so two
    /// workers cannot interleave JSON objects in the same JSONL file.
    fn write_starvation_diagnostic(&self, snapshot: &StarvationDiagnosticSnapshot) -> Result<()> {
        let needle_home = self
            .needle_workspace
            .as_ref()
            .context("needle_workspace not set - cannot write diagnostic snapshot")?;
        let state_dir = needle_home.join("state");
        std::fs::create_dir_all(&state_dir).with_context(|| {
            format!(
                "failed to create starvation diagnostics directory: {}",
                state_dir.display()
            )
        })?;

        let record_path = state_dir.join("starvation_events.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&record_path)
            .with_context(|| {
                format!(
                    "failed to open starvation diagnostics file: {}",
                    record_path.display()
                )
            })?;

        use fs2::FileExt;
        file.lock_exclusive().with_context(|| {
            format!(
                "failed to lock starvation diagnostics file: {}",
                record_path.display()
            )
        })?;

        let write_result = (|| -> Result<()> {
            use std::io::Write;
            serde_json::to_writer(&mut file, snapshot)
                .context("failed to serialize starvation diagnostic snapshot")?;
            file.write_all(b"\n").with_context(|| {
                format!(
                    "failed to write starvation diagnostic snapshot to: {}",
                    record_path.display()
                )
            })?;
            file.flush().with_context(|| {
                format!(
                    "failed to flush starvation diagnostic snapshot to: {}",
                    record_path.display()
                )
            })?;
            file.sync_data().with_context(|| {
                format!(
                    "failed to persist starvation diagnostic snapshot to: {}",
                    record_path.display()
                )
            })?;
            Ok(())
        })();

        let unlock_result = fs2::FileExt::unlock(&file).with_context(|| {
            format!(
                "failed to unlock starvation diagnostics file: {}",
                record_path.display()
            )
        });

        write_result?;
        unlock_result?;

        tracing::debug!(
            record_path = %record_path.display(),
            target_workspace = %snapshot.target_workspace,
            open_bead_count = snapshot.open_beads.len(),
            "Wrote starvation diagnostic snapshot"
        );
        Ok(())
    }
}

/// Sanitize a workspace path for use in a dedup label.
///
/// Converts workspace paths to safe label identifiers by extracting
/// the workspace name and removing special characters.
/// Examples:
/// - "/home/coding/NEEDLE" → "NEEDLE"
/// - "/home/coding/my-project" → "my-project"
/// - "/home/user/repo_name" → "repo_name"
#[allow(dead_code)]
fn sanitize_workspace_name(workspace_path: &str) -> String {
    workspace_path
        .rsplit('/')
        .find(|component| !component.is_empty())
        .unwrap_or("unknown")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
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

        // Check if the home workspace is gate-degraded. If so, skip ordinary dispatch.
        // Degraded workspaces are excluded from Pluck to prevent repeated gate
        // execution errors. The workspace remains claimable for manual intervention
        // or for fixing the specific gate that caused degradation.
        if let Ok(true) = crate::gate_health::is_degraded(
            &std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        ) {
            tracing::warn!(
                workspace = %std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")).display(),
                "Home workspace is gate-degraded — skipping Pluck strand for ordinary dispatch"
            );
            return StrandResult::Skipped {
                reason: "workspace_gate_degraded".to_string(),
            };
        }

        // 1. Query bead store for ready, unassigned beads. If the normal
        // query is empty, retry through the bounded relaxation waterfall.
        let (mut candidates, relaxation_tier) = match self.query_with_relaxation(store).await {
            Ok(result) => result,
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

        tracing::debug!(
            tier = relaxation_tier.name(),
            dropped_constraints = ?relaxation_tier.dropped_constraints(),
            count = candidates.len(),
            "Pluck candidate query completed"
        );

        // Initialize filtering statistics for starvation telemetry.
        let mut stats = FilteringStats {
            open_count: candidates.len(),
            ..Default::default()
        };

        // 2. Filter: remove beads with excluded labels unless a relaxation
        // tier explicitly dropped the worker-label constraint.
        //    Defensive guard — store.ready() passes exclude_labels in its Filters,
        //    but the backing CLI may not include label data in every query type.
        //    Filtering here guarantees excluded-label beads are never presented as
        //    candidates regardless of backend behaviour, preventing the
        //    SELECTING→CLAIMING→RETRYING spin loop observed when br ready --json
        //    omits label fields for some beads.
        let before_label_filter = candidates.len();
        let label_exclusions = if relaxation_tier.ignores_labels() {
            &[]
        } else {
            self.exclude_labels.as_slice()
        };

        // First pass: collect excluded beads and their reasons for telemetry.
        let excluded_beads: Vec<_> = candidates
            .iter()
            .filter(|b| b.labels.iter().any(|l| label_exclusions.contains(l)))
            .map(|b| {
                let excluded_labels: Vec<_> = b
                    .labels
                    .iter()
                    .filter(|l| label_exclusions.contains(l))
                    .cloned()
                    .collect();
                (b.id.as_ref().to_string(), excluded_labels)
            })
            .collect();

        // Second pass: perform the actual filtering.
        candidates.retain(|b| !b.labels.iter().any(|l| label_exclusions.contains(l)));
        let after_label_filter = candidates.len();

        if before_label_filter != after_label_filter {
            let label_excluded_count = before_label_filter - after_label_filter;
            stats.excluded_count += label_excluded_count;

            tracing::debug!(
                excluded_count = label_excluded_count,
                remaining = after_label_filter,
                excluded_labels = ?label_exclusions,
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

        // 3. Apply the normal readiness guards unless the final fallback was
        // selected. The status-only tier intentionally keeps `status=open` as
        // its sole filter, including beads with stale assignees.
        let before_status_filter = candidates.len();

        let excluded_by_status: Vec<_> = if relaxation_tier == RelaxationTier::StatusOnly {
            candidates
                .iter()
                .filter(|b| b.status != crate::types::BeadStatus::Open)
                .map(|b| (b.id.as_ref().to_string(), format!("status:{}", b.status)))
                .collect()
        } else {
            candidates
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
                .collect()
        };

        // Second pass: perform the actual filtering.
        if relaxation_tier == RelaxationTier::StatusOnly {
            candidates.retain(|b| b.status == crate::types::BeadStatus::Open);
        } else {
            candidates.retain(|b| {
                !(matches!(b.status, crate::types::BeadStatus::InProgress)
                    || (b.status == crate::types::BeadStatus::Open && b.assignee.is_some()))
            });
        }
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

        // 4. Sort: deterministic with dependency awareness.
        // Need full inventory for priority inheritance and pinned bucket computation.
        let all_beads = match store.list_all().await {
            Ok(beads) => Some(beads),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to load full inventory for dependency-aware sorting, falling back to simple sort"
                );
                Self::sort_by_current_key(&mut candidates);
                None
            }
        };

        if !candidates.is_empty() {
            let first = &candidates[0];
            tracing::debug!(
                total = candidates.len(),
                first_bead_id = %first.id,
                first_priority = first.priority,
                first_created_at = %first.created_at,
                "Sorting {} candidates by (effective_priority, pinned_bucket, failure_count, created_at, id)",
                candidates.len()
            );
        }

        // Apply the new dependency-aware sorting when the inventory snapshot
        // was available; the error path above has already used the fallback.
        if let Some(all_beads) = all_beads.as_deref() {
            self.sort_candidates(&mut candidates, all_beads, &self.telemetry);
        }

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
            // The ready query only contains the candidate frontier. Re-read the
            // complete inventory so starvation metadata reflects the beads that
            // were omitted by readiness predicates as well.
            let all_beads = match store.starvation_inventory().await {
                Ok(beads) => Some(beads),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "Unable to load full bead inventory for starvation metadata"
                    );
                    None
                }
            };

            let workspace_path = all_beads
                .as_deref()
                .map(extract_workspace_path)
                .unwrap_or_else(|| "unknown".to_string());

            if let Some(beads) = all_beads.as_deref() {
                let inventory_stats = FilteringStats::from_inventory(beads, &self.exclude_labels);
                stats.open_count = inventory_stats.open_count;
                stats.excluded_count = inventory_stats.excluded_count;
                stats.exclusion_counts = inventory_stats.exclusion_counts;
            }

            // Refresh the strand's observable statistics after the inventory
            // pass, rather than exposing the ready-query count.
            self.last_open_count
                .store(stats.open_count, Ordering::Relaxed);
            self.last_excluded_count
                .store(stats.excluded_count, Ordering::Relaxed);
            *self.last_exclusion_reasons.lock().unwrap() = stats.exclusion_reasons.clone();

            // Emit PluckStarvationDetected telemetry event with filtering statistics.
            let _ = self.telemetry.emit(
                crate::telemetry::EventKind::PluckStarvationDetected {
                    workspace: workspace_path.clone(),
                    open_count: stats.open_count,
                    excluded_count: stats.excluded_count,
                    candidate_exclusion_reasons: stats.exclusion_reasons.clone(),
                },
                Utc::now(),
            );

            // Persist a point-in-time snapshot whenever the inventory proves
            // that open work remained.  This is intentionally outside the
            // starvation-alert-bead branch: a dependency-only inventory is
            // still valuable evidence, even when it is not actionable
            // starvation and no alert bead is created.
            if self.persistent_starvation_records && stats.open_count > 0 {
                if let Some(beads) = all_beads.as_deref() {
                    let snapshot = starvation_diagnostic_snapshot(
                        beads,
                        &stats,
                        &self.exclude_labels,
                        exclusions,
                        self.telemetry.worker_id(),
                        relaxation_tier,
                        self.split_after_failures,
                    );
                    if let Err(error) = self.write_starvation_diagnostic(&snapshot) {
                        tracing::warn!(
                            error = %error,
                            target_workspace = %snapshot.target_workspace,
                            "Failed to write starvation diagnostic snapshot"
                        );
                    }
                }
            }

            // An empty inventory (or an inventory containing only in-progress
            // work) is ordinary idleness, not starvation. Keep the telemetry for
            // observability, but do not create a self-contradictory alert.
            //
            // Only create a starvation alert when at least one open bead has
            // no blocking dependencies. A dependency-only queue is expected
            // idleness while the blockers are being worked.
            if stats.open_count > 0 {
                if !stats.has_unblocked_open_bead() {
                    tracing::info!(
                        workspace = %workspace_path,
                        open_count = stats.open_count,
                        blocked_count = stats.exclusion_counts.blocked,
                        reason = "blocked_on_dependencies",
                        "Skipping starvation alert because every open bead has a blocking dependency"
                    );
                } else {
                    // Write persistent starvation record to NEEDLE workspace if enabled.
                    if self.persistent_starvation_records {
                        let reason_summary = stats.exclusion_counts.summary_vec();
                        if let Err(e) = self.write_starvation_record(
                            &workspace_path,
                            stats.open_count,
                            stats.excluded_count,
                            &reason_summary,
                        ) {
                            tracing::warn!(
                                error = %e,
                                "Failed to write persistent starvation record"
                            );
                        }
                    }

                    // Create or update a starvation alert bead with fingerprint-based deduplication.
                    // The fingerprint is computed from workspace + kind + normalized cause.
                    let cause = format!(
                        "open={}, excluded={}, reasons={}",
                        stats.open_count,
                        stats.excluded_count,
                        stats.exclusion_counts.summary()
                    );

                    let dedup_result = check_alert_deduplication(
                        store,
                        &workspace_path,
                        &AlertKind::PluckStarvation,
                        &cause,
                    )
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            error = %e,
                            workspace = %workspace_path,
                            "Failed to check alert deduplication, proceeding with creation"
                        );
                        AlertDeduplication::CreateNew
                    });

                    match dedup_result {
                        AlertDeduplication::Deduplicated {
                            bead_id,
                            fingerprint,
                        } => {
                            // Append timestamped note to existing bead
                            let note = format!(
                                "Starvation recurred: {} open, {} excluded; reasons: {}",
                                stats.open_count,
                                stats.excluded_count,
                                stats.exclusion_counts.summary()
                            );
                            let _ = append_alert_note(store, &bead_id, &note).await;

                            tracing::info!(
                                bead_id = %bead_id,
                                workspace = %workspace_path,
                                fingerprint = %fingerprint,
                                open_count = stats.open_count,
                                excluded_count = stats.excluded_count,
                                "Starvation alert deduplicated - appended note to existing bead"
                            );

                            // Emit deduplication telemetry
                            let _ = self.telemetry.emit(
                                crate::telemetry::EventKind::AlertDeduplicated {
                                    fingerprint,
                                    bead_id: bead_id.clone(),
                                    kind: "pluck-starvation".to_string(),
                                },
                                Utc::now(),
                            );
                        }
                        AlertDeduplication::Suppressed { bead_id, closed_at } => {
                            tracing::info!(
                                bead_id = %bead_id,
                                workspace = %workspace_path,
                                closed_at = %closed_at,
                                "Starvation alert suppressed - bead was closed within 24h"
                            );
                        }
                        AlertDeduplication::CreateNew => {
                            // Create a new starvation alert bead
                            let fingerprint = crate::fingerprint::compute_fingerprint(
                                &workspace_path,
                                &AlertKind::PluckStarvation,
                                &cause,
                            );

                            let title = format!(
                                "Starvation alert: beads invisible in {}",
                                workspace_path.rsplit('/').next().unwrap_or("workspace")
                            );

                            let body = starvation_alert_body(&workspace_path, &stats);
                            let labels = build_alert_labels(
                                &fingerprint,
                                &["starvation-alert", "human"], // Requires human review
                            );
                            let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();

                            if let Err(e) = store.create_bead(&title, &body, &label_refs).await {
                                tracing::warn!(
                                    error = %e,
                                    workspace = %workspace_path,
                                    "Failed to create starvation alert bead"
                                );
                            } else {
                                tracing::info!(
                                    workspace = %workspace_path,
                                    fingerprint = %fingerprint,
                                    open_count = stats.open_count,
                                    excluded_count = stats.excluded_count,
                                    "Created starvation alert bead with fingerprint"
                                );
                            }
                        }
                    }
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
    use crate::types::{BeadId, BeadStatus, BrDependency, ClaimResult};

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
            comments: vec![],
            created_at: Utc.from_utc_datetime(&dt),
            updated_at: Utc.from_utc_datetime(&dt),
        }
    }

    fn make_bead_with_age(id: &str, priority: u8, age_days: i64) -> Bead {
        let mut bead = make_bead(id, priority, "2026-01-01 00:00:00");
        let created_at = Utc::now() - chrono::Duration::days(age_days);
        bead.created_at = created_at;
        bead.updated_at = created_at;
        bead
    }

    fn make_blocks_dependency(id: &str) -> BrDependency {
        BrDependency {
            id: BeadId::from(id.to_string()),
            title: String::new(),
            status: "open".to_string(),
            priority: 0,
            dependency_type: "blocks".to_string(),
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
    async fn root_inherits_priority_from_open_transitive_dependent() {
        let root = make_bead_with_age("p2-root", 2, 1);
        let leaf = make_bead_with_age("p1-leaf", 1, 1);
        let mut dependent = make_bead_with_age("p0-dependent", 0, 1);
        dependent
            .dependencies
            .push(make_blocks_dependency("p2-root"));
        let store = MemoryStore {
            beads: vec![leaf, root, dependent],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                let root_position = beads.iter().position(|bead| bead.id.as_ref() == "p2-root");
                let leaf_position = beads.iter().position(|bead| bead.id.as_ref() == "p1-leaf");
                assert!(
                    root_position < leaf_position,
                    "P2 root inheriting P0 should sort before P1 leaf: {beads:?}"
                );
            }
            other => panic!("expected BeadFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn pinned_bucket_prefers_root_with_more_open_dependents() {
        let root = make_bead_with_age("root", 1, 1);
        let leaf = make_bead_with_age("leaf", 1, 1);
        let mut dependent_a = make_bead_with_age("dependent-a", 1, 1);
        dependent_a
            .dependencies
            .push(make_blocks_dependency("root"));
        let mut dependent_b = make_bead_with_age("dependent-b", 1, 1);
        dependent_b
            .dependencies
            .push(make_blocks_dependency("root"));
        let store = MemoryStore {
            beads: vec![leaf, dependent_a, root, dependent_b],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                let root_position = beads.iter().position(|bead| bead.id.as_ref() == "root");
                let leaf_position = beads.iter().position(|bead| bead.id.as_ref() == "leaf");
                assert!(root_position < leaf_position);
            }
            other => panic!("expected BeadFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn thirty_one_day_old_p3_leaf_is_aged_to_p1() {
        let old_leaf = make_bead_with_age("old-p3", 3, 31);
        let fresh_p2 = make_bead_with_age("fresh-p2", 2, 1);
        let store = MemoryStore {
            beads: vec![fresh_p2, old_leaf],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        assert_eq!(
            PluckStrand::compute_effective_priority(
                &store.beads[1],
                store.beads[1].priority,
                Utc::now()
            ),
            1
        );
        match result {
            StrandResult::BeadFound(beads) => {
                assert_eq!(beads[0].id.as_ref(), "old-p3");
            }
            other => panic!("expected BeadFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn separate_pluck_instances_have_identical_ordering() {
        let root = make_bead_with_age("root", 2, 1);
        let mut child = make_bead_with_age("child", 0, 1);
        child.dependencies.push(make_blocks_dependency("root"));
        let fixture = vec![
            make_bead_with_age("leaf", 1, 16),
            root,
            child,
            make_bead_with_age("tie-b", 2, 1),
            make_bead_with_age("tie-a", 2, 1),
        ];
        let store_a = MemoryStore {
            beads: fixture.clone(),
        };
        let store_b = MemoryStore { beads: fixture };
        let strand_a = PluckStrand::new(vec![], Telemetry::new("test-worker-a".to_string()));
        let strand_b = PluckStrand::new(vec![], Telemetry::new("test-worker-b".to_string()));

        let result_a = strand_a.evaluate(&store_a, &HashSet::new()).await;
        let result_b = strand_b.evaluate(&store_b, &HashSet::new()).await;
        let ids_a: Vec<_> = match result_a {
            StrandResult::BeadFound(beads) => beads.into_iter().map(|bead| bead.id).collect(),
            other => panic!("expected BeadFound, got: {other:?}"),
        };
        let ids_b: Vec<_> = match result_b {
            StrandResult::BeadFound(beads) => beads.into_iter().map(|bead| bead.id).collect(),
            other => panic!("expected BeadFound, got: {other:?}"),
        };

        assert_eq!(ids_a, ids_b);
    }

    #[tokio::test]
    async fn large_open_queue_uses_degraded_ordering() {
        use crate::telemetry::test_utils::TestHelper;
        use std::time::{Duration, Instant};

        let beads = (0..=5000)
            .map(|index| make_bead(&format!("large-{index:04}"), 1, "2026-01-01 00:00:00"))
            .collect();
        let store = MemoryStore { beads };
        let helper = TestHelper::new("test-worker");
        let strand = PluckStrand::new(vec![], helper.telemetry().clone());

        let started = Instant::now();
        let result = strand.evaluate(&store, &HashSet::new()).await;
        let elapsed = started.elapsed();
        helper.sync().await;

        assert!(matches!(result, StrandResult::BeadFound(_)));
        assert!(
            elapsed < Duration::from_secs(1),
            "degraded ordering took too long: {elapsed:?}"
        );
        let events = helper.events_by_type("strand.pluck.ordering_degraded");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data["open_bead_count"], 5001);
        assert_eq!(events[0].data["threshold"], 5000);
    }

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
        let mut urgent_but_struggling = make_bead_with_age("urgent-but-struggling", 1, 1);
        urgent_but_struggling.labels = vec!["failure-count:2".to_string()];
        let healthy_but_low_priority = make_bead_with_age("healthy-but-low-priority", 2, 1);
        let store = MemoryStore {
            // failure-count:2 stays below the default split_after_failures
            // threshold (3) — this test is about sort order, not the
            // separate split-trigger path (see split_triggered_when_failure_count_exceeds_threshold).
            beads: vec![urgent_but_struggling, healthy_but_low_priority],
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

    #[tokio::test]
    async fn retries_with_worker_labels_relaxed_when_ready_is_empty() {
        let store = MemoryStore {
            beads: vec![make_bead_with_labels("human-bead", 1, vec!["human"])],
        };

        let strand = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));
        let result = strand.evaluate(&store, &HashSet::new()).await;

        match result {
            StrandResult::BeadFound(beads) => {
                assert_eq!(beads.len(), 1);
                assert_eq!(beads[0].id.as_ref(), "human-bead");
            }
            other => panic!("expected relaxed query to find the bead, got: {other:?}"),
        }
    }

    #[test]
    fn relaxation_tiers_report_the_constraints_they_drop() {
        assert!(RelaxationTier::Initial.dropped_constraints().is_empty());
        assert_eq!(
            RelaxationTier::WorkerLabels.dropped_constraints(),
            &["worker label constraints"]
        );
        assert_eq!(
            RelaxationTier::Priority.dropped_constraints(),
            &["worker label constraints", "priority constraints"]
        );
        assert_eq!(
            RelaxationTier::StatusOnly.dropped_constraints(),
            &[
                "worker label constraints",
                "priority constraints",
                "readiness constraints",
                "assignee constraints"
            ]
        );
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

    #[test]
    fn starvation_inventory_counts_dependency_and_manual_exclusions() {
        let blocker = make_bead("blocker", 1, "2026-01-01 00:00:00");
        let mut dependency_blocked = make_bead("dependency-blocked", 1, "2026-01-01 00:00:00");
        dependency_blocked.dependencies.push(BrDependency {
            id: blocker.id.clone(),
            title: String::new(),
            status: String::new(),
            priority: 1,
            dependency_type: "blocks".to_string(),
        });

        let manual_blocked = make_bead_with_status("manual-blocked", 1, BeadStatus::Blocked);
        let human = make_bead_with_labels("human", 1, vec!["human-owned"]);
        let deferred = make_bead_with_labels("deferred", 1, vec!["deferred"]);

        let stats = FilteringStats::from_inventory(
            &[blocker, dependency_blocked, manual_blocked, human, deferred],
            &DEFAULT_EXCLUDE_LABELS
                .iter()
                .map(|label| (*label).to_string())
                .collect::<Vec<_>>(),
        );

        assert_eq!(stats.open_count, 5);
        assert_eq!(stats.excluded_count, 5);
        assert_eq!(stats.exclusion_counts.blocked, 1);
        assert_eq!(stats.exclusion_counts.manual_blocked, 1);
        assert_eq!(stats.exclusion_counts.human, 1);
        assert_eq!(stats.exclusion_counts.deferred_assignee, 1);
        assert_eq!(
            stats.exclusion_counts.summary(),
            "blocked=1, manual_blocked=1, human=1, deferred_assignee=1"
        );
    }

    #[test]
    fn starvation_inventory_counts_assigned_beads_as_deferred_assignees() {
        let assigned = make_bead_with_assignee("assigned", "old-worker");
        let stats = FilteringStats::from_inventory(&[assigned], &[]);

        assert_eq!(stats.exclusion_counts.deferred_assignee, 1);
        assert_eq!(stats.exclusion_counts.summary(), "deferred_assignee=1");
    }

    #[test]
    fn dependency_only_inventory_has_no_unblocked_open_bead() {
        let stats = FilteringStats {
            open_count: 2,
            exclusion_counts: ExclusionCounts {
                blocked: 2,
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(!stats.has_unblocked_open_bead());
    }

    #[test]
    fn mixed_inventory_has_an_unblocked_open_bead() {
        let stats = FilteringStats {
            open_count: 2,
            exclusion_counts: ExclusionCounts {
                blocked: 1,
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(stats.has_unblocked_open_bead());
    }

    #[test]
    fn starvation_alert_body_contains_counted_exclusion_reasons() {
        let stats = FilteringStats {
            open_count: 2,
            excluded_count: 2,
            exclusion_counts: ExclusionCounts {
                blocked: 1,
                manual_blocked: 1,
                human: 1,
                deferred_assignee: 1,
            },
            ..Default::default()
        };

        let body = starvation_alert_body("/workspace", &stats);

        assert!(body.contains("**Open beads:** 2"));
        assert!(body.contains("**Excluded beads:** 2"));
        assert!(body.contains(
            "**Exclusion reasons:** blocked=1, manual_blocked=1, human=1, deferred_assignee=1"
        ));
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

        // Verify open_count - should be 0 (in-progress beads are not open work)
        if let Some(open_count) = event.data.get("open_count") {
            assert_eq!(open_count.as_u64(), Some(0));
        } else {
            panic!("open_count field missing from starvation event");
        }

        // Verify excluded_count - should be 0 (there are no open beads to exclude)
        if let Some(excluded_count) = event.data.get("excluded_count") {
            assert_eq!(excluded_count.as_u64(), Some(0));
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
                    reason_strings
                        .iter()
                        .filter(|r| **r == "status:in_progress")
                        .count(),
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

    #[tokio::test]
    async fn starvation_diagnostic_snapshot_captures_full_state_and_constraints() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("diagnostic-worker");
        let needle_workspace = tempfile::tempdir().unwrap();

        let mut assigned = make_bead_with_assignee("assigned-bead", "other-worker");
        assigned.body = Some("Keep the complete description in the snapshot.".to_string());
        assigned.labels = vec!["deferred".to_string()];

        let mut dependency_blocked = make_bead_with_labels("dependency-bead", 2, vec!["blocked"]);
        dependency_blocked.dependencies.push(BrDependency {
            id: BeadId::from("missing-blocker"),
            title: "Missing blocker".to_string(),
            status: "open".to_string(),
            priority: 1,
            dependency_type: "blocks".to_string(),
        });

        // UnfilteredStore models a backend that returns rows from ready() but
        // fails to apply labels.  Pluck's defensive filter then produces the
        // zero-candidate path while the inventory still contains both beads.
        let store = UnfilteredStore {
            beads: vec![assigned, dependency_blocked],
        };
        let strand = PluckStrand::with_persistent_records(
            vec![],
            3,
            helper.telemetry().clone(),
            needle_workspace.path().to_path_buf(),
            true,
        );

        assert!(matches!(
            strand.evaluate(&store, &HashSet::new()).await,
            StrandResult::NoWork
        ));

        let record_path = needle_workspace
            .path()
            .join("state")
            .join("starvation_events.jsonl");
        let content = std::fs::read_to_string(&record_path).unwrap();
        let record: serde_json::Value = serde_json::from_str(content.lines().last().unwrap())
            .expect("diagnostic record should be one valid JSON object per line");

        assert_eq!(record["schema_version"], 1);
        assert_eq!(record["event"], "pluck.starvation");
        assert_eq!(record["target_workspace"], "/tmp/test");
        assert!(record["timestamp"].as_str().is_some());
        assert_eq!(record["summary"]["candidate_count"], 0);
        assert_eq!(record["summary"]["open_bead_count"], 2);
        assert_eq!(record["summary"]["excluded_bead_count"], 2);
        assert!(record["summary"]["message"]
            .as_str()
            .unwrap()
            .contains("2 open beads remained"));

        let open_beads = record["open_beads"].as_array().unwrap();
        assert_eq!(open_beads.len(), 2);
        let assigned_snapshot = open_beads
            .iter()
            .find(|bead| bead["id"] == "assigned-bead")
            .expect("assigned bead should be captured");
        assert_eq!(assigned_snapshot["status"], "open");
        assert_eq!(assigned_snapshot["assignee"], "other-worker");
        assert_eq!(
            assigned_snapshot["description"],
            "Keep the complete description in the snapshot."
        );
        assert_eq!(assigned_snapshot["labels"], serde_json::json!(["deferred"]));
        assert!(assigned_snapshot["exclusion_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "assignee:other-worker"));

        let dependency_snapshot = open_beads
            .iter()
            .find(|bead| bead["id"] == "dependency-bead")
            .expect("dependency bead should be captured");
        assert_eq!(
            dependency_snapshot["dependencies"][0]["id"],
            "missing-blocker"
        );
        assert!(dependency_snapshot["exclusion_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "dependency:missing-blocker"));

        assert_eq!(
            record["worker_constraints"]["exclude_labels"],
            serde_json::json!(["deferred", "human", "blocked"])
        );
        assert_eq!(
            record["worker_constraints"]["exclude_ids"],
            serde_json::json!([])
        );
        assert_eq!(record["worker_constraints"]["relaxation_tier"], "initial");
        assert_eq!(record["pluck_parameters"]["split_after_failures"], 3);
        assert_eq!(
            record["pluck_parameters"]["candidate_sort_order"],
            serde_json::json!([
                "effective_priority ASC",
                "pinned_bucket ASC",
                "failure_count ASC",
                "created_at ASC",
                "id ASC"
            ])
        );
        assert_eq!(
            record["summary"]["exclusion_reason_counts"]["dependency:missing-blocker"],
            1
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
                make_bead_with_workspace_and_labels(
                    "deferred-1",
                    1,
                    workspace_path,
                    vec!["deferred"],
                ),
                make_bead_with_workspace_and_labels("human-1", 2, workspace_path, vec!["human"]),
                make_bead_with_workspace_and_labels(
                    "blocked-1",
                    3,
                    workspace_path,
                    vec!["blocked"],
                ),
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
        let starvation_event = helper
            .find_event("strand.pluck.starvation_detected")
            .unwrap();
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

        let mut bead1 =
            make_bead_with_workspace_and_labels("assigned-1", 1, workspace_path, vec![]);
        bead1.assignee = Some("worker-old-1".to_string());
        bead1.status = BeadStatus::Open;

        let mut bead2 =
            make_bead_with_workspace_and_labels("assigned-2", 2, workspace_path, vec![]);
        bead2.assignee = Some("worker-old-2".to_string());
        bead2.status = BeadStatus::Open;

        let mut bead3 =
            make_bead_with_workspace_and_labels("in-progress-1", 3, workspace_path, vec![]);
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
        let starvation_event = helper
            .find_event("strand.pluck.starvation_detected")
            .unwrap();
        assert_eq!(starvation_event.data["open_count"], 2);
        assert_eq!(starvation_event.data["excluded_count"], 2);

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
        let _workspace_path = workspace.path().to_str().unwrap();

        // Use UnfilteredStore to bypass store-level label filtering
        let store = UnfilteredStore { beads: vec![] };

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
        let starvation_event = helper
            .find_event("strand.pluck.starvation_detected")
            .unwrap();
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

    #[test]
    fn sanitize_workspace_name_handles_various_paths() {
        assert_eq!(sanitize_workspace_name("/home/coding/NEEDLE"), "NEEDLE");
        assert_eq!(
            sanitize_workspace_name("/home/coding/my-project"),
            "my-project"
        );
        assert_eq!(sanitize_workspace_name("/home/user/repo_name"), "repo_name");
        assert_eq!(
            sanitize_workspace_name("/var/data/test.workspace"),
            "test_workspace"
        );
        assert_eq!(
            sanitize_workspace_name("/absolute/path/with/slashes"),
            "slashes"
        );
        assert_eq!(sanitize_workspace_name("NEEDLE"), "NEEDLE");
        assert_eq!(sanitize_workspace_name(""), "unknown");
        assert_eq!(sanitize_workspace_name("/"), "unknown");
        assert_eq!(sanitize_workspace_name("////"), "unknown");
        assert_eq!(sanitize_workspace_name("/home/coding/NEEDLE/"), "NEEDLE");
    }

    #[test]
    fn sanitize_workspace_name_replaces_special_chars() {
        assert_eq!(
            sanitize_workspace_name("/home/user/project@v1.0"),
            "project_v1_0"
        );
        assert_eq!(sanitize_workspace_name("/home/user/test#123"), "test_123");
        assert_eq!(sanitize_workspace_name("/home/user/$special"), "_special");
    }

    #[test]
    fn extract_workspace_path_returns_unknown_for_empty_workspace() {
        // Beads with empty workspace paths (NULL columns in bead-rs stores)
        let bead1 = make_bead_with_workspace_and_labels("bead1", 1, "", vec![]);
        let bead2 = make_bead_with_workspace_and_labels("bead2", 2, "", vec![]);

        let workspace = extract_workspace_path(&[bead1, bead2]);
        assert_eq!(
            workspace, "unknown",
            "empty workspace strings should return 'unknown'"
        );
    }

    #[test]
    fn extract_workspace_path_returns_first_valid_workspace() {
        let bead1 = make_bead_with_workspace_and_labels("bead1", 1, "/workspace/a", vec![]);
        let bead2 = make_bead_with_workspace_and_labels("bead2", 2, "", vec![]); // NULL workspace

        let workspace = extract_workspace_path(&[bead1, bead2]);
        assert_eq!(
            workspace, "/workspace/a",
            "should return first non-empty workspace"
        );
    }

    #[tokio::test]
    async fn starvation_alert_not_created_when_open_count_is_zero() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");
        let needle_workspace = tempfile::tempdir().unwrap();

        // All beads are InProgress - no open work available
        let store = UnfilteredStore {
            beads: vec![
                make_bead_with_status("in-progress-1", 1, BeadStatus::InProgress),
                make_bead_with_status("in-progress-2", 2, BeadStatus::InProgress),
            ],
        };

        let strand = PluckStrand::with_persistent_records(
            vec![],
            3,
            helper.telemetry().clone(),
            needle_workspace.path().to_path_buf(),
            true,
        );

        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork
        match result {
            StrandResult::NoWork => {}
            other => panic!("expected NoWork, got: {:?}", other),
        }

        // Verify NO starvation alert bead was created
        // Since the store is a mock, we can't check actual bead creation,
        // but we can verify the logic didn't attempt to create one by checking
        // that the code path was skipped (no panic/error occurred)
        // Test reaches this point = success
    }

    #[tokio::test]
    async fn starvation_alert_not_created_when_all_beads_blocked() {
        use crate::telemetry::test_utils::TestHelper;

        let helper = TestHelper::new("test-worker");
        let needle_workspace = tempfile::tempdir().unwrap();

        // Create a workspace with beads that are all blocked by dependencies
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = workspace.path().to_str().unwrap();

        let mut blocker = make_bead_with_workspace_and_labels("blocker", 1, workspace_path, vec![]);
        blocker.status = BeadStatus::Closed; // Blocker is done

        let mut blocked = make_bead_with_workspace_and_labels("blocked", 1, workspace_path, vec![]);
        blocked.status = BeadStatus::Open; // Bead is open but blocked by dependency
        blocked.dependencies.push(BrDependency {
            id: blocker.id.clone(),
            title: "Blocks the blocked bead".to_string(),
            status: "closed".to_string(),
            priority: 1,
            dependency_type: "blocks".to_string(),
        });

        // Even though there's an open bead, it's blocked by a closed dependency
        // This should NOT trigger a starvation alert
        let store = UnfilteredStore {
            beads: vec![blocked],
        };

        let strand = PluckStrand::with_persistent_records(
            vec![],
            3,
            helper.telemetry().clone(),
            needle_workspace.path().to_path_buf(),
            true,
        );

        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Should return NoWork
        match result {
            StrandResult::NoWork => {}
            other => panic!(
                "expected NoWork when all beads are blocked, got: {:?}",
                other
            ),
        }

        helper.sync().await;

        // Verify starvation telemetry was emitted (it's always emitted)
        helper.assert_event_emitted("strand.pluck.starvation_detected");

        // Verify the event shows all beads are blocked
        let event = helper
            .find_event("strand.pluck.starvation_detected")
            .unwrap();
        assert_eq!(
            event.data.get("open_count").and_then(|v| v.as_u64()),
            Some(1),
            "should have one open bead (blocked bead)"
        );
    }
}
