//! Knot strand: exhaustion alerting with three-state verification.
//!
//! When all other strands return NoWork, Knot diagnoses why:
//! - NO_BEADS_EXIST: queue is genuinely empty (normal idle)
//! - ALL_CLAIMED: other workers hold every bead (normal contention)
//! - INVISIBLE: open beads exist but Pluck's filters excluded them (config error)
//!
//! Only the INVISIBLE diagnosis triggers a starvation telemetry event. Rate-limited to one
//! alert per workspace per `config.knot.alert_cooldown_minutes`.
//!
//! The verification query uses `list_all()` — a DIFFERENT code path from
//! Pluck's `ready()` — to avoid v1's 100% false positive rate.

use std::sync::Mutex;

use chrono::{DateTime, Utc};

use crate::bead_store::BeadStore;
use crate::config::KnotConfig;
use crate::telemetry::Telemetry;
use crate::types::{BeadStatus, StrandError, StrandResult};

/// Diagnosis from the three-state verification check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExhaustionDiagnosis {
    /// Queue is genuinely empty — no beads at all.
    NoBeadsExist,
    /// All beads are claimed by workers (in_progress). Normal contention.
    AllClaimed {
        in_progress_count: usize,
        claimed_by: Vec<String>,
    },
    /// Open beads exist but Pluck found none — configuration error (filters, workspace).
    Invisible {
        total: usize,
        open_count: usize,
        in_progress_count: usize,
        claimed_by: Vec<String>,
    },
}

impl ExhaustionDiagnosis {
    /// Return the diagnosis as a string for telemetry.
    pub fn as_str(&self) -> &'static str {
        match self {
            ExhaustionDiagnosis::NoBeadsExist => "no_beads_exist",
            ExhaustionDiagnosis::AllClaimed { .. } => "all_claimed",
            ExhaustionDiagnosis::Invisible { .. } => "invisible",
        }
    }
}

/// The Knot strand — exhaustion alerting with three-state verification.
pub struct KnotStrand {
    config: KnotConfig,
    /// How many consecutive exhaustion cycles have occurred.
    exhaustion_count: Mutex<u64>,
    /// Timestamp of the last alert emitted (for rate limiting).
    last_alert_at: Mutex<Option<DateTime<Utc>>>,
    /// Telemetry emitter for starvation events.
    telemetry: Telemetry,
}

impl KnotStrand {
    /// Create a new KnotStrand with the given configuration and telemetry.
    pub fn new(config: KnotConfig, telemetry: Telemetry) -> Self {
        KnotStrand {
            config,
            exhaustion_count: Mutex::new(0),
            last_alert_at: Mutex::new(None),
            telemetry,
        }
    }

    /// Perform three-state verification using a DIFFERENT code path from Pluck.
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
        let mut in_progress_count = 0usize;
        let mut claimed_by = Vec::new();

        for bead in &all_beads {
            match bead.status {
                BeadStatus::Open => {
                    open_count += 1;
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

        // Open beads exist but Pluck returned nothing → config error.
        Ok(ExhaustionDiagnosis::Invisible {
            total,
            open_count,
            in_progress_count,
            claimed_by,
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
}

#[async_trait::async_trait]
impl super::Strand for KnotStrand {
    fn name(&self) -> &str {
        "knot"
    }

    async fn evaluate(&self, store: &dyn BeadStore) -> StrandResult {
        let cycle = self.increment_exhaustion();

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
            needle.strand.exhaustion_count = cycle,
        );
        let _knot_enter = knot_span.enter();

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

        // Record the diagnosis result on the span
        tracing::Span::current().record("needle.strand.result", "no_work");
        tracing::Span::current().record("needle.strand.diagnosis", diagnosis.as_str());

        tracing::info!(
            strand = "knot",
            diagnosis = diagnosis.as_str(),
            cycle,
            "knot strand evaluated"
        );

        // Only emit telemetry for INVISIBLE diagnosis and only after threshold.
        if let ExhaustionDiagnosis::Invisible {
            total,
            open_count,
            in_progress_count,
            claimed_by,
        } = &diagnosis
        {
            if cycle >= self.config.exhaustion_threshold && !self.is_within_cooldown() {
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

                // Extract workspace path from beads for telemetry.
                // All beads in a single store should have the same workspace.
                let workspace_path = if let Ok(beads) = store.list_all().await {
                    beads
                        .first()
                        .and_then(|b| b.workspace.to_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| "unknown".to_string())
                } else {
                    "unknown".to_string()
                };

                // Emit telemetry instead of creating a bead.
                let _ = self
                    .telemetry
                    .emit(crate::telemetry::EventKind::PluckStarvationDetected {
                        workspace: workspace_path,
                        open_count: *open_count,
                        excluded_count,
                        candidate_exclusion_reasons,
                    });

                self.record_alert();
                tracing::warn!(
                    diagnosis = diagnosis.as_str(),
                    open_count,
                    excluded_count,
                    "knot emitted starvation telemetry"
                );
            }
        }

        // Knot never produces work — always returns NoWork.
        StrandResult::NoWork
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
            let result = knot.evaluate(&store).await;
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
            let result = knot.evaluate(&store).await;
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
            let result = knot.evaluate(&store).await;
            assert!(matches!(result, StrandResult::NoWork));
        }
        assert_eq!(
            events.lock().unwrap().len(),
            0,
            "no telemetry below threshold"
        );
        assert_eq!(store.created_count(), 0, "no beads created below threshold");

        // Third cycle: hits threshold, telemetry emitted.
        let result = knot.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert_eq!(store.created_count(), 0, "no beads created at threshold");

        // Drop knot to close telemetry channel and flush all events.
        drop(knot);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events_guard = events.lock().unwrap();
        assert_eq!(events_guard.len(), 1, "telemetry emitted at threshold");
        let event = &events_guard[0];
        assert_eq!(event.event_type, "strand.pluck.starvation_detected");
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

        // First cycle: emits telemetry.
        knot.evaluate(&store).await;
        assert_eq!(store.created_count(), 0, "no beads created on first cycle");

        // Allow time for background telemetry task to process the event.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(
            events.lock().unwrap().len(),
            1,
            "telemetry emitted on first cycle"
        );

        // Second cycle: within cooldown, no new telemetry.
        knot.evaluate(&store).await;
        assert_eq!(store.created_count(), 0, "no beads created on second cycle");
        assert_eq!(
            events.lock().unwrap().len(),
            1,
            "rate limited — no second telemetry event"
        );

        // Third cycle: still within cooldown.
        knot.evaluate(&store).await;
        assert_eq!(store.created_count(), 0, "no beads created on third cycle");
        assert_eq!(events.lock().unwrap().len(), 1, "still rate limited");
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

        knot.evaluate(&store).await;

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

        assert_eq!(event.event_type, "strand.pluck.starvation_detected");
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
            let result = knot.evaluate(&store).await;
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

        let result = knot.evaluate(&store).await;
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
            } => {
                assert_eq!(total, 2);
                assert_eq!(open_count, 1);
                assert_eq!(in_progress_count, 1);
                assert_eq!(claimed_by, vec!["w1"]);
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

        let result = knot.evaluate(&store).await;
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

        let result = knot.evaluate(&store).await;

        assert!(
            matches!(result, StrandResult::Skipped { ref reason } if reason == "no_home_store"),
            "knot should return Skipped {{ reason: \"no_home_store\" }} when no .beads/ directory exists, got: {:?}",
            result
        );
    }
}
