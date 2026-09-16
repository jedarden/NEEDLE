//! Bead filing for `needle audit` violations (N-T58).
//!
//! A detector that only prints is read by nobody: the 2026-09-14 CI outage and
//! the stalled NEEDLE workspace each needed a tracked bead, and neither had
//! one for the whole of its life. Filing closes that gap — but history sets
//! the terms. 931 of 946 self-filed ALERT beads were noise, so the response to
//! a self-filing detector is to stop reading it unless filing is *rare*,
//! *deduplicated* and *budgeted*. Three rules follow:
//!
//! - **One bead per evidence signature.** The signature is a pure function of
//!   (rule, scope, subject), carried on the bead as
//!   `audit-signature:<16 hex>`. A later run that sees the same evidence
//!   appends a bounded note to the existing bead instead of filing a twin.
//! - **A per-run budget.** At most `audit.max_beads_per_run` new beads. A
//!   violation past the budget is still reported and still telemetered; only
//!   the bead is withheld, so the cap can never hide a finding.
//! - **Violations only, and never `human`.** Informational findings describe
//!   states that are correct as-is, so filing one would ask a person to fix
//!   something that is not broken. The `human` label is what Unravel
//!   consumes; an audit bead must never wear it.
//!
//! Nothing here repairs anything. Filing a bead is a report with a tracking
//! number, which is a different blast radius from changing state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use super::{AuditContext, AuditReport, Finding, Severity, WorkspaceEntry};
use crate::bead_store::BeadStore;
use crate::types::BeadId;

/// Label prefix carrying a finding's evidence signature. The deduplication key.
pub const SIGNATURE_LABEL_PREFIX: &str = "audit-signature:";

/// Label every filed bead carries, so the whole self-filed population is one
/// `bead list` away.
pub const AUDIT_LABEL: &str = "audit";

/// The label Unravel consumes. An audit bead must never carry it.
pub const HUMAN_LABEL: &str = "human";

/// Label marking a bead no fleet worker may claim (N-T60).
///
/// `strands.pluck.exclude_labels` carries it by default.
pub const ESCALATION_LABEL: &str = "escalation";

/// Whether a finding escalates instead of filing ordinary work.
///
/// One rule does. `F5_LEARNING_LOOP_STALLED` fires exactly when the fleet has
/// failed to move the learning loop, so a bead a worker could claim would hand
/// the problem straight back to its cause.
pub fn is_escalating(finding: &Finding) -> bool {
    finding.rule == super::factory::F5_LEARNING_LOOP_STALLED
}

/// Longest repeat note appended to an existing bead.
///
/// Bounded because the note is appended once per run per still-open finding:
/// an unbounded note on a violation that lasts a fortnight is how a bead body
/// becomes unreadable, and an unreadable bead is an unworked bead.
const NOTE_MAX_BYTES: usize = 512;

/// Hex characters of the digest kept in a signature.
const SIGNATURE_HEX_LEN: usize = 16;

/// The evidence signature of a finding: the first 16 hex characters of the
/// SHA-256 over rule, scope and subject, NUL-separated.
///
/// NUL separation rather than concatenation so that a rule ending in a scope's
/// first characters cannot collide with its neighbour. `detail` and `count` are
/// deliberately excluded: the same broken thing whose numbers moved by one
/// attempt is the same finding, and including them would file a fresh bead on
/// every run.
pub fn signature(finding: &Finding) -> String {
    let mut hasher = Sha256::new();
    hasher.update(finding.rule.as_bytes());
    hasher.update([0u8]);
    hasher.update(finding.scope.as_bytes());
    hasher.update([0u8]);
    hasher.update(finding.subject.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    digest[..SIGNATURE_HEX_LEN].to_string()
}

/// The signature label a filed bead carries.
pub fn signature_label(finding: &Finding) -> String {
    format!("{SIGNATURE_LABEL_PREFIX}{}", signature(finding))
}

/// Labels a filed bead carries: the population label, the rule, and the
/// signature. Never [`HUMAN_LABEL`].
pub fn labels_for(finding: &Finding) -> Vec<String> {
    let mut labels = vec![
        AUDIT_LABEL.to_string(),
        format!("{AUDIT_LABEL}:{}", finding.rule),
        signature_label(finding),
    ];
    if is_escalating(finding) {
        labels.push(ESCALATION_LABEL.to_string());
    }
    labels
}

/// The title a filed bead carries.
pub fn title_for(finding: &Finding) -> String {
    format!("[{}] {}: {}", finding.rule, finding.scope, finding.subject)
}

/// The body a filed bead carries.
///
/// `escalation` names the brief written for an escalating finding, so the bead
/// points at the file a person is meant to open. An escalation whose brief
/// nobody can find is the same as no brief at all.
pub fn body_for(finding: &Finding, escalation: Option<&Path>) -> String {
    let mut body = format!(
        "Filed by `needle audit` (N-T58). This bead reports a finding; it does not repair it.\n\
         \n\
         - rule: {rule}\n\
         - severity: {severity}\n\
         - scope: {scope}\n\
         - subject: {subject}\n\
         - beads affected: {count}\n\
         \n\
         {detail}\n\
         \n\
         This bead carries `{label}`. A later audit run that sees the same\n\
         evidence signature appends a note here instead of filing a duplicate,\n\
         so one bead tracks this finding for as long as it lasts.\n",
        rule = finding.rule,
        severity = finding.severity.as_str(),
        scope = finding.scope,
        subject = finding.subject,
        count = finding.count,
        detail = finding.detail,
        label = signature_label(finding),
    );
    if let Some(path) = escalation {
        body.push_str(&format!(
            "\nThis finding is escalated. The bead carries `{ESCALATION_LABEL}`, which\n\
             Pluck excludes by default, so no fleet worker will claim it — the fleet is\n\
             what failed to move this work. The brief for an interactive session or a\n\
             stronger-model lane is at:\n\
             \n    {}\n",
            path.display()
        ));
    }
    body
}

/// The note a repeat sighting appends to an existing bead.
pub fn repeat_note(finding: &Finding, at: DateTime<Utc>) -> String {
    let mut note = format!(
        "{}: `needle audit` saw this finding again ({} bead(s) affected). {}",
        at.to_rfc3339(),
        finding.count,
        finding.detail
    );
    truncate_on_char_boundary(&mut note, NOTE_MAX_BYTES);
    note
}

/// Truncate to at most `max_bytes`, never splitting a UTF-8 character.
fn truncate_on_char_boundary(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    // Reserve the ellipsis so the result still fits the budget.
    let ellipsis = '…';
    let mut end = max_bytes.saturating_sub(ellipsis.len_utf8());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push(ellipsis);
}

/// What a run decided to do about one violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filing {
    /// No non-closed bead carries this signature: file one.
    Create,
    /// A non-closed bead already carries it: append a bounded note.
    Note(BeadId),
    /// Past the per-run budget. Reported and telemetered, not filed.
    OverBudget,
}

/// One planned filing, paired with the finding that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFiling {
    pub finding: Finding,
    pub signature: String,
    pub action: Filing,
    /// The escalation brief written for this finding, when it escalates.
    ///
    /// Set by [`file_report`] once the brief exists, so the filed bead can
    /// name a path that is already on disk rather than one it hopes for.
    pub escalation: Option<PathBuf>,
}

/// Decide what to do with every violation in `report`.
///
/// Pure, and the whole of the filing policy: `existing` answers "does a
/// non-closed bead already carry this signature?", `max_new` is the per-run
/// budget, and informational findings are dropped before either applies.
/// Keeping the decision separate from the store means the policy is provable
/// against a fixture without a bead backend anywhere near it.
pub fn plan(
    findings: &[Finding],
    existing: &dyn Fn(&Finding) -> Option<BeadId>,
    max_new: usize,
) -> Vec<PlannedFiling> {
    let mut planned = Vec::new();
    let mut created = 0usize;
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();

    for finding in findings
        .iter()
        .filter(|finding| finding.severity == Severity::Violation)
    {
        let signature = signature(finding);
        // Two findings with one signature are byte-identical findings; the
        // second is a duplicate of the first, not a second thing to file.
        if seen.insert(signature.clone(), ()).is_some() {
            continue;
        }

        let action = match existing(finding) {
            Some(id) => Filing::Note(id),
            None if created < max_new => {
                created += 1;
                Filing::Create
            }
            None => Filing::OverBudget,
        };
        planned.push(PlannedFiling {
            finding: finding.clone(),
            signature,
            action,
            escalation: None,
        });
    }
    planned
}

/// Non-closed beads in `store` that already carry an audit signature, as
/// signature -> bead id.
///
/// Closed beads are skipped on purpose: a finding that recurs after its bead
/// was closed is a regression, and a regression deserves its own bead rather
/// than a note appended to a closed one nobody is reading.
pub async fn existing_signatures(store: &dyn BeadStore) -> Result<BTreeMap<String, BeadId>> {
    let mut beads = store
        .list_all()
        .await
        .context("failed to read the bead inventory for audit filing")?;
    // Sorted so that two beads sharing a signature resolve to the same one on
    // every run, whatever order the backend listed them in.
    beads.sort_by_key(|bead| bead.id.to_string());

    let mut found = BTreeMap::new();
    for bead in beads {
        if bead.status.is_done() {
            continue;
        }
        for label in &bead.labels {
            if let Some(signature) = label.strip_prefix(SIGNATURE_LABEL_PREFIX) {
                found
                    .entry(signature.to_string())
                    .or_insert_with(|| bead.id.clone());
            }
        }
    }
    Ok(found)
}

/// The workspace whose bead store owns a finding.
///
/// A finding scoped to a discovered workspace belongs in that workspace's own
/// store, where the people working it will see it. Anything else — a
/// fleet-scoped finding, a scope that matches no workspace — goes to
/// `audit.home_workspace` rather than to whichever workspace happened to sort
/// first.
pub fn owning_workspace(finding: &Finding, workspaces: &[WorkspaceEntry], home: &Path) -> PathBuf {
    workspaces
        .iter()
        .find(|workspace| workspace.name == finding.scope)
        .map(|workspace| workspace.path.clone())
        .unwrap_or_else(|| home.to_path_buf())
}

/// What one filing run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FilingSummary {
    /// Beads created this run.
    pub created: Vec<BeadId>,
    /// Beads that gained a repeat note instead of a duplicate.
    pub noted: Vec<BeadId>,
    /// Violations reported and telemetered but not filed, being past budget.
    pub over_budget: usize,
}

/// Apply one planned filing to an already-opened store.
pub async fn apply(
    store: &dyn BeadStore,
    planned: &PlannedFiling,
    at: DateTime<Utc>,
    summary: &mut FilingSummary,
) -> Result<()> {
    match &planned.action {
        Filing::Create => {
            let labels = labels_for(&planned.finding);
            debug_assert!(
                !labels.iter().any(|label| label == HUMAN_LABEL),
                "an audit bead must never carry the human label"
            );
            let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let id = store
                .create_bead(
                    &title_for(&planned.finding),
                    &body_for(&planned.finding, planned.escalation.as_deref()),
                    &label_refs,
                )
                .await
                .with_context(|| format!("failed to file a bead for {}", planned.finding.rule))?;
            summary.created.push(id);
        }
        Filing::Note(id) => {
            store
                .append_notes(id, &repeat_note(&planned.finding, at))
                .await
                .with_context(|| format!("failed to note the repeat on bead {id}"))?;
            summary.noted.push(id.clone());
        }
        Filing::OverBudget => summary.over_budget += 1,
    }
    Ok(())
}

/// File every violation in `report` into the store that owns it.
///
/// Production path: opens one store per owning workspace, reads its existing
/// signatures once, plans the whole run against a single shared budget, and
/// applies. The budget is per run rather than per store, so a noisy workspace
/// cannot spend another one's allowance.
pub async fn file_report(ctx: &AuditContext, report: &AuditReport) -> Result<FilingSummary> {
    let home = &ctx.config.audit.home_workspace;
    let violations: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Violation)
        .collect();

    // Open each owning store once and read its signatures once.
    let mut stores = BTreeMap::new();
    for finding in &violations {
        let path = owning_workspace(finding, &ctx.workspaces, home);
        if stores.contains_key(&path) {
            continue;
        }
        let store = super::workspace_store(&path)
            .with_context(|| format!("failed to open the bead store at {}", path.display()))?;
        let signatures = existing_signatures(store.as_ref()).await?;
        stores.insert(path, (store, signatures));
    }

    let lookup = |finding: &Finding| -> Option<BeadId> {
        let path = owning_workspace(finding, &ctx.workspaces, home);
        stores
            .get(&path)
            .and_then(|(_, signatures)| signatures.get(&signature(finding)).cloned())
    };

    let findings: Vec<Finding> = violations
        .iter()
        .map(|finding| (*finding).clone())
        .collect();
    let mut planned = plan(&findings, &lookup, ctx.config.audit.max_beads_per_run);

    // An escalating finding gets its brief before its bead, so the bead names
    // a path that is already on disk rather than one it hopes for (N-T60).
    let brief_dir = super::escalation::default_brief_dir(&ctx.config);
    for item in planned
        .iter_mut()
        .filter(|item| is_escalating(&item.finding))
    {
        let workspace = owning_workspace(&item.finding, &ctx.workspaces, home);
        let stalled = super::factory::stalled_loop_beads(ctx, &workspace);
        item.escalation = Some(super::escalation::write_brief(
            &brief_dir,
            &item.finding,
            &stalled,
            ctx.collected_at,
        )?);
    }

    let mut summary = FilingSummary::default();
    for item in &planned {
        // Resolved from the finding rather than from a parallel index: `plan`
        // drops intra-run duplicate signatures, so a positional index into
        // `violations` can run short and file a finding into another
        // workspace's store.
        let path = owning_workspace(&item.finding, &ctx.workspaces, home);
        let Some((store, _)) = stores.get(&path) else {
            continue;
        };
        apply(store.as_ref(), item, ctx.collected_at, &mut summary).await?;
    }
    Ok(summary)
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::Filters;
    use crate::types::{Bead, BeadStatus, ClaimResult};
    use std::sync::Mutex;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-16T12:00:00Z")
            .expect("fixture stamp parses")
            .with_timezone(&Utc)
    }

    fn violation(rule: &str, scope: &str, subject: &str) -> Finding {
        Finding::violation(rule, scope, subject, "detail of the finding", 3)
    }

    fn bead(id: &str, status: BeadStatus, labels: &[&str]) -> Bead {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "title": id,
            "description": null,
            "priority": 1,
            "status": status,
            "assignee": "",
            "labels": labels,
            "source_repo": "/fixture-root/NEEDLE",
            "created_at": now().to_rfc3339(),
            "updated_at": now().to_rfc3339(),
        }))
        .expect("fixture bead deserializes")
    }

    /// An in-memory bead store. No process is spawned, which the `--lib`
    /// harness enforces (tests/lib-process-purity).
    #[derive(Default)]
    struct MemoryStore {
        beads: Mutex<Vec<Bead>>,
        created: Mutex<Vec<(String, String, Vec<String>)>>,
        notes: Mutex<Vec<(BeadId, String)>>,
        next: Mutex<usize>,
    }

    impl MemoryStore {
        fn with(beads: Vec<Bead>) -> Self {
            MemoryStore {
                beads: Mutex::new(beads),
                ..MemoryStore::default()
            }
        }

        fn created(&self) -> Vec<(String, String, Vec<String>)> {
            self.created.lock().expect("created lock").clone()
        }

        fn notes(&self) -> Vec<(BeadId, String)> {
            self.notes.lock().expect("notes lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for MemoryStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(Vec::new())
        }

        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().expect("beads lock").clone())
        }

        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.beads
                .lock()
                .expect("beads lock")
                .iter()
                .find(|bead| bead.id == *id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))
        }

        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("claim is not exercised by the filing fixtures")
        }

        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("claim_auto is not exercised by the filing fixtures")
        }

        async fn release(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("release is not exercised by the filing fixtures")
        }

        async fn block(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("block is not exercised by the filing fixtures")
        }

        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("clear_assignee is not exercised by the filing fixtures")
        }

        async fn flush(&self) -> Result<()> {
            Ok(())
        }

        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            anyhow::bail!("reopen is not exercised by the filing fixtures")
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

        async fn append_notes(&self, id: &BeadId, note: &str) -> Result<()> {
            self.notes
                .lock()
                .expect("notes lock")
                .push((id.clone(), note.to_string()));
            Ok(())
        }

        async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
            let mut next = self.next.lock().expect("next lock");
            *next += 1;
            let id = BeadId::from(format!("audit-{next}"));
            self.created.lock().expect("created lock").push((
                title.to_string(),
                body.to_string(),
                labels.iter().map(|label| label.to_string()).collect(),
            ));
            Ok(id)
        }

        async fn add_dependency(&self, _blocker: &BeadId, _blocked: &BeadId) -> Result<()> {
            anyhow::bail!("add_dependency is not exercised by the filing fixtures")
        }

        async fn remove_dependency(&self, _blocked: &BeadId, _blocker: &BeadId) -> Result<()> {
            anyhow::bail!("remove_dependency is not exercised by the filing fixtures")
        }

        async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
            anyhow::bail!("doctor_repair is not exercised by the filing fixtures")
        }

        async fn doctor_check(&self) -> Result<crate::bead_store::RepairReport> {
            anyhow::bail!("doctor_check is not exercised by the filing fixtures")
        }

        async fn full_rebuild(&self) -> Result<()> {
            anyhow::bail!("full_rebuild is not exercised by the filing fixtures")
        }

        fn has_valid_store(&self) -> bool {
            true
        }
    }

    #[test]
    fn nt58_signature_keys_on_rule_scope_and_subject_only() {
        let finding = violation("F4_CI_RED", "NEEDLE", "needle-ci");
        let signature = signature(&finding);
        assert_eq!(signature.len(), 16, "signature is 16 hex characters");
        assert!(signature.chars().all(|c| c.is_ascii_hexdigit()));

        // The same broken thing whose numbers moved is the same finding.
        let moved = Finding::violation(
            "F4_CI_RED",
            "NEEDLE",
            "needle-ci",
            "a different detail entirely",
            99,
        );
        assert_eq!(signature, super::signature(&moved));

        // A different rule, scope or subject is a different thing.
        assert_ne!(
            signature,
            super::signature(&violation("F1_X", "NEEDLE", "needle-ci"))
        );
        assert_ne!(
            signature,
            super::signature(&violation("F4_CI_RED", "SEAM", "needle-ci"))
        );
        assert_ne!(
            signature,
            super::signature(&violation("F4_CI_RED", "NEEDLE", "seam-ci"))
        );
    }

    #[test]
    fn nt58_informational_findings_are_never_filed() {
        let findings = vec![
            Finding::informational("I_CHECKLIST_DRIFT", "NEEDLE", "needle-parent", "drift", 1),
            violation("F4_CI_RED", "NEEDLE", "needle-ci"),
        ];
        let planned = plan(&findings, &|_| None, 10);

        assert_eq!(planned.len(), 1, "only the violation is planned");
        assert_eq!(planned[0].finding.rule, "F4_CI_RED");
    }

    #[test]
    fn nt58_a_violation_past_the_budget_is_reported_but_not_filed() {
        let findings: Vec<Finding> = (0..5)
            .map(|index| violation("F4_CI_RED", &format!("ws-{index}"), "ci"))
            .collect();
        let planned = plan(&findings, &|_| None, 3);

        let created = planned
            .iter()
            .filter(|item| item.action == Filing::Create)
            .count();
        let withheld = planned
            .iter()
            .filter(|item| item.action == Filing::OverBudget)
            .count();
        assert_eq!(created, 3, "the budget caps new beads");
        assert_eq!(withheld, 2, "the rest are still planned, just not filed");
        assert_eq!(
            planned.len(),
            findings.len(),
            "every violation is accounted for"
        );
    }

    #[test]
    fn nt58_an_existing_signature_is_noted_regardless_of_budget() {
        let existing = BeadId::from("needle-existing".to_string());
        let findings = vec![violation("F4_CI_RED", "NEEDLE", "needle-ci")];
        // Budget zero: a note is not a new bead, so it is not budgeted.
        let planned = plan(&findings, &|_| Some(existing.clone()), 0);

        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].action, Filing::Note(existing));
    }

    #[test]
    fn nt58_a_duplicate_signature_within_one_run_is_planned_once() {
        let findings = vec![
            violation("F4_CI_RED", "NEEDLE", "needle-ci"),
            violation("F4_CI_RED", "NEEDLE", "needle-ci"),
        ];
        let planned = plan(&findings, &|_| None, 10);
        assert_eq!(planned.len(), 1);
    }

    #[tokio::test]
    async fn nt58_two_runs_over_one_violation_create_a_bead_then_note_it() {
        let store = MemoryStore::default();
        let findings = vec![violation("F4_CI_RED", "NEEDLE", "needle-ci")];
        let signature_of = signature(&findings[0]);

        // First run: nothing carries the signature, so a bead is filed.
        let existing = existing_signatures(&store).await.expect("inventory reads");
        assert!(existing.is_empty());
        let planned = plan(
            &findings,
            &|finding| existing.get(&signature(finding)).cloned(),
            3,
        );
        let mut summary = FilingSummary::default();
        for item in &planned {
            apply(&store, item, now(), &mut summary)
                .await
                .expect("filing applies");
        }
        assert_eq!(summary.created.len(), 1, "one bead filed");
        assert!(summary.noted.is_empty());
        assert_eq!(store.created().len(), 1);

        // The filed bead now exists carrying its signature label.
        let filed = store.created()[0].clone();
        store.beads.lock().expect("beads lock").push(bead(
            "audit-1",
            BeadStatus::Open,
            &[
                AUDIT_LABEL,
                "audit:F4_CI_RED",
                &format!("{SIGNATURE_LABEL_PREFIX}{signature_of}"),
            ],
        ));

        // Second run over the same violation: a note, never a duplicate.
        let existing = existing_signatures(&store).await.expect("inventory reads");
        assert_eq!(existing.len(), 1);
        let planned = plan(
            &findings,
            &|finding| existing.get(&signature(finding)).cloned(),
            3,
        );
        let mut summary = FilingSummary::default();
        for item in &planned {
            apply(&store, item, now(), &mut summary)
                .await
                .expect("filing applies");
        }
        assert!(summary.created.is_empty(), "no second bead");
        assert_eq!(summary.noted.len(), 1, "the existing bead gained a note");
        assert_eq!(store.created().len(), 1, "still exactly one bead filed");
        assert_eq!(store.notes().len(), 1);
        assert!(store.notes()[0].1.contains("saw this finding again"));
        assert!(filed
            .2
            .iter()
            .any(|label| label.starts_with(SIGNATURE_LABEL_PREFIX)));
    }

    #[tokio::test]
    async fn nt58_a_filed_bead_carries_audit_labels_and_never_human() {
        let store = MemoryStore::default();
        let finding = violation("F1_WORKSPACE_NO_VERIFIED_CLOSURES", "NEEDLE", "NEEDLE");
        let planned = plan(std::slice::from_ref(&finding), &|_| None, 3);
        let mut summary = FilingSummary::default();
        apply(&store, &planned[0], now(), &mut summary)
            .await
            .expect("filing applies");

        let (title, body, labels) = store.created()[0].clone();
        assert!(title.contains("F1_WORKSPACE_NO_VERIFIED_CLOSURES"));
        assert!(title.contains("NEEDLE"));
        assert!(labels.contains(&AUDIT_LABEL.to_string()));
        assert!(labels.contains(&"audit:F1_WORKSPACE_NO_VERIFIED_CLOSURES".to_string()));
        assert!(labels
            .iter()
            .any(|label| label.starts_with(SIGNATURE_LABEL_PREFIX)));
        assert!(
            !labels.iter().any(|label| label == HUMAN_LABEL),
            "Unravel consumes `human`; an audit bead must never carry it: {labels:?}"
        );
        assert!(body.contains("does not repair"));
        assert!(body.contains(&signature(&finding)));
    }

    #[tokio::test]
    async fn nt58_a_closed_bead_does_not_suppress_a_new_one() {
        let finding = violation("F4_CI_RED", "NEEDLE", "needle-ci");
        let label = signature_label(&finding);
        // The same signature, but the bead tracking it has been closed: the
        // finding has recurred, which is its own bead rather than a note on a
        // closed one nobody reads.
        let store = MemoryStore::with(vec![bead("audit-old", BeadStatus::Closed, &[&label])]);

        let existing = existing_signatures(&store).await.expect("inventory reads");
        assert!(
            existing.is_empty(),
            "closed beads are not deduplication targets"
        );

        let planned = plan(
            std::slice::from_ref(&finding),
            &|f| existing.get(&signature(f)).cloned(),
            3,
        );
        assert_eq!(planned[0].action, Filing::Create);
    }

    #[test]
    fn nt58_the_owning_store_falls_back_to_the_home_workspace() {
        let workspaces = vec![WorkspaceEntry {
            path: PathBuf::from("/fixture-root/NEEDLE"),
            name: "NEEDLE".to_string(),
        }];
        let home = Path::new("/fixture-root/HOME");

        // A finding scoped to a discovered workspace lands in that workspace.
        let scoped = violation("F1_WORKSPACE_NO_VERIFIED_CLOSURES", "NEEDLE", "NEEDLE");
        assert_eq!(
            owning_workspace(&scoped, &workspaces, home),
            PathBuf::from("/fixture-root/NEEDLE")
        );

        // A fleet-scoped finding matches no workspace and lands at home.
        let fleet = violation("F3_UNCOSTED_TIMEOUTS", "fleet", "glm-flash");
        assert_eq!(
            owning_workspace(&fleet, &workspaces, home),
            home.to_path_buf()
        );
    }

    /// The escalation bead is the one bead the fleet must not claim, and it
    /// has to name the brief a person is meant to open (N-T60).
    #[tokio::test]
    async fn nt60_an_escalation_bead_carries_the_label_and_names_its_brief() {
        let store = MemoryStore::default();
        let finding = Finding::violation(
            crate::cli::audit::factory::F5_LEARNING_LOOP_STALLED,
            "NEEDLE",
            "learning-loop",
            "7 open unassigned learning-loop bead(s) waiting",
            7,
        );
        assert!(is_escalating(&finding));

        let mut planned = plan(std::slice::from_ref(&finding), &|_| None, 3);
        let brief =
            PathBuf::from("/fixture/.needle/state/escalations/F5_LEARNING_LOOP_STALLED--NEEDLE.md");
        planned[0].escalation = Some(brief.clone());

        let mut summary = FilingSummary::default();
        apply(&store, &planned[0], now(), &mut summary)
            .await
            .expect("filing applies");

        let (_, body, labels) = store.created()[0].clone();
        assert!(
            labels.contains(&ESCALATION_LABEL.to_string()),
            "an escalation bead carries the label Pluck excludes: {labels:?}"
        );
        assert!(
            !labels.iter().any(|label| label == HUMAN_LABEL),
            "still never `human`: {labels:?}"
        );
        assert!(
            body.contains("F5_LEARNING_LOOP_STALLED--NEEDLE.md"),
            "the bead names its brief: {body}"
        );

        // An ordinary violation is not escalated and names no brief.
        let ordinary = violation("F4_CI_RED", "NEEDLE", "needle-ci");
        assert!(!is_escalating(&ordinary));
        assert!(!labels_for(&ordinary)
            .iter()
            .any(|label| label == ESCALATION_LABEL));
        assert!(!body_for(&ordinary, None).contains("escalated"));
    }

    #[test]
    fn nt58_the_repeat_note_is_bounded() {
        let finding = Finding::violation("F4_CI_RED", "NEEDLE", "needle-ci", "é".repeat(4000), 1);
        let note = repeat_note(&finding, now());

        assert!(
            note.len() <= NOTE_MAX_BYTES,
            "note is {} bytes, over the {NOTE_MAX_BYTES} budget",
            note.len()
        );
        // Truncation never splits a character.
        assert!(std::str::from_utf8(note.as_bytes()).is_ok());
        assert!(note.ends_with('…'));
    }
}
