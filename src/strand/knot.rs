//! Knot strand: terminal starvation telemetry with independent verification.
//!
//! When all other strands return NoWork, Knot diagnoses why:
//! - NO_BEADS_EXIST: queue is genuinely empty (normal idle)
//! - ALL_CLAIMED: other workers hold every bead (normal contention)
//! - BLOCKED_ONLY: every open bead has an unfinished dependency (normal idle)
//! - INVISIBLE: unblocked open beads exist but Pluck's filters excluded them
//!
//! Only the INVISIBLE diagnosis triggers a starvation telemetry event. Rate-limited to one
//! alert per workspace per `config.knot.alert_cooldown_minutes`.
//!
//! The verification query uses `list_all()` — a DIFFERENT code path from
//! Pluck's `ready()` — to avoid v1's 100% false positive rate.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use tracing::Instrument;

use crate::bead_store::BeadStore;
use crate::config::KnotConfig;
use crate::telemetry::Telemetry;
use crate::types::{BeadId, BeadStatus, StrandError, StrandResult};

/// Diagnosis from the terminal verification check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExhaustionDiagnosis {
    /// Queue is genuinely empty — no beads at all.
    NoBeadsExist,
    /// All beads are claimed by workers (in_progress). Normal contention.
    AllClaimed {
        in_progress_count: usize,
        claimed_by: Vec<String>,
    },
    /// Open beads exist, but every one is waiting on an unfinished blocker.
    BlockedOnly { open_count: usize },
    /// Open beads exist but Pluck found none — configuration error (filters, workspace).
    Invisible {
        total: usize,
        open_count: usize,
        in_progress_count: usize,
        claimed_by: Vec<String>,
        workspace: String,
    },
}

impl ExhaustionDiagnosis {
    /// Return the diagnosis as a string for telemetry.
    pub fn as_str(&self) -> &'static str {
        match self {
            ExhaustionDiagnosis::NoBeadsExist => "no_beads_exist",
            ExhaustionDiagnosis::AllClaimed { .. } => "all_claimed",
            ExhaustionDiagnosis::BlockedOnly { .. } => "blocked_only",
            ExhaustionDiagnosis::Invisible { .. } => "invisible",
        }
    }
}

/// The Knot strand — terminal starvation telemetry with independent verification.
pub struct KnotStrand {
    config: KnotConfig,
    /// Configured home workspace used when bead-rs inventory rows do not carry
    /// their workspace path. Falls back to the explicit fleet scope.
    workspace_scope: PathBuf,
    /// How many consecutive exhaustion cycles have occurred.
    exhaustion_count: Mutex<u64>,
    /// Timestamp of the last alert emitted (for rate limiting).
    last_alert_at: Mutex<Option<DateTime<Utc>>>,
    /// Timestamp of the first starvation detection (for transient gap backoff).
    first_starvation_detected_at: Mutex<Option<DateTime<Utc>>>,
    /// Telemetry emitter for starvation events.
    telemetry: Telemetry,
}

impl KnotStrand {
    /// Create a new KnotStrand with the given configuration and telemetry.
    pub fn new(config: KnotConfig, telemetry: Telemetry) -> Self {
        Self::with_workspace(config, telemetry, PathBuf::from("fleet"))
    }

    /// Create a Knot strand with a non-empty terminal event scope.
    pub fn with_workspace(config: KnotConfig, telemetry: Telemetry, workspace: PathBuf) -> Self {
        KnotStrand {
            config,
            workspace_scope: workspace,
            exhaustion_count: Mutex::new(0),
            last_alert_at: Mutex::new(None),
            first_starvation_detected_at: Mutex::new(None),
            telemetry,
        }
    }

    fn workspace_from_inventory(&self, beads: &[crate::types::Bead]) -> String {
        beads
            .iter()
            .map(|bead| bead.workspace.as_path())
            .find(|workspace| !workspace.as_os_str().is_empty())
            .or_else(|| {
                let configured = self.workspace_scope.as_path();
                (!configured.as_os_str().is_empty()).then_some(configured)
            })
            .unwrap_or_else(|| Path::new("fleet"))
            .display()
            .to_string()
    }

    /// Perform terminal verification using a DIFFERENT code path from Pluck.
    ///
    /// Queries ALL beads via `list_all()` (not `ready()`) and classifies the
    /// exhaustion reason.
    async fn diagnose(&self, store: &dyn BeadStore) -> Result<ExhaustionDiagnosis, StrandError> {
        let all_beads = store.list_all().await.map_err(StrandError::StoreError)?;

        if all_beads.is_empty() {
            return Ok(ExhaustionDiagnosis::NoBeadsExist);
        }

        let total = all_beads.len();
        let mut open_count = 0usize;
        let mut unblocked_open_count = 0usize;
        let mut in_progress_count = 0usize;
        let mut claimed_by = Vec::new();

        for bead in &all_beads {
            match bead.status {
                BeadStatus::Open => {
                    open_count += 1;
                    let has_unfinished_blocker = bead.dependencies.iter().any(|dependency| {
                        if dependency.dependency_type != "blocks" {
                            return false;
                        }

                        let enriched_status = dependency.status.to_ascii_lowercase();
                        if matches!(
                            enriched_status.as_str(),
                            "closed" | "done" | "completed" | "resolved"
                        ) {
                            return false;
                        }
                        if !enriched_status.is_empty() {
                            return true;
                        }

                        // bead-rs returns lean dependency edges without status.
                        // Resolve those against the inventory; an absent blocker
                        // is conservatively treated as unfinished.
                        all_beads
                            .iter()
                            .find(|candidate| candidate.id == dependency.id)
                            .map_or(true, |blocker| {
                                !matches!(blocker.status, BeadStatus::Done | BeadStatus::Closed)
                            })
                    });
                    if !has_unfinished_blocker {
                        unblocked_open_count += 1;
                    }
                }
                BeadStatus::InProgress => {
                    in_progress_count += 1;
                    if let Some(ref assignee) = bead.assignee {
                        if !claimed_by.contains(assignee) {
                            claimed_by.push(assignee.clone());
                        }
                    }
                }
                BeadStatus::Done | BeadStatus::Closed | BeadStatus::Blocked => {}
                // Non-exhaustive: treat unknown statuses as neither open nor in_progress.
                #[allow(unreachable_patterns)]
                _ => {}
            }
        }

        // If no open beads remain but some are in_progress, it's normal contention.
        if open_count == 0 && in_progress_count > 0 {
            return Ok(ExhaustionDiagnosis::AllClaimed {
                in_progress_count,
                claimed_by,
            });
        }

        // If no open beads AND no in_progress, everything is Done/Blocked — genuinely idle.
        if open_count == 0 && in_progress_count == 0 {
            return Ok(ExhaustionDiagnosis::NoBeadsExist);
        }

        // The ready queue is expected to omit dependency-blocked open beads.
        // This is normal idle, not evidence that Pluck hid runnable work.
        if open_count > 0 && unblocked_open_count == 0 {
            return Ok(ExhaustionDiagnosis::BlockedOnly { open_count });
        }

        // Open beads exist but Pluck returned nothing → config error.
        Ok(ExhaustionDiagnosis::Invisible {
            total,
            open_count,
            in_progress_count,
            claimed_by,
            workspace: self.workspace_from_inventory(&all_beads),
        })
    }

    /// Check whether we're within the alert cooldown window.
    fn is_within_cooldown(&self) -> bool {
        let guard = self.last_alert_at.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(last) = *guard {
            let cooldown = chrono::Duration::minutes(self.config.alert_cooldown_minutes as i64);
            Utc::now() - last < cooldown
        } else {
            false
        }
    }

    /// Record that an alert was just emitted.
    fn record_alert(&self) {
        let mut guard = self.last_alert_at.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(Utc::now());
    }

    /// Increment the exhaustion counter and return the new value.
    fn increment_exhaustion(&self) -> u64 {
        let mut guard = self
            .exhaustion_count
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard += 1;
        *guard
    }

    fn reset_exhaustion(&self) {
        let mut guard = self
            .exhaustion_count
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = 0;
    }

    /// Check whether we're within the transient gap backoff window.
    ///
    /// Returns true if we're still waiting for the backoff period to elapse.
    fn is_within_backoff_window(&self) -> bool {
        let guard = self
            .first_starvation_detected_at
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(first_detected) = *guard {
            // Use a random backoff between 5-15 minutes (jittered)
            let backoff_minutes = 5 + (first_detected.timestamp() % 10); // 5-14 minutes based on timestamp
            let backoff_duration = chrono::Duration::minutes(backoff_minutes);
            Utc::now() - first_detected < backoff_duration
        } else {
            false
        }
    }

    /// Record that starvation was first detected (start the backoff window).
    fn record_first_starvation_detection(&self) {
        let mut guard = self
            .first_starvation_detected_at
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = Some(Utc::now());
        }
    }

    /// Clear the starvation detection timestamp (when condition resolves).
    fn clear_starvation_detection(&self) {
        let mut guard = self
            .first_starvation_detected_at
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }

    /// Get the backoff window duration in minutes (for logging).
    fn backoff_window_minutes(&self) -> i64 {
        let guard = self
            .first_starvation_detected_at
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(first_detected) = *guard {
            5 + (first_detected.timestamp() % 10)
        } else {
            0
        }
    }

    /// Run cross-repo precondition validation before emitting starvation alert.
    ///
    /// Returns (beads_marked, success, details) where:
    /// - beads_marked: number of beads marked as manual_blocked
    /// - success: whether validation ran without error
    /// - details: human-readable description of what happened
    fn run_cross_repo_validation(&self, workspace_path: &Path) -> (usize, bool, String) {
        // Try to find the validation script
        // First, check if SEAM workspace exists and has the script
        let seam_path = Path::new("/home/coding/SEAM");
        let script_path = seam_path.join("tools/validate_cross_repo_preconditions.sh");

        if !script_path.exists() {
            return (
                0,
                true,
                "Cross-repo validation script not found, skipping".to_string(),
            );
        }

        // Run the validation script in the workspace directory
        let output = match Command::new(&script_path)
            .arg("--verbose")
            .current_dir(workspace_path)
            .output()
        {
            Ok(output) => output,
            Err(e) => return (0, false, format!("Failed to run validation script: {}", e)),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Parse output to count beads marked as manual_blocked
        // The script prints "✓ Marked <bead_id> as manual_blocked" for each bead
        let beads_marked = stdout
            .lines()
            .filter(|line| line.contains("Marked") && line.contains("as manual_blocked"))
            .count();

        let success = output.status.success();

        let details = if success {
            if beads_marked > 0 {
                format!(
                    "Cross-repo validation marked {} beads as manual_blocked",
                    beads_marked
                )
            } else {
                "Cross-repo validation completed, no unmet preconditions found".to_string()
            }
        } else {
            format!(
                "Cross-repo validation failed: {}",
                if stderr.is_empty() { &stdout } else { &stderr }
            )
        };

        (beads_marked, success, details)
    }
}

#[async_trait::async_trait]
impl super::Strand for KnotStrand {
    fn name(&self) -> &str {
        "knot"
    }

    async fn evaluate(&self, store: &dyn BeadStore, _exclusions: &HashSet<BeadId>) -> StrandResult {
        // Check if the home bead store exists. If not, skip this strand.
        // This distinguishes between "no home store configured" (expected for
        // roam-only workers) and "home store is broken" (unexpected error).
        if !store.has_valid_store() {
            tracing::info!(
                "Home workspace has no .beads/ directory — skipping Knot strand \
                 (expected for roam-only workers)"
            );
            return StrandResult::Skipped {
                reason: "no_home_store".to_string(),
            };
        }

        // Enter the strand.knot span for the exhaustion diagnosis.
        let knot_span = tracing::info_span!(
            "strand.knot",
            needle.strand.name = "knot",
            needle.strand.result = tracing::field::Empty, // Will be set based on diagnosis
            needle.strand.diagnosis = tracing::field::Empty, // Will be set based on diagnosis
            needle.strand.exhaustion_count = tracing::field::Empty,
        );

        async {
            // Diagnose the exhaustion reason using a different code path from Pluck.
            let diagnosis = match self.diagnose(store).await {
                Ok(d) => d,
                Err(e) => {
                    // Record error on the span
                    tracing::Span::current().record("needle.strand.result", "error");
                    tracing::Span::current().record("otel.status_code", 2u64);
                    tracing::Span::current().record("otel.status_description", format!("{e}"));
                    return StrandResult::Error(e);
                }
            };

            // Check if we had a previous starvation detection in backoff and the condition resolved
            let was_in_backoff = self.first_starvation_detected_at.lock().unwrap_or_else(|e| e.into_inner()).is_some();
            let is_now_invisible = matches!(diagnosis, ExhaustionDiagnosis::Invisible { .. });

            // If we were tracking a starvation detection and it's no longer Invisible, log as resolved
            if was_in_backoff && !is_now_invisible {
                tracing::info!(
                    strand = "knot",
                    diagnosis = diagnosis.as_str(),
                    "transient starvation gap resolved — condition self-corrected within backoff window"
                );
                self.clear_starvation_detection();
            }

            let cycle = if is_now_invisible {
                self.increment_exhaustion()
            } else {
                self.reset_exhaustion();
                0
            };

            // Record the diagnosis result on the span
            tracing::Span::current().record("needle.strand.result", "no_work");
            tracing::Span::current().record("needle.strand.diagnosis", diagnosis.as_str());
            tracing::Span::current().record("needle.strand.exhaustion_count", cycle);

            tracing::info!(
                strand = "knot",
                diagnosis = diagnosis.as_str(),
                cycle,
                "knot strand evaluated"
            );

            // Only emit telemetry for INVISIBLE diagnosis and only after threshold AND backoff window.
            if let ExhaustionDiagnosis::Invisible {
                total,
                open_count,
                in_progress_count,
                claimed_by,
                workspace,
            } = &diagnosis
            {
                if cycle >= self.config.exhaustion_threshold && !self.is_within_cooldown() {
                    // First time reaching threshold - record detection and enter backoff
                    if cycle == self.config.exhaustion_threshold {
                        self.record_first_starvation_detection();
                        tracing::info!(
                            workspace = %workspace,
                            diagnosis = diagnosis.as_str(),
                            open_count,
                            backoff_window_minutes = self.backoff_window_minutes(),
                            "starvation threshold reached — entering backoff window to confirm persistence"
                        );
                    } else if self.is_within_backoff_window() {
                        // Still within backoff window - continue waiting
                        tracing::info!(
                            workspace = %workspace,
                            diagnosis = diagnosis.as_str(),
                            open_count,
                            backoff_window_minutes = self.backoff_window_minutes(),
                            "still within backoff window, waiting to confirm persistence"
                        );
                    } else {
                        // Backoff window has elapsed — condition is persistent
                        tracing::info!(
                            workspace = %workspace,
                            diagnosis = diagnosis.as_str(),
                            open_count,
                            "starvation persisted through backoff window — running cross-repo precondition validation"
                        );

                        // Run cross-repo precondition validation before emitting alert
                        let workspace_path = Path::new(workspace);
                        let (beads_marked, validation_success, validation_details) =
                            self.run_cross_repo_validation(workspace_path);

                        tracing::info!(
                            workspace = %workspace,
                            diagnosis = diagnosis.as_str(),
                            open_count,
                            beads_marked,
                            validation_details,
                            "cross-repo validation completed"
                        );

                        // If validation marked beads as manual_blocked, the ready frontier
                        // is empty due to unmet cross-repo preconditions, not a system error.
                        // Log instead of emitting a starvation alert.
                        if beads_marked > 0 {
                            tracing::warn!(
                                workspace = %workspace,
                                diagnosis = diagnosis.as_str(),
                                open_count,
                                beads_marked,
                                "starvation due to unmet cross-repo preconditions — {} beads marked as manual_blocked, not emitting alert",
                                beads_marked
                            );
                            // Reset exhaustion tracking since this is a legitimate waiting state
                            self.reset_exhaustion();
                            self.clear_starvation_detection();
                            return StrandResult::NoWork;
                        }

                        // If validation failed but we're still starving, proceed with alert
                        // but note that validation could not run
                        if !validation_success {
                            tracing::warn!(
                                workspace = %workspace,
                                diagnosis = diagnosis.as_str(),
                                open_count,
                                validation_details,
                                "cross-repo validation failed, proceeding with starvation alert"
                            );
                        }

                        tracing::info!(
                            workspace = %workspace,
                            diagnosis = diagnosis.as_str(),
                            open_count,
                            "starvation confirmed after cross-repo validation — emitting telemetry"
                        );

                        // Calculate excluded beads: those that are neither open nor in progress.
                        let excluded_count = total - open_count - in_progress_count;

                        // Build candidate exclusion reasons from assignees holding in-progress beads.
                        let mut candidate_exclusion_reasons = Vec::new();
                        for worker in claimed_by {
                            candidate_exclusion_reasons.push(format!("held_by_{}", worker));
                        }
                        if excluded_count > 0 {
                            candidate_exclusion_reasons.push("excluded_by_status".to_string());
                        }

                        // Emit the sole terminal starvation verdict. Knot must not
                        // manufacture target-repository work: doing so feeds the
                        // scheduler its own control-plane artifact and can trigger
                        // recursive Unravel generation.
                        let _ = self.telemetry.emit(
                            crate::telemetry::EventKind::KnotStarvationDetected {
                                workspace: workspace.clone(),
                                open_count: *open_count,
                                excluded_count,
                                candidate_exclusion_reasons: candidate_exclusion_reasons.clone(),
                            },
                            Utc::now(),
                        );

                        self.record_alert();
                        // Clear the backoff tracking after emitting the alert
                        self.clear_starvation_detection();

                        tracing::warn!(
                            workspace = %workspace,
                            diagnosis = diagnosis.as_str(),
                            open_count,
                            excluded_count,
                            "knot emitted terminal starvation telemetry"
                        );
                    }
                }
            }

            // Knot never produces work — always returns NoWork.
            StrandResult::NoWork
        }
        .instrument(knot_span)
        .await
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{Filters, RepairReport};
    use crate::telemetry::TelemetryEvent;
    use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};

    use anyhow::Result;
    use chrono::{TimeZone, Utc};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;

    /// Configurable in-memory bead store for Knot testing.
    struct KnotTestStore {
        all_beads: Vec<Bead>,
        created_beads: StdMutex<Vec<(String, String, Vec<String>)>>,
    }

    impl KnotTestStore {
        fn new(beads: Vec<Bead>) -> Self {
            KnotTestStore {
                all_beads: beads,
                created_beads: StdMutex::new(vec![]),
            }
        }

        fn created_count(&self) -> usize {
            self.created_beads.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for KnotTestStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.all_beads.clone())
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            // Pluck would return empty — that's why Knot is being evaluated.
            Ok(vec![])
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("not implemented")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }

        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
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
        async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
            self.created_beads.lock().unwrap().push((
                title.to_string(),
                body.to_string(),
                labels.iter().map(|s| s.to_string()).collect(),
            ));
            Ok(BeadId::from("alert-001"))
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

        fn has_valid_store(&self) -> bool {
            true
        }
    }

    /// Failing store for error-path tests.
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

        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
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
            anyhow::bail!("store connection failed")
        }
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("store connection failed")
        }

        fn has_valid_store(&self) -> bool {
            true
        }
    }

    fn make_bead(id: &str, status: BeadStatus, assignee: Option<&str>) -> Bead {
        let dt = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
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

    fn default_knot_config() -> KnotConfig {
        KnotConfig {
            alert_destination: None,
            alert_cooldown_minutes: 60,
            exhaustion_threshold: 3,
        }
    }

    /// Create a KnotStrand with test defaults for telemetry.
    /// Returns the knot and the captured events for verification.
    fn make_test_knot_with_events(
        config: KnotConfig,
    ) -> (KnotStrand, Arc<StdMutex<Vec<TelemetryEvent>>>) {
        let (sink, events) = crate::telemetry::test_utils::MemorySink::new();
        let telemetry = crate::telemetry::Telemetry::with_sink("test-worker".to_string(), sink);
        let knot = KnotStrand::new(config, telemetry);
        (knot, events)
    }

    /// Create a KnotStrand with test defaults for telemetry (legacy, no event capture).
    fn make_test_knot(config: KnotConfig) -> KnotStrand {
        let telemetry = crate::telemetry::Telemetry::new("test-worker".to_string());
        KnotStrand::new(config, telemetry)
    }

    use super::super::Strand;

    // ──────────────────────────────────────────────────────────────────────────
    // Three-state verification
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn no_beads_exist_returns_no_work_no_alert() {
        let store = KnotTestStore::new(vec![]);
        let knot = make_test_knot(default_knot_config());

        // Run past threshold to ensure no alert for empty queue.
        for _ in 0..5 {
            let result = knot.evaluate(&store, &HashSet::new()).await;
            assert!(matches!(result, StrandResult::NoWork));
        }
        assert_eq!(
            store.created_count(),
            0,
            "no alert for genuinely empty queue"
        );
    }

    #[tokio::test]
    async fn all_claimed_returns_no_work_no_alert() {
        let store = KnotTestStore::new(vec![
            make_bead("b1", BeadStatus::InProgress, Some("worker-1")),
            make_bead("b2", BeadStatus::InProgress, Some("worker-2")),
        ]);
        let knot = make_test_knot(default_knot_config());

        for _ in 0..5 {
            let result = knot.evaluate(&store, &HashSet::new()).await;
            assert!(matches!(result, StrandResult::NoWork));
        }
        assert_eq!(
            store.created_count(),
            0,
            "no alert when all beads are claimed"
        );
    }

    #[tokio::test]
    async fn invisible_emits_telemetry_after_threshold() {
        // Open beads exist but Pluck returned nothing → INVISIBLE.
        let store = KnotTestStore::new(vec![
            make_bead("open-1", BeadStatus::Open, None),
            make_bead("ip-1", BeadStatus::InProgress, Some("worker-1")),
            make_bead("done-1", BeadStatus::Done, None), // excluded bead
        ]);
        let config = KnotConfig {
            exhaustion_threshold: 3,
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let (knot, events) = make_test_knot_with_events(config);

        // First two cycles: below threshold, no telemetry.
        for _ in 0..2 {
            let result = knot.evaluate(&store, &HashSet::new()).await;
            assert!(matches!(result, StrandResult::NoWork));
        }
        assert_eq!(
            events.lock().unwrap().len(),
            0,
            "no telemetry below threshold"
        );
        assert_eq!(store.created_count(), 0, "no beads created below threshold");

        // Third cycle: hits threshold, enters backoff window, no telemetry yet.
        let result = knot.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert_eq!(store.created_count(), 0, "no beads created at threshold");

        // Simulate backoff window elapse by clearing tracking
        *knot.first_starvation_detected_at.lock().unwrap() = None;

        // Fourth cycle: backoff elapsed, telemetry emitted.
        let result = knot.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));

        // Drop knot to close telemetry channel and flush all events.
        drop(knot);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events_guard = events.lock().unwrap();
        assert_eq!(
            events_guard.len(),
            1,
            "telemetry emitted after backoff elapsed"
        );
        let event = &events_guard[0];
        assert_eq!(event.event_type, "strand.knot.starvation_detected");
        assert_eq!(event.data["workspace"], "/tmp/test");
        assert_eq!(event.data["open_count"], 1);
        assert_eq!(event.data["excluded_count"], 1);
    }

    #[tokio::test]
    async fn alert_rate_limited_within_cooldown() {
        let store = KnotTestStore::new(vec![make_bead("open-1", BeadStatus::Open, None)]);
        let config = KnotConfig {
            exhaustion_threshold: 1, // Alert after first cycle.
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let (knot, events) = make_test_knot_with_events(config);

        // First cycle: enters backoff, no telemetry yet.
        knot.evaluate(&store, &HashSet::new()).await;
        assert_eq!(store.created_count(), 0, "no beads created on first cycle");

        // Simulate backoff window elapse
        *knot.first_starvation_detected_at.lock().unwrap() = None;

        // Second cycle: backoff elapsed, emits telemetry.
        knot.evaluate(&store, &HashSet::new()).await;
        assert_eq!(store.created_count(), 0, "no beads created on second cycle");

        // Allow time for background telemetry task to process the event.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(
            events.lock().unwrap().len(),
            1,
            "telemetry emitted after backoff elapsed"
        );

        // Third cycle: within cooldown, no new telemetry.
        knot.evaluate(&store, &HashSet::new()).await;
        assert_eq!(store.created_count(), 0, "no beads created on third cycle");
        assert_eq!(
            events.lock().unwrap().len(),
            1,
            "rate limited — no second telemetry event"
        );
    }

    #[tokio::test]
    async fn telemetry_contains_diagnostic_details() {
        let store = KnotTestStore::new(vec![
            make_bead("open-1", BeadStatus::Open, None),
            make_bead("open-2", BeadStatus::Open, None),
            make_bead("ip-1", BeadStatus::InProgress, Some("worker-1")),
            make_bead("done-1", BeadStatus::Done, None), // excluded bead
        ]);
        let config = KnotConfig {
            exhaustion_threshold: 1,
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let (knot, events) = make_test_knot_with_events(config);

        // First evaluation: enters backoff
        knot.evaluate(&store, &HashSet::new()).await;

        // Simulate backoff window elapse
        *knot.first_starvation_detected_at.lock().unwrap() = None;

        // Second evaluation: emits telemetry
        knot.evaluate(&store, &HashSet::new()).await;

        // Verify no bead was written to the target workspace
        assert_eq!(
            store.created_count(),
            0,
            "no beads should be written to target workspace"
        );

        // Drop knot to close telemetry channel and flush all events.
        drop(knot);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Verify telemetry event contains diagnostic details
        let events_guard = events.lock().unwrap();
        assert_eq!(events_guard.len(), 1, "telemetry event emitted");
        let event = &events_guard[0];

        assert_eq!(event.event_type, "strand.knot.starvation_detected");
        assert_eq!(event.data["open_count"], 2);
        assert_eq!(event.data["excluded_count"], 1);

        // Verify candidate exclusion reasons include the worker holding beads
        let reasons = event.data["candidate_exclusion_reasons"]
            .as_array()
            .expect("candidate_exclusion_reasons should be an array");
        assert!(reasons
            .iter()
            .any(|r| r.as_str().unwrap().contains("held_by_worker-1")));
    }

    #[tokio::test]
    async fn all_done_or_blocked_is_no_beads_exist() {
        // Only Done and Blocked beads — no open or in-progress.
        let store = KnotTestStore::new(vec![
            make_bead("d1", BeadStatus::Done, None),
            make_bead("bl1", BeadStatus::Blocked, None),
        ]);
        let knot = make_test_knot(default_knot_config());

        for _ in 0..5 {
            let result = knot.evaluate(&store, &HashSet::new()).await;
            assert!(matches!(result, StrandResult::NoWork));
        }
        assert_eq!(
            store.created_count(),
            0,
            "no alert when all beads are done/blocked"
        );
    }

    #[tokio::test]
    async fn store_error_returns_strand_error() {
        let store = FailingStore;
        let knot = make_test_knot(default_knot_config());

        let result = knot.evaluate(&store, &HashSet::new()).await;
        assert!(
            matches!(result, StrandResult::Error(StrandError::StoreError(_))),
            "expected StrandError::StoreError, got: {result:?}"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Diagnosis unit tests
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn diagnose_empty_queue() {
        let store = KnotTestStore::new(vec![]);
        let knot = make_test_knot(default_knot_config());
        let diagnosis = knot.diagnose(&store).await.unwrap();
        assert_eq!(diagnosis, ExhaustionDiagnosis::NoBeadsExist);
    }

    #[tokio::test]
    async fn diagnose_all_claimed() {
        let store = KnotTestStore::new(vec![
            make_bead("b1", BeadStatus::InProgress, Some("w1")),
            make_bead("b2", BeadStatus::InProgress, Some("w2")),
        ]);
        let knot = make_test_knot(default_knot_config());
        let diagnosis = knot.diagnose(&store).await.unwrap();
        match diagnosis {
            ExhaustionDiagnosis::AllClaimed {
                in_progress_count,
                claimed_by,
            } => {
                assert_eq!(in_progress_count, 2);
                assert!(claimed_by.contains(&"w1".to_string()));
                assert!(claimed_by.contains(&"w2".to_string()));
            }
            other => panic!("expected AllClaimed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn diagnose_invisible() {
        let store = KnotTestStore::new(vec![
            make_bead("open-1", BeadStatus::Open, None),
            make_bead("ip-1", BeadStatus::InProgress, Some("w1")),
        ]);
        let knot = make_test_knot(default_knot_config());
        let diagnosis = knot.diagnose(&store).await.unwrap();
        match diagnosis {
            ExhaustionDiagnosis::Invisible {
                total,
                open_count,
                in_progress_count,
                claimed_by,
                workspace,
            } => {
                assert_eq!(total, 2);
                assert_eq!(open_count, 1);
                assert_eq!(in_progress_count, 1);
                assert_eq!(claimed_by, vec!["w1"]);
                assert_eq!(workspace, "/tmp/test");
            }
            other => panic!("expected Invisible, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn diagnose_mixed_done_and_in_progress_is_all_claimed() {
        // Some Done, some InProgress, no Open → AllClaimed.
        let store = KnotTestStore::new(vec![
            make_bead("done-1", BeadStatus::Done, None),
            make_bead("ip-1", BeadStatus::InProgress, Some("w1")),
        ]);
        let knot = make_test_knot(default_knot_config());
        let diagnosis = knot.diagnose(&store).await.unwrap();
        assert!(
            matches!(diagnosis, ExhaustionDiagnosis::AllClaimed { .. }),
            "expected AllClaimed, got: {diagnosis:?}"
        );
    }

    #[tokio::test]
    async fn dependency_blocked_open_beads_are_normal_idle() {
        let mut waiting = make_bead("waiting", BeadStatus::Open, None);
        waiting.dependencies.push(crate::types::BrDependency {
            id: BeadId::from("blocker"),
            title: String::new(),
            status: String::new(),
            priority: 1,
            dependency_type: "blocks".to_string(),
        });
        let store = KnotTestStore::new(vec![
            waiting,
            make_bead("blocker", BeadStatus::InProgress, Some("worker-1")),
        ]);
        let config = KnotConfig {
            exhaustion_threshold: 1,
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let (knot, events) = make_test_knot_with_events(config);

        let result = knot.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert!(matches!(
            knot.diagnose(&store).await.unwrap(),
            ExhaustionDiagnosis::BlockedOnly { open_count: 1 }
        ));
        assert!(events.lock().unwrap().is_empty());
        assert_eq!(store.created_count(), 0);
    }

    #[tokio::test]
    async fn ordinary_idle_resets_consecutive_invisible_threshold() {
        let config = KnotConfig {
            exhaustion_threshold: 2,
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let (knot, events) = make_test_knot_with_events(config);
        let invisible = KnotTestStore::new(vec![make_bead("open", BeadStatus::Open, None)]);
        let empty = KnotTestStore::new(vec![]);

        knot.evaluate(&invisible, &HashSet::new()).await;
        knot.evaluate(&empty, &HashSet::new()).await;
        knot.evaluate(&invisible, &HashSet::new()).await;
        assert!(events.lock().unwrap().is_empty());

        // Fourth evaluation: hits threshold, enters backoff
        knot.evaluate(&invisible, &HashSet::new()).await;
        assert!(events.lock().unwrap().is_empty());

        // Simulate backoff elapse
        *knot.first_starvation_detected_at.lock().unwrap() = None;

        // Fifth evaluation: backoff elapsed, emits telemetry
        knot.evaluate(&invisible, &HashSet::new()).await;
        drop(knot);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(events.lock().unwrap().len(), 1);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Name
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn strand_name_is_knot() {
        let knot = make_test_knot(default_knot_config());
        assert_eq!(knot.name(), "knot");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Knot always returns NoWork
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn knot_always_returns_no_work() {
        // Even with invisible beads and alert creation, result is always NoWork.
        let store = KnotTestStore::new(vec![make_bead("open-1", BeadStatus::Open, None)]);
        let config = KnotConfig {
            exhaustion_threshold: 1,
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let knot = make_test_knot(config);

        let result = knot.evaluate(&store, &HashSet::new()).await;
        assert!(
            matches!(result, StrandResult::NoWork),
            "knot always returns NoWork"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Duplicate claimed_by deduplication
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn claimed_by_deduplicates_workers() {
        let store = KnotTestStore::new(vec![
            make_bead("b1", BeadStatus::InProgress, Some("w1")),
            make_bead("b2", BeadStatus::InProgress, Some("w1")),
            make_bead("b3", BeadStatus::InProgress, Some("w2")),
        ]);
        let knot = make_test_knot(default_knot_config());
        let diagnosis = knot.diagnose(&store).await.unwrap();
        match diagnosis {
            ExhaustionDiagnosis::AllClaimed { claimed_by, .. } => {
                assert_eq!(claimed_by.len(), 2, "should deduplicate workers");
            }
            other => panic!("expected AllClaimed, got: {other:?}"),
        }
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Regression test: no_home_store detection
    // ──────────────────────────────────────────────────────────────────────────

    /// Mock store that simulates a workspace without a .beads/ directory.
    struct NoHomeStoreMock;

    #[async_trait::async_trait]
    impl BeadStore for NoHomeStoreMock {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn block(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn flush(&self) -> Result<()> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn doctor_repair(&self) -> Result<RepairReport> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn full_rebuild(&self) -> Result<()> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            anyhow::bail!("no .beads/ directory")
        }
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("no .beads/ directory")
        }
        fn has_valid_store(&self) -> bool {
            false // Simulates workspace without .beads/ directory
        }
    }

    #[tokio::test]
    async fn no_home_store_returns_skipped() {
        // Regression test: verifies that knot returns Skipped with reason "no_home_store"
        // when the workspace has no .beads/ directory (expected for roam-only workers).
        let store = NoHomeStoreMock;
        let knot = make_test_knot(default_knot_config());

        let result = knot.evaluate(&store, &HashSet::new()).await;

        assert!(
            matches!(result, StrandResult::Skipped { ref reason } if reason == "no_home_store"),
            "knot should return Skipped {{ reason: \"no_home_store\" }} when no .beads/ directory exists, got: {:?}",
            result
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Transient starvation gap backoff tests
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn starvation_backoff_window_delays_telemetry() {
        // Verify that starvation detection enters backoff window and doesn't emit telemetry immediately.
        let store = KnotTestStore::new(vec![make_bead("open-1", BeadStatus::Open, None)]);
        let config = KnotConfig {
            exhaustion_threshold: 1, // Trigger after first cycle
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let (knot, events) = make_test_knot_with_events(config);

        // First cycle: hits threshold, enters backoff window, no telemetry yet
        let result = knot.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert_eq!(
            events.lock().unwrap().len(),
            0,
            "no telemetry during backoff window"
        );
        assert_eq!(store.created_count(), 0, "no beads created during backoff");

        // Still within backoff window — no telemetry
        let result = knot.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert_eq!(
            events.lock().unwrap().len(),
            0,
            "still no telemetry during backoff"
        );

        drop(knot);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            events.lock().unwrap().len(),
            0,
            "no telemetry emitted while in backoff"
        );
    }

    #[tokio::test]
    async fn starvation_resolves_within_backoff_window() {
        // Verify that when starvation condition resolves during backoff, it's logged as transient.
        let invisible_store = KnotTestStore::new(vec![make_bead("open-1", BeadStatus::Open, None)]);
        let resolved_store = KnotTestStore::new(vec![]);
        let config = KnotConfig {
            exhaustion_threshold: 1,
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let (knot, events) = make_test_knot_with_events(config);

        // First evaluation: starvation detected, enters backoff
        knot.evaluate(&invisible_store, &HashSet::new()).await;
        assert_eq!(
            events.lock().unwrap().len(),
            0,
            "no telemetry on first detection"
        );

        // Second evaluation: condition resolved (empty queue)
        let result = knot.evaluate(&resolved_store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert_eq!(
            events.lock().unwrap().len(),
            0,
            "no telemetry when condition resolves"
        );

        // Verify backoff tracking was cleared
        assert!(knot.first_starvation_detected_at.lock().unwrap().is_none());

        drop(knot);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            events.lock().unwrap().len(),
            0,
            "no starvation telemetry for resolved condition"
        );
    }

    #[tokio::test]
    async fn starvation_persists_past_backoff_window_emits_telemetry() {
        // Verify that persistent starvation beyond backoff window emits telemetry.
        // This test uses a mock that simulates time passage by manipulating the backoff check.
        let store = KnotTestStore::new(vec![make_bead("open-1", BeadStatus::Open, None)]);
        let config = KnotConfig {
            exhaustion_threshold: 1,
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };

        // We need to use a custom knot with manipulated time for this test
        // For now, we'll verify the logic with the minimum backoff
        let (knot, events) = make_test_knot_with_events(config);

        // First cycle: enters backoff
        knot.evaluate(&store, &HashSet::new()).await;

        // Manually clear the backoff tracking to simulate time passage
        // (In production, this would happen after 5-15 minutes)
        *knot.first_starvation_detected_at.lock().unwrap() = None;

        // Re-evaluate: backoff window cleared, should emit telemetry
        // But we need to increment exhaustion count past threshold again
        knot.evaluate(&store, &HashSet::new()).await;

        drop(knot);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Should have emitted telemetry after backoff "elapsed"
        assert_eq!(
            events.lock().unwrap().len(),
            1,
            "telemetry emitted after backoff elapsed"
        );
    }

    #[tokio::test]
    async fn backoff_tracking_cleared_after_telemetry_emitted() {
        // Verify that backoff tracking is cleared after telemetry is emitted.
        let store = KnotTestStore::new(vec![make_bead("open-1", BeadStatus::Open, None)]);
        let config = KnotConfig {
            exhaustion_threshold: 1,
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let (knot, events) = make_test_knot_with_events(config);

        // First cycle: enters backoff
        knot.evaluate(&store, &HashSet::new()).await;
        assert!(knot.first_starvation_detected_at.lock().unwrap().is_some());

        // Simulate backoff elapse by clearing and re-evaluating
        *knot.first_starvation_detected_at.lock().unwrap() = None;
        knot.evaluate(&store, &HashSet::new()).await;

        // Verify backoff was cleared before checking events
        let was_tracking_cleared = knot.first_starvation_detected_at.lock().unwrap().is_none();

        drop(knot);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(events.lock().unwrap().len(), 1, "telemetry emitted");
        assert!(
            was_tracking_cleared,
            "backoff tracking cleared after telemetry"
        );
    }
}
