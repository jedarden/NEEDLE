//! Focused behavioral contracts for the admission adapter (N-T54,
//! `needle-e830524d`; ADR-029 step 3).
//!
//! The adapter is where the loop touches the world. The properties that
//! matter are that it does not file beads for evidence somebody already owns,
//! that concurrent submissions of one signature produce one bead, and that
//! every refusal is recorded rather than dropped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::cli::improvements::read_decisions;
use needle::evidence_routing::LedgerRow;
use needle::improvement_controller::{
    plan_owners, submit, SubmissionContext, IMPROVEMENT_LABEL, SIGNATURE_LABEL_PREFIX,
};
use needle::learning::improvement::{
    AdmissionPolicy, EvidenceClass, GeneratorThresholds, WorkspaceImpactProfile,
};
use needle::types::{Bead, BeadId, ClaimResult};
use serde_json::json;

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap()
}

// ──────────────────────────────────────────────────────────────────────────
// An in-memory store. No process is spawned.
// ──────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct MemoryStore {
    beads: Mutex<Vec<Bead>>,
    created: Mutex<Vec<(String, String, Vec<String>)>>,
    next: Mutex<usize>,
}

impl MemoryStore {
    fn created(&self) -> Vec<(String, String, Vec<String>)> {
        self.created.lock().expect("lock").clone()
    }
}

#[async_trait]
impl BeadStore for MemoryStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }
    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(self.beads.lock().expect("lock").clone())
    }
    async fn show(&self, id: &BeadId) -> Result<Bead> {
        self.beads
            .lock()
            .expect("lock")
            .iter()
            .find(|bead| bead.id == *id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))
    }
    async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
        anyhow::bail!("not exercised")
    }
    async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
        anyhow::bail!("not exercised")
    }
    async fn release(&self, _id: &BeadId) -> Result<()> {
        anyhow::bail!("not exercised")
    }
    async fn block(&self, _id: &BeadId) -> Result<()> {
        anyhow::bail!("not exercised")
    }
    async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
        anyhow::bail!("not exercised")
    }
    async fn flush(&self) -> Result<()> {
        Ok(())
    }
    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        anyhow::bail!("not exercised")
    }
    async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }
    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }
    async fn append_notes(&self, _id: &BeadId, _note: &str) -> Result<()> {
        Ok(())
    }
    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        let mut next = self.next.lock().expect("lock");
        *next += 1;
        let id = BeadId::from(format!("improv-{next}"));
        self.created.lock().expect("lock").push((
            title.to_string(),
            body.to_string(),
            labels.iter().map(|label| label.to_string()).collect(),
        ));
        Ok(id)
    }
    async fn add_dependency(&self, _blocker: &BeadId, _blocked: &BeadId) -> Result<()> {
        anyhow::bail!("not exercised")
    }
    async fn remove_dependency(&self, _blocked: &BeadId, _blocker: &BeadId) -> Result<()> {
        anyhow::bail!("not exercised")
    }
    async fn doctor_repair(&self) -> Result<RepairReport> {
        anyhow::bail!("not exercised")
    }
    async fn doctor_check(&self) -> Result<RepairReport> {
        anyhow::bail!("not exercised")
    }
    async fn full_rebuild(&self) -> Result<()> {
        anyhow::bail!("not exercised")
    }
    fn has_valid_store(&self) -> bool {
        true
    }
}

/// A bead already carrying an improvement signature.
fn signed_bead(id: &str, signature: &str) -> Bead {
    serde_json::from_value(json!({
        "id": id,
        "title": id,
        "description": null,
        "priority": 1,
        "status": "open",
        "assignee": "",
        "labels": [format!("{SIGNATURE_LABEL_PREFIX}{signature}")],
        "source_repo": "/fixture-root/NEEDLE",
        "created_at": at(13).to_rfc3339(),
        "updated_at": at(13).to_rfc3339(),
    }))
    .expect("fixture bead deserializes")
}

// ──────────────────────────────────────────────────────────────────────────
// Ledger fixtures
// ──────────────────────────────────────────────────────────────────────────

/// One ledger row.
///
/// `costed` is a parameter because it decides which evidence classes a
/// fixture can produce: an uncosted row cannot prove a spend concentration
/// (its cost is unknown, never zero — ADR-030), so the red-baseline fixtures
/// below use uncosted rows to exercise that class alone.
#[allow(clippy::too_many_arguments)]
fn row(
    workspace: &str,
    adapter: &str,
    outcome: &str,
    bead: &str,
    attempt: &str,
    terminal_reason: &str,
    day: u32,
    costed: bool,
) -> LedgerRow {
    LedgerRow {
        timestamp: Some(at(day)),
        data: json!({
            "attempt_id": attempt,
            "bead_id": bead,
            "workspace": format!("/fixture-root/{workspace}"),
            "worker": "glm-roam-18",
            "adapter": adapter,
            "outcome": outcome,
            "costed": costed,
            "estimated_cost_usd": if costed { 4.0 } else { 0.0 },
            "terminal_reason": terminal_reason,
        }),
    }
}

/// One bead failing identically three times: the unchanged-retry class.
fn unchanged_retry_rows() -> Vec<LedgerRow> {
    (1..=3)
        .map(|n| {
            row(
                "NEEDLE",
                "claude-code-glm-5.3-flash",
                "work_failure",
                "needle-aaaa1111",
                &format!("a{n}"),
                "gate:cargo-test",
                11 + n,
                true,
            )
        })
        .collect()
}

/// A workspace with judged attempts and nothing verified: red baseline, a
/// class the plan has not already assigned.
fn red_baseline_rows() -> Vec<LedgerRow> {
    (0..12)
        .map(|n| {
            row(
                "commitgraph",
                "claude-code-glm-5.3-flash",
                "indeterminate",
                &format!("cg-{n}"),
                &format!("cg-a{n}"),
                "signal:9",
                13,
                false,
            )
        })
        .collect()
}

struct Harness {
    dir: tempfile::TempDir,
    store: std::sync::Arc<MemoryStore>,
}

impl Harness {
    fn new(beads: Vec<Bead>) -> Self {
        let store = std::sync::Arc::new(MemoryStore {
            beads: Mutex::new(beads),
            ..MemoryStore::default()
        });
        Harness {
            dir: tempfile::tempdir().expect("tempdir"),
            store,
        }
    }

    fn journal(&self) -> PathBuf {
        self.dir.path().join("improvements/decisions.jsonl")
    }

    async fn submit(
        &self,
        rows: &[LedgerRow],
        policy: AdmissionPolicy,
    ) -> Result<needle::improvement_controller::SubmissionSummary> {
        let workspaces: BTreeMap<String, PathBuf> = [
            ("NEEDLE".to_string(), PathBuf::from("/fixture-root/NEEDLE")),
            (
                "commitgraph".to_string(),
                PathBuf::from("/fixture-root/commitgraph"),
            ),
        ]
        .into_iter()
        .collect();
        let profiles: BTreeMap<String, WorkspaceImpactProfile> = BTreeMap::new();
        let home = PathBuf::from("/fixture-root/NEEDLE");

        let ctx = SubmissionContext {
            rows,
            thresholds: GeneratorThresholds::default(),
            policy,
            profiles: &profiles,
            workspaces: &workspaces,
            home_workspace: &home,
            now: at(14),
        };

        let store = self.store.clone();
        let open = move |_: &Path| -> Result<std::sync::Arc<dyn BeadStore>> {
            Ok(std::sync::Arc::new(SharedStore(store.clone())))
        };

        let (_, _, summary) = submit(&ctx, &open, &self.journal()).await?;
        Ok(summary)
    }
}

/// A handle that shares one MemoryStore across every opened path, so a test
/// can see everything the controller filed.
struct SharedStore(std::sync::Arc<MemoryStore>);

#[async_trait]
impl BeadStore for SharedStore {
    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
        self.0.ready(filters).await
    }
    async fn list_all(&self) -> Result<Vec<Bead>> {
        self.0.list_all().await
    }
    async fn show(&self, id: &BeadId) -> Result<Bead> {
        self.0.show(id).await
    }
    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        self.0.claim(id, actor).await
    }
    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
        self.0.claim_auto(actor).await
    }
    async fn release(&self, id: &BeadId) -> Result<()> {
        self.0.release(id).await
    }
    async fn block(&self, id: &BeadId) -> Result<()> {
        self.0.block(id).await
    }
    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        self.0.clear_assignee(id).await
    }
    async fn flush(&self) -> Result<()> {
        self.0.flush().await
    }
    async fn reopen(&self, id: &BeadId) -> Result<()> {
        self.0.reopen(id).await
    }
    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        self.0.labels(id).await
    }
    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.0.add_label(id, label).await
    }
    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.0.remove_label(id, label).await
    }
    async fn append_notes(&self, id: &BeadId, note: &str) -> Result<()> {
        self.0.append_notes(id, note).await
    }
    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        self.0.create_bead(title, body, labels).await
    }
    async fn add_dependency(&self, blocker: &BeadId, blocked: &BeadId) -> Result<()> {
        self.0.add_dependency(blocker, blocked).await
    }
    async fn remove_dependency(&self, blocked: &BeadId, blocker: &BeadId) -> Result<()> {
        self.0.remove_dependency(blocked, blocker).await
    }
    async fn doctor_repair(&self) -> Result<RepairReport> {
        self.0.doctor_repair().await
    }
    async fn doctor_check(&self) -> Result<RepairReport> {
        self.0.doctor_check().await
    }
    async fn full_rebuild(&self) -> Result<()> {
        self.0.full_rebuild().await
    }
    fn has_valid_store(&self) -> bool {
        self.0.has_valid_store()
    }
}

fn live() -> AdmissionPolicy {
    AdmissionPolicy {
        shadow: false,
        ..AdmissionPolicy::default()
    }
}

// ──────────────────────────────────────────────────────────────────────────
// The three cases the acceptance criteria name
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_unchanged_retry_class_resolves_to_its_existing_owners_and_creates_nothing() {
    let harness = Harness::new(Vec::new());
    let summary = harness
        .submit(&unchanged_retry_rows(), live())
        .await
        .expect("submit");

    assert!(
        summary.created.is_empty(),
        "R1/R2 already own unchanged retries; filing again is the ADR-015 failure"
    );
    assert!(harness.store.created().is_empty());

    let decisions = read_decisions(&harness.journal()).expect("journal");
    assert_eq!(decisions.len(), 1, "the decision is still recorded");
    let encoded = serde_json::to_string(&decisions[0].decision).expect("encode");
    assert!(
        encoded.contains("already_owned") && encoded.contains("needle-d6c5397a"),
        "the refusal names the owner: {encoded}"
    );

    // The owner table is the plan's, not invented here.
    let owners = plan_owners();
    assert_eq!(
        owners.get(&EvidenceClass::RepeatedIdenticalFailures),
        Some(&vec!["needle-d6c5397a", "needle-7b9718bc"])
    );
}

#[tokio::test]
async fn a_new_evidence_class_creates_exactly_one_bead_across_two_submissions() {
    let harness = Harness::new(Vec::new());

    let first = harness
        .submit(&red_baseline_rows(), live())
        .await
        .expect("submit");
    assert_eq!(
        first.created.len(),
        1,
        "the first submission files one bead"
    );

    // The second submission sees the first's bead through its signature label.
    let signature = harness.store.created()[0]
        .2
        .iter()
        .find_map(|label| label.strip_prefix(SIGNATURE_LABEL_PREFIX))
        .expect("the bead carries its signature")
        .to_string();
    harness
        .store
        .beads
        .lock()
        .expect("lock")
        .push(signed_bead("improv-1", &signature));

    let second = harness
        .submit(&red_baseline_rows(), live())
        .await
        .expect("submit");
    assert!(
        second.created.is_empty(),
        "one evidence signature creates one bead, however many times it is submitted"
    );
    assert_eq!(
        harness.store.created().len(),
        1,
        "exactly one bead exists for the class"
    );
}

#[tokio::test]
async fn budget_exhaustion_is_refused_with_a_recorded_decision() {
    let harness = Harness::new(Vec::new());

    // Two red-baseline workspaces, a budget of one.
    let mut rows = red_baseline_rows();
    rows.extend((0..12).map(|n| {
        row(
            "LOOM",
            "claude-code-glm-5.3-flash",
            "indeterminate",
            &format!("loom-{n}"),
            &format!("loom-a{n}"),
            "signal:9",
            13,
            false,
        )
    }));

    let summary = harness.submit(&rows, live()).await.expect("submit");
    assert_eq!(summary.created.len(), 1, "one per day");
    assert_eq!(
        summary.refusals.get("budget_exhausted").copied(),
        Some(1),
        "the second is refused for budget, and said so: {:?}",
        summary.refusals
    );

    let decisions = read_decisions(&harness.journal()).expect("journal");
    assert_eq!(
        decisions.len(),
        2,
        "both are recorded, not just the admitted one"
    );
}

#[tokio::test]
async fn shadow_mode_records_decisions_and_files_nothing() {
    let harness = Harness::new(Vec::new());
    let summary = harness
        .submit(&red_baseline_rows(), AdmissionPolicy::default())
        .await
        .expect("submit");

    assert!(summary.created.is_empty(), "shadow files nothing");
    assert_eq!(
        summary.refusals.get("shadow_mode").copied(),
        Some(1),
        "and records that shadow was the reason"
    );
    assert_eq!(
        read_decisions(&harness.journal()).expect("journal").len(),
        1
    );
}

// ──────────────────────────────────────────────────────────────────────────
// The bead an admitted proposal produces
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_filed_bead_carries_provenance_the_acceptance_measure_and_its_signature() {
    let harness = Harness::new(Vec::new());
    harness
        .submit(&red_baseline_rows(), live())
        .await
        .expect("submit");

    let created = harness.store.created();
    let (title, body, labels) = &created[0];

    assert!(
        title.contains("red_baseline_workspace") && title.contains("commitgraph"),
        "the title names the class and the workspace: {title}"
    );
    assert!(
        body.contains("Filed by the NEEDLE improvement loop"),
        "a reader can tell no person wrote this"
    );
    assert!(
        body.contains("needle-proposal:"),
        "provenance ref is in the body: {body}"
    );
    assert!(
        body.contains("## Acceptance measure") && body.contains("verified_yield_per_attempt"),
        "the acceptance measure is stated in the bead a worker will read"
    );
    assert!(
        body.contains("withdrawn whatever the implementing agent reports"),
        "and so is the consequence of missing it"
    );
    assert!(body.contains("## Evidence"), "evidence is cited");
    assert!(body.contains("## Rollback"), "and the rollback");

    assert!(labels.contains(&IMPROVEMENT_LABEL.to_string()));
    assert!(
        labels
            .iter()
            .any(|label| label.starts_with(SIGNATURE_LABEL_PREFIX)),
        "the signature label is what makes the next run deduplicate: {labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label == "human"),
        "an automatically filed bead never claims a human gate"
    );
}

#[tokio::test]
async fn the_budget_survives_a_restart_because_it_is_read_from_the_journal() {
    let harness = Harness::new(Vec::new());

    let first = harness
        .submit(&red_baseline_rows(), live())
        .await
        .expect("submit");
    assert_eq!(first.created.len(), 1);

    // A different evidence class, so deduplication is not what stops it: a
    // fresh process reading the same journal must still see the budget spent.
    let mut rows = Vec::new();
    rows.extend((0..12).map(|n| {
        row(
            "LOOM",
            "claude-code-glm-5.3-flash",
            "indeterminate",
            &format!("loom-{n}"),
            &format!("loom-a{n}"),
            "signal:9",
            13,
            false,
        )
    }));

    let second = harness.submit(&rows, live()).await.expect("submit");
    assert!(
        second.created.is_empty(),
        "a restarted controller must not admit another one: {:?}",
        second.refusals
    );
    assert_eq!(second.refusals.get("budget_exhausted").copied(), Some(1));
}
