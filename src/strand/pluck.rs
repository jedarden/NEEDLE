//! Pluck strand: primary bead selection from the assigned workspace.
//!
//! Pluck handles >90% of all bead processing. It queries the bead store for
//! unassigned, ready beads, filters by excluded labels, and sorts them in
//! deterministic priority order: `(priority ASC, created_at ASC, id ASC)`.
//!
//! Given the same queue state, every worker computes the same candidate list.

use crate::bead_store::{BeadStore, Filters};
use crate::types::{Bead, StrandError, StrandResult};

/// Default labels excluded from Pluck selection when not configured.
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];

/// The Pluck strand — primary work selection.
pub struct PluckStrand {
    /// Labels to exclude from candidate selection.
    exclude_labels: Vec<String>,
}

impl PluckStrand {
    /// Create a new PluckStrand with the given exclude labels.
    ///
    /// If `exclude_labels` is empty, the default set (`deferred`, `human`,
    /// `blocked`) is used.
    pub fn new(exclude_labels: Vec<String>) -> Self {
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
        }
    }

    /// Sort candidates in deterministic priority order.
    ///
    /// Sort key: `(priority ASC, created_at ASC, id ASC)`.
    /// The id tie-breaker ensures identical ordering across platforms.
    fn sort_candidates(candidates: &mut [Bead]) {
        candidates.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
                .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
        });
    }
}

#[async_trait::async_trait]
impl super::Strand for PluckStrand {
    fn name(&self) -> &str {
        "pluck"
    }

    async fn evaluate(&self, store: &dyn BeadStore) -> StrandResult {
        // 1. Query bead store for ready, unassigned beads.
        let filters = Filters {
            assignee: None,
            exclude_labels: self.exclude_labels.clone(),
        };

        let mut candidates = match store.ready(&filters).await {
            Ok(beads) => beads,
            Err(e) => {
                // Bead store error is semantically different from NoWork.
                return StrandResult::Error(StrandError::StoreError(e));
            }
        };

        // 2. Filter: remove beads that are already assigned (belt-and-suspenders;
        //    the store should already filter these, but we verify).
        candidates.retain(|b| b.assignee.is_none());

        // 3. Sort: deterministic (priority, created_at, id).
        Self::sort_candidates(&mut candidates);

        // 4. Return result.
        if candidates.is_empty() {
            StrandResult::NoWork
        } else {
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

        let strand = PluckStrand::new(vec![]);
        let result = strand.evaluate(&store).await;

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

        let strand = PluckStrand::new(vec![]);
        let result = strand.evaluate(&store).await;

        match result {
            StrandResult::BeadFound(beads) => {
                let ids: Vec<&str> = beads.iter().map(|b| b.id.as_ref()).collect();
                assert_eq!(ids, vec!["aaa", "bbb", "ccc"]);
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

        let strand = PluckStrand::new(vec![]); // Uses default excludes
        let result = strand.evaluate(&store).await;

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
        let strand = PluckStrand::new(vec!["wip".to_string()]);
        let result = strand.evaluate(&store).await;

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
    async fn assigned_beads_are_filtered_out() {
        let store = MemoryStore {
            beads: vec![
                make_bead_with_assignee("assigned", "worker-1"),
                make_bead("unassigned", 1, "2026-01-01 00:00:00"),
            ],
        };

        let strand = PluckStrand::new(vec![]);
        let result = strand.evaluate(&store).await;

        match result {
            StrandResult::BeadFound(beads) => {
                assert_eq!(beads.len(), 1);
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
        let strand = PluckStrand::new(vec![]);
        let result = strand.evaluate(&store).await;

        match result {
            StrandResult::NoWork => {}
            other => panic!("expected NoWork, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn store_error_returns_error_not_no_work() {
        let store = FailingStore;
        let strand = PluckStrand::new(vec![]);
        let result = strand.evaluate(&store).await;

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

        let strand = PluckStrand::new(vec![]);

        let store1 = MemoryStore {
            beads: beads.clone(),
        };
        let store2 = MemoryStore { beads };

        let r1 = strand.evaluate(&store1).await;
        let r2 = strand.evaluate(&store2).await;

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
        let strand = PluckStrand::new(vec![]);
        assert_eq!(strand.name(), "pluck");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Default exclude labels
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn default_exclude_labels_applied_when_empty() {
        let strand = PluckStrand::new(vec![]);
        assert_eq!(strand.exclude_labels, vec!["deferred", "human", "blocked"]);
    }

    #[test]
    fn custom_exclude_labels_used_when_provided() {
        let strand = PluckStrand::new(vec!["custom".to_string()]);
        assert_eq!(strand.exclude_labels, vec!["custom"]);
    }
}
