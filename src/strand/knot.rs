//! Knot strand: exhaustion alerting with three-state verification.
//!
//! When all other strands return NoWork, Knot diagnoses why:
//! - NO_BEADS_EXIST: queue is genuinely empty (normal idle)
//! - ALL_CLAIMED: other workers hold every bead (normal contention)
//! - INVISIBLE: open beads exist but Pluck's filters excluded them (config error)
//!
//! Only the INVISIBLE diagnosis triggers an alert bead. Rate-limited to one
//! alert per workspace per `config.knot.alert_cooldown_minutes`.
//!
//! The verification query uses `list_all()` — a DIFFERENT code path from
//! Pluck's `ready()` — to avoid v1's 100% false positive rate.

use std::sync::Mutex;

use chrono::{DateTime, Utc};

use crate::bead_store::BeadStore;
use crate::config::KnotConfig;
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
    /// Timestamp of the last alert bead created (for rate limiting).
    last_alert_at: Mutex<Option<DateTime<Utc>>>,
}

impl KnotStrand {
    /// Create a new KnotStrand with the given configuration.
    pub fn new(config: KnotConfig) -> Self {
        KnotStrand {
            config,
            exhaustion_count: Mutex::new(0),
            last_alert_at: Mutex::new(None),
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
                BeadStatus::Done | BeadStatus::Blocked => {}
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

    /// Record that an alert was just created.
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

    /// Build the alert bead body with full diagnostics.
    fn build_alert_body(diagnosis: &ExhaustionDiagnosis, workspace: &str) -> String {
        match diagnosis {
            ExhaustionDiagnosis::Invisible {
                total,
                open_count,
                in_progress_count,
                claimed_by,
            } => {
                let claimers = if claimed_by.is_empty() {
                    "(none)".to_string()
                } else {
                    claimed_by.join(", ")
                };
                format!(
                    "## Starvation Alert\n\n\
                     Open beads exist but Pluck found none — possible configuration error.\n\n\
                     **Workspace:** {workspace}\n\
                     **Total beads:** {total}\n\
                     **Open:** {open_count}\n\
                     **In-progress:** {in_progress_count}\n\
                     **Claimed by:** {claimers}\n\n\
                     Check exclude_labels, workspace path, and filter configuration."
                )
            }
            // Alert is only created for Invisible diagnosis.
            ExhaustionDiagnosis::NoBeadsExist | ExhaustionDiagnosis::AllClaimed { .. } => {
                String::new()
            }
        }
    }
}

#[async_trait::async_trait]
impl super::Strand for KnotStrand {
    fn name(&self) -> &str {
        "knot"
    }

    async fn evaluate(&self, store: &dyn BeadStore) -> StrandResult {
        let cycle = self.increment_exhaustion();

        // Diagnose the exhaustion reason using a different code path from Pluck.
        let diagnosis = match self.diagnose(store).await {
            Ok(d) => d,
            Err(e) => return StrandResult::Error(e),
        };

        tracing::info!(
            strand = "knot",
            diagnosis = diagnosis.as_str(),
            cycle,
            "knot strand evaluated"
        );

        // Only create an alert for INVISIBLE diagnosis and only after threshold.
        if let ExhaustionDiagnosis::Invisible { .. } = &diagnosis {
            if cycle >= self.config.exhaustion_threshold && !self.is_within_cooldown() {
                let workspace = "default";
                let title = "Starvation alert: beads invisible to worker";
                let body = Self::build_alert_body(&diagnosis, workspace);

                match store.create_bead(title, &body, &["starvation-alert"]).await {
                    Ok(alert_id) => {
                        self.record_alert();
                        tracing::warn!(
                            alert_bead = %alert_id,
                            diagnosis = diagnosis.as_str(),
                            "knot created starvation alert bead"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "knot failed to create alert bead"
                        );
                    }
                }
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
    use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};

    use anyhow::Result;
    use chrono::{TimeZone, Utc};
    use std::path::PathBuf;
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
        async fn release(&self, _id: &BeadId) -> Result<()> {
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
        async fn release(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("store connection failed")
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

    use super::super::Strand;

    // ──────────────────────────────────────────────────────────────────────────
    // Three-state verification
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn no_beads_exist_returns_no_work_no_alert() {
        let store = KnotTestStore::new(vec![]);
        let knot = KnotStrand::new(default_knot_config());

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
        let knot = KnotStrand::new(default_knot_config());

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
    async fn invisible_creates_alert_after_threshold() {
        // Open beads exist but Pluck returned nothing → INVISIBLE.
        let store = KnotTestStore::new(vec![
            make_bead("open-1", BeadStatus::Open, None),
            make_bead("ip-1", BeadStatus::InProgress, Some("worker-1")),
        ]);
        let config = KnotConfig {
            exhaustion_threshold: 3,
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let knot = KnotStrand::new(config);

        // First two cycles: below threshold, no alert.
        for _ in 0..2 {
            let result = knot.evaluate(&store).await;
            assert!(matches!(result, StrandResult::NoWork));
        }
        assert_eq!(store.created_count(), 0, "no alert below threshold");

        // Third cycle: hits threshold, alert created.
        let result = knot.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert_eq!(store.created_count(), 1, "alert created at threshold");
    }

    #[tokio::test]
    async fn alert_rate_limited_within_cooldown() {
        let store = KnotTestStore::new(vec![make_bead("open-1", BeadStatus::Open, None)]);
        let config = KnotConfig {
            exhaustion_threshold: 1, // Alert after first cycle.
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let knot = KnotStrand::new(config);

        // First cycle: creates alert.
        knot.evaluate(&store).await;
        assert_eq!(store.created_count(), 1);

        // Second cycle: within cooldown, no new alert.
        knot.evaluate(&store).await;
        assert_eq!(store.created_count(), 1, "rate limited — no second alert");

        // Third cycle: still within cooldown.
        knot.evaluate(&store).await;
        assert_eq!(store.created_count(), 1, "still rate limited");
    }

    #[tokio::test]
    async fn alert_body_contains_diagnostics() {
        let store = KnotTestStore::new(vec![
            make_bead("open-1", BeadStatus::Open, None),
            make_bead("open-2", BeadStatus::Open, None),
            make_bead("ip-1", BeadStatus::InProgress, Some("worker-1")),
        ]);
        let config = KnotConfig {
            exhaustion_threshold: 1,
            alert_cooldown_minutes: 60,
            ..default_knot_config()
        };
        let knot = KnotStrand::new(config);

        knot.evaluate(&store).await;
        assert_eq!(store.created_count(), 1);

        let created = store.created_beads.lock().unwrap();
        let (title, body, labels) = &created[0];
        assert!(title.contains("Starvation alert"));
        assert!(body.contains("**Total beads:** 3"), "body: {body}");
        assert!(body.contains("**Open:** 2"), "body: {body}");
        assert!(body.contains("**In-progress:** 1"), "body: {body}");
        assert!(body.contains("worker-1"), "body: {body}");
        assert!(labels.contains(&"starvation-alert".to_string()));
    }

    #[tokio::test]
    async fn all_done_or_blocked_is_no_beads_exist() {
        // Only Done and Blocked beads — no open or in-progress.
        let store = KnotTestStore::new(vec![
            make_bead("d1", BeadStatus::Done, None),
            make_bead("bl1", BeadStatus::Blocked, None),
        ]);
        let knot = KnotStrand::new(default_knot_config());

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
        let knot = KnotStrand::new(default_knot_config());

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
        let knot = KnotStrand::new(default_knot_config());
        let diagnosis = knot.diagnose(&store).await.unwrap();
        assert_eq!(diagnosis, ExhaustionDiagnosis::NoBeadsExist);
    }

    #[tokio::test]
    async fn diagnose_all_claimed() {
        let store = KnotTestStore::new(vec![
            make_bead("b1", BeadStatus::InProgress, Some("w1")),
            make_bead("b2", BeadStatus::InProgress, Some("w2")),
        ]);
        let knot = KnotStrand::new(default_knot_config());
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
        let knot = KnotStrand::new(default_knot_config());
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
        let knot = KnotStrand::new(default_knot_config());
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
        let knot = KnotStrand::new(default_knot_config());
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
        let knot = KnotStrand::new(config);

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
        let knot = KnotStrand::new(default_knot_config());
        let diagnosis = knot.diagnose(&store).await.unwrap();
        match diagnosis {
            ExhaustionDiagnosis::AllClaimed { claimed_by, .. } => {
                assert_eq!(claimed_by.len(), 2, "should deduplicate workers");
            }
            other => panic!("expected AllClaimed, got: {other:?}"),
        }
    }
}
