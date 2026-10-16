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
//!
//! # Filing never costs the verdict
//!
//! A failure to file or note one finding is recorded as a [`FilingError`] and
//! the run carries on: the verdict, every other finding and every other bead
//! still land. From 2026-10-12 to 10-16 one unwritable repeat note — the
//! finding bead was leased by a worker that was no longer running — turned
//! every daily factory audit into `audit: no verdict`, so the fleet's health
//! check reported nothing for five days while gates were red. One bead's
//! bookkeeping must never be able to do that again.
//!
//! The audit writes only under its own identity. When bead-rs refuses a note
//! because another claimant holds the bead's lease, the refusal is the policy
//! working: the error text even names the epoch to pass, and presenting it
//! would be writing with somebody else's fencing token. The finding is
//! recorded as `filing_blocked` with the holder named instead.

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

/// Label marking an audit bead as an operational defect: the fleet's own
/// machinery is failing every change put in front of it, so the bead is
/// filed at P0 for whoever can fix it now — never as a held proposal.
pub const OPERATIONAL_DEFECT_LABEL: &str = "operational-defect";

/// Whether a finding is an operational defect (filed P0, never a proposal).
///
/// `F6_GATE_RED_AT_HEAD` is: NEEDLE's own gate_1 was red at pristine HEAD
/// for days in October 2026 while its only fix waited as a `human`-held
/// proposal nobody looked at.
pub fn is_operational_defect(finding: &Finding) -> bool {
    finding.rule == super::factory::F6_GATE_RED_AT_HEAD
}

/// The priority a filed bead is created at, when it is not the backend
/// default. Operational defects are P0; everything else keeps P2.
pub fn priority_for(finding: &Finding) -> Option<u8> {
    is_operational_defect(finding).then_some(0)
}

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
    if is_operational_defect(finding) {
        labels.push(OPERATIONAL_DEFECT_LABEL.to_string());
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
    if is_operational_defect(finding) {
        body.push_str(&format!(
            "\nThis is an operational defect, filed at P0 and carrying `{OPERATIONAL_DEFECT_LABEL}`.\n\
             It is not a proposal and needs no admission: the fleet's own machinery is\n\
             failing every change put in front of it, and every attempt dispatched\n\
             until it is fixed is spent for nothing.\n"
        ));
    }
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
    /// Findings whose bead could not be filed or noted this run. Each is
    /// reported beside the verdict; none of them suppresses it.
    pub errors: Vec<FilingError>,
}

impl FilingSummary {
    /// Filing errors that were lease refusals rather than failures.
    pub fn blocked(&self) -> usize {
        self.errors
            .iter()
            .filter(|error| error.kind == FilingErrorKind::FilingBlocked)
            .count()
    }
}

/// Why a finding's bead was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilingErrorKind {
    /// The target bead is leased by another claimant and bead-rs refused the
    /// write without that claimant's credential. Expected, not a fault: the
    /// audit never presents a credential it does not own.
    FilingBlocked,
    /// Anything else: a store that would not open, an unreadable inventory, a
    /// create or note the backend refused, a brief that could not be written.
    FilingFailed,
}

impl FilingErrorKind {
    /// The wire name the JSON report and stderr summary share.
    pub fn as_str(&self) -> &'static str {
        match self {
            FilingErrorKind::FilingBlocked => "filing_blocked",
            FilingErrorKind::FilingFailed => "filing_failed",
        }
    }
}

/// One finding whose bead could not be filed or noted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FilingError {
    pub kind: FilingErrorKind,
    pub rule: String,
    pub scope: String,
    pub subject: String,
    /// The finding's evidence signature, so the error joins to its bead.
    pub signature: String,
    /// The existing bead a repeat note was meant for, when there was one.
    pub bead: Option<String>,
    /// Who holds the lease, for a `filing_blocked` error whose holder the
    /// store would name.
    pub holder: Option<String>,
    /// The backend's own words, bounded.
    pub reason: String,
}

impl FilingError {
    fn new(
        kind: FilingErrorKind,
        finding: &Finding,
        bead: Option<&BeadId>,
        holder: Option<String>,
        reason: String,
    ) -> Self {
        let mut reason = reason;
        truncate_on_char_boundary(&mut reason, NOTE_MAX_BYTES);
        FilingError {
            kind,
            rule: finding.rule.clone(),
            scope: finding.scope.clone(),
            subject: finding.subject.clone(),
            signature: signature(finding),
            bead: bead.map(|id| id.to_string()),
            holder,
            reason,
        }
    }

    /// One line for stderr and the human report.
    pub fn describe(&self) -> String {
        let target = match (&self.bead, &self.holder) {
            (Some(bead), Some(holder)) => format!(" on {bead} (lease held by {holder})"),
            (Some(bead), None) => format!(" on {bead}"),
            (None, _) => String::new(),
        };
        format!(
            "{}{target}: [{}] {}: {} — {}",
            self.kind.as_str(),
            self.rule,
            self.scope,
            self.subject,
            self.reason
        )
    }
}

/// Whether a backend error is a lease refusal: the bead is claimed under a
/// fenced lease and the write did not present that claim's credential.
///
/// Matched on bead-rs's own error wording (`Lease conflict:` /
/// `Lease error:` / `Claim-epoch credential ...`), which is what reaches the
/// caller through the CLI store's error context.
pub fn is_lease_refusal(error_text: &str) -> bool {
    error_text.contains("Lease conflict")
        || error_text.contains("Lease error")
        || error_text.contains("Claim-epoch credential")
}

/// Apply one planned filing to an already-opened store.
///
/// Never fails the run. A refused create or note becomes a [`FilingError`] on
/// `summary`, and the caller moves on to the next finding.
pub async fn apply(
    store: &dyn BeadStore,
    planned: &PlannedFiling,
    at: DateTime<Utc>,
    summary: &mut FilingSummary,
) {
    match &planned.action {
        Filing::Create => {
            let labels = labels_for(&planned.finding);
            debug_assert!(
                !labels.iter().any(|label| label == HUMAN_LABEL),
                "an audit bead must never carry the human label"
            );
            let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let title = title_for(&planned.finding);
            let body = body_for(&planned.finding, planned.escalation.as_deref());
            let created = match priority_for(&planned.finding) {
                Some(priority) => {
                    store
                        .create_bead_with_priority(&title, &body, &label_refs, priority)
                        .await
                }
                None => store.create_bead(&title, &body, &label_refs).await,
            };
            match created {
                Ok(id) => summary.created.push(id),
                Err(error) => summary.errors.push(FilingError::new(
                    FilingErrorKind::FilingFailed,
                    &planned.finding,
                    None,
                    None,
                    format!("failed to file a bead: {error:#}"),
                )),
            }
        }
        Filing::Note(id) => match store
            .append_notes(id, &repeat_note(&planned.finding, at))
            .await
        {
            Ok(()) => summary.noted.push(id.clone()),
            Err(error) => {
                let text = format!("{error:#}");
                // A lease refusal is answered by naming the holder, never by
                // retrying with a credential: the one bead-rs suggests is the
                // claimant's, and this process is not the claimant.
                let error = if is_lease_refusal(&text) {
                    let holder = store
                        .show(id)
                        .await
                        .ok()
                        .and_then(|bead| bead.assignee)
                        .filter(|holder| !holder.trim().is_empty());
                    FilingError::new(
                        FilingErrorKind::FilingBlocked,
                        &planned.finding,
                        Some(id),
                        holder,
                        text,
                    )
                } else {
                    FilingError::new(
                        FilingErrorKind::FilingFailed,
                        &planned.finding,
                        Some(id),
                        None,
                        format!("failed to note the repeat: {text}"),
                    )
                };
                summary.errors.push(error);
            }
        },
        Filing::OverBudget => summary.over_budget += 1,
    }
}

/// File every violation in `report` into the store that owns it.
///
/// Production path: opens one store per owning workspace, reads its existing
/// signatures once, plans the whole run against a single shared budget, and
/// applies. The budget is per run rather than per store, so a noisy workspace
/// cannot spend another one's allowance.
///
/// Infallible by design: a store that will not open or an inventory that will
/// not read costs the findings that belong to it — each recorded as a
/// [`FilingError`] — and nothing else. Those findings are kept out of the
/// plan, so a store nobody could read never spends the shared budget.
pub async fn file_report(ctx: &AuditContext, report: &AuditReport) -> FilingSummary {
    let home = &ctx.config.audit.home_workspace;
    let violations: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Violation)
        .collect();

    // Open each owning store once and read its signatures once.
    type OpenStore = (std::sync::Arc<dyn BeadStore>, BTreeMap<String, BeadId>);
    let mut stores: BTreeMap<PathBuf, std::result::Result<OpenStore, String>> = BTreeMap::new();
    for finding in &violations {
        let path = owning_workspace(finding, &ctx.workspaces, home);
        if stores.contains_key(&path) {
            continue;
        }
        let opened = match super::workspace_store(&path) {
            Ok(store) => match existing_signatures(store.as_ref()).await {
                Ok(signatures) => Ok((store, signatures)),
                Err(error) => Err(format!("{error:#}")),
            },
            Err(error) => Err(format!(
                "failed to open the bead store at {}: {error:#}",
                path.display()
            )),
        };
        stores.insert(path, opened);
    }

    let mut summary = FilingSummary::default();
    let mut findings: Vec<Finding> = Vec::new();
    for finding in &violations {
        let path = owning_workspace(finding, &ctx.workspaces, home);
        match stores.get(&path) {
            Some(Ok(_)) => findings.push((*finding).clone()),
            Some(Err(reason)) => summary.errors.push(FilingError::new(
                FilingErrorKind::FilingFailed,
                finding,
                None,
                None,
                reason.clone(),
            )),
            None => {}
        }
    }

    let lookup = |finding: &Finding| -> Option<BeadId> {
        let path = owning_workspace(finding, &ctx.workspaces, home);
        match stores.get(&path) {
            Some(Ok((_, signatures))) => signatures.get(&signature(finding)).cloned(),
            _ => None,
        }
    };
    let mut planned = plan(&findings, &lookup, ctx.config.audit.max_beads_per_run);

    // An escalating finding gets its brief before its bead, so the bead names
    // a path that is already on disk rather than one it hopes for (N-T60). A
    // brief that cannot be written is reported, and the bead is still filed
    // without naming one: a bead with no brief is worse than one with a
    // brief, and much better than no bead at all.
    let brief_dir = super::escalation::default_brief_dir(&ctx.config);
    for item in planned
        .iter_mut()
        .filter(|item| is_escalating(&item.finding))
    {
        let workspace = owning_workspace(&item.finding, &ctx.workspaces, home);
        let stalled = super::factory::stalled_loop_beads(ctx, &workspace);
        match super::escalation::write_brief(&brief_dir, &item.finding, &stalled, ctx.collected_at)
        {
            Ok(path) => item.escalation = Some(path),
            Err(error) => summary.errors.push(FilingError::new(
                FilingErrorKind::FilingFailed,
                &item.finding,
                None,
                None,
                format!("escalation brief not written: {error:#}"),
            )),
        }
    }

    for item in &planned {
        // Resolved from the finding rather than from a parallel index: `plan`
        // drops intra-run duplicate signatures, so a positional index into
        // `violations` can run short and file a finding into another
        // workspace's store.
        let path = owning_workspace(&item.finding, &ctx.workspaces, home);
        let Some(Ok((store, _))) = stores.get(&path) else {
            continue;
        };
        apply(store.as_ref(), item, ctx.collected_at, &mut summary).await;
    }
    summary
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
        /// Bead ids whose notes the store refuses, with the refusal text —
        /// the shape of a bead leased by another claimant.
        refuse_notes: Mutex<BTreeMap<String, String>>,
        /// Every write attempted against the store, refused or not, so a
        /// test can prove nothing retried or reached for a credential.
        attempts: Mutex<Vec<String>>,
        /// Explicit priorities beads were created at, by title.
        priorities: Mutex<Vec<(String, u8)>>,
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
            self.attempts
                .lock()
                .expect("attempts lock")
                .push(format!("append_notes {id}"));
            if let Some(refusal) = self
                .refuse_notes
                .lock()
                .expect("refuse lock")
                .get(id.as_ref())
            {
                anyhow::bail!("{refusal}");
            }
            self.notes
                .lock()
                .expect("notes lock")
                .push((id.clone(), note.to_string()));
            Ok(())
        }

        async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
            self.attempts
                .lock()
                .expect("attempts lock")
                .push(format!("create_bead {title}"));
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

        async fn create_bead_with_priority(
            &self,
            title: &str,
            body: &str,
            labels: &[&str],
            priority: u8,
        ) -> Result<BeadId> {
            self.priorities
                .lock()
                .expect("priorities lock")
                .push((title.to_string(), priority));
            self.create_bead(title, body, labels).await
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
            apply(&store, item, now(), &mut summary).await;
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
            apply(&store, item, now(), &mut summary).await;
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
        apply(&store, &planned[0], now(), &mut summary).await;

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
        apply(&store, &planned[0], now(), &mut summary).await;

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

    /// The bead-rs refusal the 2026-10-12..16 audits died on, verbatim in
    /// shape: the finding bead is leased by a worker that is not running.
    const LEASE_REFUSAL: &str = "backend 'bead-rs' operation 'append_notes' exited with code 4: \
        bead: Lease conflict: Claim-epoch credential required: issue needle-held is claimed at \
        claim epoch 15; pass --fencing-token 15 (the current epoch is projected by \
        `bead show needle-held --json`)";

    /// needle-39b6d6e4: a repeat note refused on a leased bead is recorded as
    /// `filing_blocked` naming the holder, is attempted exactly once with no
    /// credential, and never stops the other findings from filing.
    #[tokio::test]
    async fn a_lease_refused_note_is_filing_blocked_and_the_run_carries_on() {
        let held = violation("F1_WORKSPACE_NO_VERIFIED_CLOSURES", "NEEDLE", "NEEDLE");
        let fresh = violation("F4_CI_RED", "NEEDLE", "needle-ci");
        let noted = violation("F4_CI_RED", "SEAM", "seam-ci");

        let mut held_bead = bead(
            "needle-held",
            BeadStatus::InProgress,
            &[&signature_label(&held)],
        );
        held_bead.assignee = Some("codex-luna-cgov-03".to_string());
        let store = MemoryStore::with(vec![
            held_bead,
            bead("needle-open", BeadStatus::Open, &[&signature_label(&noted)]),
        ]);
        store
            .refuse_notes
            .lock()
            .expect("refuse lock")
            .insert("needle-held".to_string(), LEASE_REFUSAL.to_string());

        let existing = existing_signatures(&store).await.expect("inventory reads");
        // The held finding sorts first, so a fatal refusal would have stopped
        // both findings after it.
        let findings = vec![held.clone(), fresh, noted];
        let planned = plan(
            &findings,
            &|finding| existing.get(&signature(finding)).cloned(),
            3,
        );
        let mut summary = FilingSummary::default();
        for item in &planned {
            apply(&store, item, now(), &mut summary).await;
        }

        assert_eq!(summary.created.len(), 1, "the new finding still filed");
        assert_eq!(
            summary.noted,
            vec![BeadId::from("needle-open".to_string())],
            "the unheld repeat still noted"
        );
        assert_eq!(summary.errors.len(), 1);
        assert_eq!(summary.blocked(), 1);
        let error = &summary.errors[0];
        assert_eq!(error.kind, FilingErrorKind::FilingBlocked);
        assert_eq!(error.kind.as_str(), "filing_blocked");
        assert_eq!(error.bead.as_deref(), Some("needle-held"));
        assert_eq!(
            error.holder.as_deref(),
            Some("codex-luna-cgov-03"),
            "the holder is named"
        );
        assert_eq!(error.signature, signature(&held));
        assert!(error
            .describe()
            .contains("lease held by codex-luna-cgov-03"));

        // One attempt on the held bead, and nothing else touched it: no retry
        // with the epoch the refusal suggests, no claim, no release.
        let attempts = store.attempts.lock().expect("attempts lock").clone();
        assert_eq!(
            attempts
                .iter()
                .filter(|attempt| attempt.as_str() == "append_notes needle-held")
                .count(),
            1,
            "the refused note is never retried: {attempts:?}"
        );
        assert!(
            !store
                .notes()
                .iter()
                .any(|(id, _)| id.as_ref() == "needle-held"),
            "nothing was written to the held bead"
        );
    }

    /// Any other refusal is `filing_failed`, still non-fatal, and names no
    /// holder it does not know.
    #[tokio::test]
    async fn a_non_lease_note_failure_is_filing_failed() {
        let finding = violation("F4_CI_RED", "NEEDLE", "needle-ci");
        let store = MemoryStore::with(vec![bead(
            "needle-x",
            BeadStatus::Open,
            &[&signature_label(&finding)],
        )]);
        store
            .refuse_notes
            .lock()
            .expect("refuse lock")
            .insert("needle-x".to_string(), "database is locked".to_string());

        let planned = plan(
            std::slice::from_ref(&finding),
            &|_| Some(BeadId::from("needle-x".to_string())),
            3,
        );
        let mut summary = FilingSummary::default();
        apply(&store, &planned[0], now(), &mut summary).await;

        assert_eq!(summary.errors.len(), 1);
        assert_eq!(summary.errors[0].kind, FilingErrorKind::FilingFailed);
        assert_eq!(summary.errors[0].holder, None);
        assert_eq!(summary.blocked(), 0);
        assert!(summary.errors[0].reason.contains("database is locked"));
    }

    /// needle-9c66c160: a red-at-HEAD gate is filed at P0 as an operational
    /// defect — never `human`, never a proposal — and a second run notes the
    /// same bead instead of filing a twin.
    #[tokio::test]
    async fn a_gate_red_at_head_is_filed_p0_as_an_operational_defect() {
        let store = MemoryStore::default();
        let finding = Finding::violation(
            crate::cli::audit::factory::F6_GATE_RED_AT_HEAD,
            "NEEDLE",
            "gate_1",
            "gate `gate_1` (`cargo test --lib`) failed work_failure on 7 distinct bead(s)",
            7,
        );
        assert!(is_operational_defect(&finding));
        assert_eq!(priority_for(&finding), Some(0));

        let planned = plan(std::slice::from_ref(&finding), &|_| None, 3);
        let mut summary = FilingSummary::default();
        apply(&store, &planned[0], now(), &mut summary).await;
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);

        let (title, body, labels) = store.created()[0].clone();
        assert_eq!(
            store.priorities.lock().expect("priorities lock").clone(),
            vec![(title.clone(), 0)],
            "filed at an explicit P0"
        );
        assert!(labels.contains(&OPERATIONAL_DEFECT_LABEL.to_string()));
        for forbidden in [HUMAN_LABEL, "proposal-pending", ESCALATION_LABEL] {
            assert!(
                !labels.iter().any(|label| label == forbidden),
                "an operational defect never carries `{forbidden}`: {labels:?}"
            );
        }
        assert!(body.contains("cargo test --lib"), "{body}");
        assert!(body.contains("operational defect"), "{body}");

        // Another run over the same gate in the same workspace notes it.
        let existing = BeadId::from("audit-1".to_string());
        let again = plan(
            std::slice::from_ref(&finding),
            &|_| Some(existing.clone()),
            3,
        );
        assert_eq!(again[0].action, Filing::Note(existing));

        // An ordinary finding keeps the backend default priority.
        let ordinary = violation("F4_CI_RED", "NEEDLE", "needle-ci");
        assert_eq!(priority_for(&ordinary), None);
        assert!(!labels_for(&ordinary).contains(&OPERATIONAL_DEFECT_LABEL.to_string()));
    }

    #[test]
    fn lease_refusals_are_recognised_by_bead_rs_wording() {
        assert!(is_lease_refusal(LEASE_REFUSAL));
        assert!(is_lease_refusal("bead: Lease error: lease expired"));
        assert!(is_lease_refusal(
            "Claim-epoch credential mismatch: issue x is claimed at claim epoch 3, not 2"
        ));
        assert!(!is_lease_refusal("database is locked"));
        assert!(!is_lease_refusal("Issue not found: needle-x"));
    }
}
