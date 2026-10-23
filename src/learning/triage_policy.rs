//! Outcome tracking for the subgov proposal-triage rule as a candidate policy
//! (needle-eea2a19a; plan §5 needle-learning).
//!
//! The operator-approved triage rule (`proposal-triage-2026-10-23.1`,
//! subgov-91eebc6b) auto-admits and auto-rejects weave-generated proposals and
//! appends one JSON receipt per decision to
//! `~/.local/state/subgov/proposal-triage-receipts.jsonl`. This module turns
//! that receipt stream into the evidence a candidate policy needs before it
//! may enter the self-learning lifecycle:
//!
//! 1. **Ingest** — [`parse_receipts`] reads the JSONL exposure log.
//! 2. **Join** — [`join_receipts_to_outcomes`] resolves each triaged bead's
//!    mature outcome through a caller-supplied resolver and buckets the
//!    results per action class. Admitted beads are measured as shipped vs.
//!    failed/retried/quarantined; rejected beads are measured by whether they
//!    were later reopened (the rule's own close reason invites that).
//! 3. **Evaluate** — [`evaluate_evidence`] compares the joined rates against
//!    [`EvidenceThresholds`] and returns the promote/demote/hold verdict.
//! 4. **Register** — [`candidate_policy_document`] renders the report and
//!    verdict as a `needle.candidate-lesson/v1` document, so registration and
//!    any later promotion go through the reviewed, receipted
//!    `crate::cli::lesson` boundary rather than a side door.
//!
//! Like the rest of `src::learning`, this module is pure records: it never
//! mutates a bead store or instruction file (plan 5.1 — controllers own
//! effects), and promotion stays operator-reviewed even on a `Promote`
//! verdict.

use std::fmt::Write as _;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// First line a triage rejection writes into a bead's close reason and notes
/// (`scripts/proposal-triage.py`, `apply_workspace`). Its presence in a bead
/// that is open again identifies a false rejection.
pub const REJECT_CLOSE_MARKER: &str = "Rejected by proposal triage";

/// One proposal-triage decision receipt (one JSON line).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageReceipt {
    pub id: String,
    pub workspace: String,
    /// `admit-evidence`, `admit-security`, `reject-dup`, `reject-test`,
    /// `reject-doc` — other values (e.g. dry-run `leave`) are not receipted.
    pub action: String,
    #[serde(default)]
    pub reason: String,
    /// RFC3339 decision timestamp; defaulted so hand-written fixtures and a
    /// torn tail line cannot discard a receipt.
    #[serde(default)]
    pub at: String,
    pub rule: String,
    /// Set on the live (non-dry-run) path; a failed application is still an
    /// exposure decision but is reported separately via the `ok` histogram.
    #[serde(default)]
    pub ok: Option<bool>,
}

/// Parse the receipts JSONL, skipping blank or malformed lines.
///
/// Receipts are machine-written and append-only; a torn trailing line must not
/// discard the whole stream, so malformed lines are skipped rather than fatal.
/// The report's `total_receipts` therefore counts well-formed rows only.
pub fn parse_receipts(jsonl: &str) -> Vec<TriageReceipt> {
    jsonl
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// True when the receipt's action admits the proposal into the queue.
pub fn is_admission(action: &str) -> bool {
    action.starts_with("admit")
}

/// True when the receipt's action rejects the proposal.
pub fn is_rejection(action: &str) -> bool {
    action.starts_with("reject")
}

/// Mature outcome of one triaged bead, resolved from its workspace's store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOutcome {
    /// Closed and not reopened: for an admission, work landed; for a
    /// rejection, the closure held.
    Closed,
    /// Open again after the triage decision: a reopened rejection is the
    /// rule's false-negative signal.
    Reopened,
    /// Failure pressure without closure: failed attempts, retry/cooldown or
    /// cycling labels.
    FailedOrRetried,
    /// Quarantined or manually blocked.
    Quarantined,
    /// Still open (or deferred) with no attempt and no failure pressure —
    /// not yet evidence in either direction.
    Unmatured,
}

/// The subset of a `bead list --json` row the classifier needs.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BeadStoreRow {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub manual_blocked: bool,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub attempts: BeadAttempts,
}

/// Attempt counters from a `bead list --json` row.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BeadAttempts {
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub consecutive_failures: u32,
}

const FAILURE_LABELS: [&str; 3] = ["retry-cooldown", "failure-count", "cycling"];

/// Classify one bead-store row into a [`PolicyOutcome`].
///
/// Heuristic by design: it reads only the row the list endpoint already
/// exposes, so a join over 4k receipts costs one `bead list` per workspace.
/// Closure semantics (shipped vs. zero-commit close) are deliberately not
/// inferred here — the report counts `Closed` and the candidate document
/// quotes the raw counts.
pub fn classify_bead_row(row: &BeadStoreRow) -> PolicyOutcome {
    if row.manual_blocked || row.labels.iter().any(|l| l.starts_with("quarantine")) {
        return PolicyOutcome::Quarantined;
    }
    let rejected_by_triage = row
        .notes
        .as_deref()
        .is_some_and(|notes| notes.contains(REJECT_CLOSE_MARKER));
    match row.status.as_str() {
        "closed" => PolicyOutcome::Closed,
        "open" | "in_progress" if rejected_by_triage => PolicyOutcome::Reopened,
        "open" | "in_progress" => {
            if row.attempts.consecutive_failures > 0
                || row
                    .labels
                    .iter()
                    .any(|l| FAILURE_LABELS.contains(&l.as_str()))
            {
                PolicyOutcome::FailedOrRetried
            } else {
                PolicyOutcome::Unmatured
            }
        }
        _ => PolicyOutcome::Unmatured,
    }
}

/// Receipts joined to mature outcomes, bucketed per action class.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExposureReport {
    pub rule_version: String,
    pub total_receipts: usize,
    pub admitted_receipts: usize,
    pub rejected_receipts: usize,
    /// Receipts whose action is neither admit nor reject (future actions).
    pub untriaged_receipts: usize,
    // Admitted-bead outcome histogram.
    pub admitted_closed: usize,
    pub admitted_reopened: usize,
    pub admitted_failed_or_retried: usize,
    pub admitted_quarantined: usize,
    pub admitted_unmatured: usize,
    // Rejected-bead outcome histogram.
    pub rejected_stayed_closed: usize,
    pub rejected_reopened: usize,
    pub rejected_unmatured: usize,
    /// Admitted/rejected beads the resolver could not resolve at all.
    pub unresolved_receipts: usize,
    /// Admissions whose recorded application failed (`ok: false`).
    pub failed_applications: usize,
    /// Outcomes that count as evidence: everything except `Unmatured` and
    /// unresolved rows.
    pub mature_samples: usize,
}

/// Resolve one receipt's bead to its mature outcome; `None` means the store
/// could not answer (bead gone, workspace unavailable).
pub type OutcomeResolver<'a> = &'a dyn Fn(&TriageReceipt) -> Option<PolicyOutcome>;

/// Join receipts to outcomes through `outcome_of`.
///
/// The resolver keeps this pure: bead-store access stays with the caller, so
/// the report is reproducible from a snapshot and the kernel never touches a
/// store (plan 5.1).
pub fn join_receipts_to_outcomes(
    receipts: &[TriageReceipt],
    outcome_of: OutcomeResolver<'_>,
) -> ExposureReport {
    let mut report = ExposureReport {
        rule_version: receipts
            .first()
            .map(|receipt| receipt.rule.clone())
            .unwrap_or_default(),
        total_receipts: receipts.len(),
        ..ExposureReport::empty()
    };
    for receipt in receipts {
        if receipt.ok == Some(false) {
            report.failed_applications += 1;
        }
        if is_admission(&receipt.action) {
            report.admitted_receipts += 1;
        } else if is_rejection(&receipt.action) {
            report.rejected_receipts += 1;
        } else {
            report.untriaged_receipts += 1;
            continue;
        }
        let Some(outcome) = outcome_of(receipt) else {
            report.unresolved_receipts += 1;
            continue;
        };
        match (is_admission(&receipt.action), outcome) {
            (true, PolicyOutcome::Closed) => {
                report.admitted_closed += 1;
                report.mature_samples += 1;
            }
            (true, PolicyOutcome::Reopened) => {
                report.admitted_reopened += 1;
                report.mature_samples += 1;
            }
            (true, PolicyOutcome::FailedOrRetried) => {
                report.admitted_failed_or_retried += 1;
                report.mature_samples += 1;
            }
            (true, PolicyOutcome::Quarantined) => {
                report.admitted_quarantined += 1;
                report.mature_samples += 1;
            }
            (true, PolicyOutcome::Unmatured) => report.admitted_unmatured += 1,
            (false, PolicyOutcome::Closed) => {
                report.rejected_stayed_closed += 1;
                report.mature_samples += 1;
            }
            (false, PolicyOutcome::Reopened) => {
                report.rejected_reopened += 1;
                report.mature_samples += 1;
            }
            (false, PolicyOutcome::FailedOrRetried | PolicyOutcome::Quarantined) => {
                // A rejected bead under failure pressure was closed without
                // work; that pressure predates or is unrelated to the
                // rejection, so it is neither a false rejection nor evidence
                // of one.
                report.rejected_unmatured += 1;
            }
            (false, PolicyOutcome::Unmatured) => report.rejected_unmatured += 1,
        }
    }
    report
}

impl ExposureReport {
    fn empty() -> Self {
        Self {
            rule_version: String::new(),
            total_receipts: 0,
            admitted_receipts: 0,
            rejected_receipts: 0,
            untriaged_receipts: 0,
            admitted_closed: 0,
            admitted_reopened: 0,
            admitted_failed_or_retried: 0,
            admitted_quarantined: 0,
            admitted_unmatured: 0,
            rejected_stayed_closed: 0,
            rejected_reopened: 0,
            rejected_unmatured: 0,
            unresolved_receipts: 0,
            failed_applications: 0,
            mature_samples: 0,
        }
    }

    /// Admissions with a resolved, mature outcome.
    pub fn admitted_mature(&self) -> usize {
        self.admitted_closed
            + self.admitted_reopened
            + self.admitted_failed_or_retried
            + self.admitted_quarantined
    }

    /// Rejections with a resolved, mature outcome.
    pub fn rejected_mature(&self) -> usize {
        self.rejected_stayed_closed + self.rejected_reopened
    }

    /// Share of matured admissions that closed with their work kept.
    pub fn shipped_rate(&self) -> f64 {
        rate(self.admitted_closed, self.admitted_mature())
    }

    /// Share of matured rejections that were later reopened.
    pub fn false_rejection_rate(&self) -> f64 {
        rate(self.rejected_reopened, self.rejected_mature())
    }
}

fn rate(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// Thresholds separating a promote verdict from a demote or a hold.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvidenceThresholds {
    /// Mature samples required on both sides before promotion.
    pub promote_min_mature_samples: usize,
    /// Minimum matured-admission shipped rate for promotion.
    pub promote_min_shipped_rate: f64,
    /// Maximum matured-rejection reopen rate for promotion.
    pub promote_max_false_rejection_rate: f64,
    /// Mature samples that justify a demote on strong negative evidence.
    pub demote_min_mature_samples: usize,
    /// Below this shipped rate the rule is doing more harm than holding.
    pub demote_max_shipped_rate: f64,
    /// Above this reopen rate the rule rejects work the fleet wants.
    pub demote_min_false_rejection_rate: f64,
}

impl Default for EvidenceThresholds {
    fn default() -> Self {
        Self {
            promote_min_mature_samples: 30,
            promote_min_shipped_rate: 0.80,
            promote_max_false_rejection_rate: 0.05,
            demote_min_mature_samples: 10,
            demote_max_shipped_rate: 0.50,
            demote_min_false_rejection_rate: 0.20,
        }
    }
}

/// The evidence check's verdict for the candidate policy.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum EvidenceVerdict {
    /// Evidence supports promoting the rule to standing policy.
    Promote {
        shipped_rate: f64,
        false_rejection_rate: f64,
        mature_samples: usize,
    },
    /// Evidence argues the rule is net-harmful as written.
    Demote {
        reason: String,
        shipped_rate: f64,
        false_rejection_rate: f64,
        mature_samples: usize,
    },
    /// Not enough mature evidence yet, or mixed signals — keep observing.
    Hold { reason: String },
}

/// Compare a joined report against `thresholds`.
///
/// Demotion is checked first: strong negative evidence outranks reaching the
/// promote bar on the other side.
pub fn evaluate_evidence(
    report: &ExposureReport,
    thresholds: &EvidenceThresholds,
) -> EvidenceVerdict {
    let shipped = report.shipped_rate();
    let false_rejection = report.false_rejection_rate();
    let mature = report.mature_samples;
    let rejected_mature = report.rejected_mature();
    if report.admitted_mature() >= thresholds.demote_min_mature_samples
        && shipped < thresholds.demote_max_shipped_rate
    {
        return EvidenceVerdict::Demote {
            reason: format!(
                "only {shipped:.2} of matured admissions closed with kept work (demote bar {:.2})",
                thresholds.demote_max_shipped_rate
            ),
            shipped_rate: shipped,
            false_rejection_rate: false_rejection,
            mature_samples: mature,
        };
    }
    if rejected_mature >= thresholds.demote_min_mature_samples
        && false_rejection > thresholds.demote_min_false_rejection_rate
    {
        return EvidenceVerdict::Demote {
            reason: format!(
                "{false_rejection:.2} of matured rejections were reopened (demote bar {:.2})",
                thresholds.demote_min_false_rejection_rate
            ),
            shipped_rate: shipped,
            false_rejection_rate: false_rejection,
            mature_samples: mature,
        };
    }
    let admission_ready = report.admitted_mature() >= thresholds.promote_min_mature_samples
        && shipped >= thresholds.promote_min_shipped_rate;
    let rejection_ready = rejected_mature >= thresholds.promote_min_mature_samples
        && false_rejection <= thresholds.promote_max_false_rejection_rate;
    if admission_ready && rejection_ready {
        EvidenceVerdict::Promote {
            shipped_rate: shipped,
            false_rejection_rate: false_rejection,
            mature_samples: mature,
        }
    } else {
        EvidenceVerdict::Hold {
            reason: format!(
                "promotion needs >= {} mature samples on both sides (admissions {}, \
                 rejections {rejected_mature}) with shipped >= {:.2} and false rejections <= {:.2}",
                thresholds.promote_min_mature_samples,
                report.admitted_mature(),
                thresholds.promote_min_shipped_rate,
                thresholds.promote_max_false_rejection_rate,
            ),
        }
    }
}

/// Stable candidate-lesson ID for a rule version.
pub fn candidate_id(rule_version: &str) -> String {
    format!("triage-policy.{rule_version}")
}

/// Inputs for rendering the candidate policy document.
pub struct CandidatePolicyInput<'a> {
    pub report: &'a ExposureReport,
    pub verdict: &'a EvidenceVerdict,
    /// Evidence reference recorded in the frontmatter; must resolve from the
    /// repository root when the document is promoted (receipts file, report
    /// artifact, or attempt ID).
    pub evidence_ref: String,
    /// One-line scope statement for the candidate policy.
    pub scope: String,
    /// Reviewer of record. Promotion is refused while this is empty.
    pub reviewed_by: String,
    /// Review expiry; `None` defaults to 90 days out so a stale candidate
    /// cannot sit in the promotion queue forever.
    pub expires: Option<DateTime<Utc>>,
}

/// Render the report and verdict as a `needle.candidate-lesson/v1` document
/// that [`crate::cli::lesson::promote`] accepts.
pub fn candidate_policy_document(input: &CandidatePolicyInput<'_>) -> String {
    let report = input.report;
    let id = candidate_id(&report.rule_version);
    let fingerprint = report_fingerprint(report);
    let expiry = input
        .expires
        .unwrap_or_else(|| Utc::now() + Duration::days(90))
        .format("%Y-%m-%d");
    let scope = yaml_scalar(&one_line(&input.scope));
    let reviewer = yaml_scalar(&one_line(&input.reviewed_by));
    let evidence = yaml_scalar(&one_line(&input.evidence_ref));
    let mut document = String::new();
    let _ = writeln!(
        document,
        "---\nschema: needle.candidate-lesson/v1\nid: {id}\n\
         fingerprint: {fingerprint}\nevidence_refs:\n  - {evidence}\n\
         scope: {scope}\nexpiry: {expiry}\nreviewed_by: {reviewer}\n---\n"
    );
    let _ = writeln!(
        document,
        "\n## Candidate policy: subgov proposal-triage {}\n",
        report.rule_version
    );
    let _ = writeln!(
        document,
        "Joined {} triage receipts to mature bead outcomes: {} admissions \
         ({} closed, {} reopened, {} failed/retried, {} quarantined, {} unmatured), \
         {} rejections ({} stayed closed, {} reopened), {} unresolved, \
         {} failed applications.\n",
        report.total_receipts,
        report.admitted_receipts,
        report.admitted_closed,
        report.admitted_reopened,
        report.admitted_failed_or_retried,
        report.admitted_quarantined,
        report.admitted_unmatured,
        report.rejected_receipts,
        report.rejected_stayed_closed,
        report.rejected_reopened,
        report.unresolved_receipts,
        report.failed_applications,
    );
    let _ = writeln!(
        document,
        "Matured evidence: {} samples; shipped rate {:.3}; false-rejection rate {:.3}.\n",
        report.mature_samples,
        report.shipped_rate(),
        report.false_rejection_rate(),
    );
    let _ = match input.verdict {
        EvidenceVerdict::Promote {
            shipped_rate,
            false_rejection_rate,
            mature_samples,
        } => writeln!(
            document,
            "Evidence verdict: promote — shipped rate {shipped_rate:.3}, \
             false-rejection rate {false_rejection_rate:.3} over {mature_samples} mature samples."
        ),
        EvidenceVerdict::Demote { reason, .. } => {
            writeln!(document, "Evidence verdict: demote — {reason}.")
        }
        EvidenceVerdict::Hold { reason } => {
            writeln!(document, "Evidence verdict: hold — {reason}.")
        }
    };
    document
}

/// Content fingerprint of a report (first 16 hex bytes of SHA-256 over its
/// serialized form). Identifies the evidence snapshot in the frontmatter; it
/// is a label, not a security boundary.
pub fn report_fingerprint(report: &ExposureReport) -> String {
    let serialized = serde_json::to_vec(report).unwrap_or_default();
    let digest = Sha256::digest(&serialized);
    let mut hex = String::with_capacity(32);
    for byte in &digest[..16] {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Quote a frontmatter scalar so colons and other YAML punctuation stay
/// part of the value.
fn yaml_scalar(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Collapse a caller-supplied line to one line so the rendered frontmatter
/// stays valid YAML and passes the single-line validation on promotion.
fn one_line(value: &str) -> String {
    let collapsed: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.replace("-->", "—>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(id: &str, action: &str) -> TriageReceipt {
        TriageReceipt {
            id: id.to_owned(),
            workspace: "/tmp/ws".to_owned(),
            action: action.to_owned(),
            reason: "fixture".to_owned(),
            at: "2026-10-23T08:29:48+00:00".to_owned(),
            rule: "proposal-triage-2026-10-23.1".to_owned(),
            ok: Some(true),
        }
    }

    #[test]
    fn parse_skips_malformed_lines_and_keeps_fields() {
        let jsonl = concat!(
            "{\"id\": \"a-1\", \"workspace\": \"/w\", \"action\": \"admit-evidence\", ",
            "\"reason\": \"dated failure\", \"at\": \"t1\", \"rule\": \"r1\", \"ok\": true}\n",
            "not json at all\n",
            "\n",
            "{\"id\": \"a-2\", \"workspace\": \"/w\", \"action\": \"reject-dup\", ",
            "\"reason\": \"duplicate of a-1\", \"survivor\": \"a-1\", ",
            "\"at\": \"t2\", \"rule\": \"r1\"}\n",
        );
        let parsed = parse_receipts(jsonl);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "a-1");
        assert_eq!(parsed[0].ok, Some(true));
        assert_eq!(parsed[1].action, "reject-dup");
        assert_eq!(parsed[1].ok, None);
    }

    #[test]
    fn join_buckets_admissions_and_rejections_by_outcome() {
        let receipts = vec![
            receipt("ad-1", "admit-evidence"),
            receipt("ad-2", "admit-security"),
            receipt("rj-1", "reject-dup"),
            receipt("rj-2", "reject-test"),
            receipt("ad-3", "admit-evidence"),
            receipt("rj-3", "reject-doc"),
        ];
        let outcomes: std::collections::HashMap<&str, PolicyOutcome> = [
            ("ad-1", PolicyOutcome::Closed),
            ("ad-2", PolicyOutcome::FailedOrRetried),
            ("ad-3", PolicyOutcome::Unmatured),
            ("rj-1", PolicyOutcome::Reopened),
            ("rj-2", PolicyOutcome::Closed),
        ]
        .into_iter()
        .collect();
        let report = join_receipts_to_outcomes(&receipts, &|receipt| {
            outcomes.get(receipt.id.as_str()).copied()
        });
        assert_eq!(report.rule_version, "proposal-triage-2026-10-23.1");
        assert_eq!(report.admitted_receipts, 3);
        assert_eq!(report.rejected_receipts, 3);
        assert_eq!(report.admitted_closed, 1);
        assert_eq!(report.admitted_failed_or_retried, 1);
        assert_eq!(report.admitted_unmatured, 1);
        assert_eq!(report.rejected_stayed_closed, 1);
        assert_eq!(report.rejected_reopened, 1);
        assert_eq!(report.unresolved_receipts, 1);
        assert_eq!(report.mature_samples, 4);
        assert!((report.shipped_rate() - 0.5).abs() < 1e-9);
        assert!((report.false_rejection_rate() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn evaluate_holds_when_no_outcome_is_mature() {
        let receipts = vec![receipt("ad-1", "admit-evidence")];
        let report = join_receipts_to_outcomes(&receipts, &|_| None);
        let verdict = evaluate_evidence(&report, &EvidenceThresholds::default());
        assert!(matches!(verdict, EvidenceVerdict::Hold { .. }));
        assert!((report.shipped_rate()).abs() < 1e-9);
    }

    #[test]
    fn evaluate_promotes_on_healthy_fixtures() {
        let mut receipts = Vec::new();
        for index in 0..4 {
            receipts.push(receipt(&format!("ad-{index}"), "admit-evidence"));
            receipts.push(receipt(&format!("rj-{index}"), "reject-dup"));
        }
        // 4/4 admissions closed, 0/4 rejections reopened: healthy on both sides.
        let report = join_receipts_to_outcomes(&receipts, &|_| Some(PolicyOutcome::Closed));
        let thresholds = EvidenceThresholds {
            promote_min_mature_samples: 4,
            ..EvidenceThresholds::default()
        };
        let verdict = evaluate_evidence(&report, &thresholds);
        assert_eq!(
            verdict,
            EvidenceVerdict::Promote {
                shipped_rate: 1.0,
                false_rejection_rate: 0.0,
                mature_samples: 8,
            }
        );
    }

    #[test]
    fn evaluate_demotes_on_mass_reopenings() {
        let mut receipts = Vec::new();
        for index in 0..3 {
            receipts.push(receipt(&format!("rj-{index}"), "reject-test"));
        }
        let report = join_receipts_to_outcomes(&receipts, &|_| Some(PolicyOutcome::Reopened));
        let thresholds = EvidenceThresholds {
            demote_min_mature_samples: 3,
            ..EvidenceThresholds::default()
        };
        let verdict = evaluate_evidence(&report, &thresholds);
        let EvidenceVerdict::Demote { reason, .. } = &verdict else {
            panic!("expected demote, got {verdict:?}");
        };
        assert!(reason.contains("reopened"), "unexpected reason: {reason}");
    }

    #[test]
    fn demote_outranks_promote_on_conflicting_evidence() {
        let mut receipts = Vec::new();
        for index in 0..3 {
            receipts.push(receipt(&format!("ad-{index}"), "admit-evidence"));
            receipts.push(receipt(&format!("rj-{index}"), "reject-test"));
        }
        let resolver = |receipt: &TriageReceipt| {
            if is_admission(&receipt.action) {
                Some(PolicyOutcome::Closed)
            } else {
                Some(PolicyOutcome::Reopened)
            }
        };
        let report = join_receipts_to_outcomes(&receipts, &resolver);
        let thresholds = EvidenceThresholds {
            promote_min_mature_samples: 3,
            demote_min_mature_samples: 3,
            ..EvidenceThresholds::default()
        };
        assert!(matches!(
            evaluate_evidence(&report, &thresholds),
            EvidenceVerdict::Demote { .. }
        ));
    }

    #[test]
    fn classify_reads_row_signals() {
        let row =
            |status: &str, labels: Vec<&str>, notes: Option<&str>, failures: u32| BeadStoreRow {
                status: status.to_owned(),
                labels: labels.into_iter().map(str::to_owned).collect(),
                manual_blocked: false,
                notes: notes.map(str::to_owned),
                attempts: BeadAttempts {
                    count: 0,
                    consecutive_failures: failures,
                },
            };
        assert_eq!(
            classify_bead_row(&row("closed", vec![], Some("shipped"), 0)),
            PolicyOutcome::Closed
        );
        assert_eq!(
            classify_bead_row(&row(
                "open",
                vec![],
                Some("Rejected by proposal triage (duplicate); rule r. Reopen to reconsider."),
                0
            )),
            PolicyOutcome::Reopened
        );
        assert_eq!(
            classify_bead_row(&row("open", vec![], None, 2)),
            PolicyOutcome::FailedOrRetried
        );
        assert_eq!(
            classify_bead_row(&row("open", vec!["retry-cooldown"], None, 0)),
            PolicyOutcome::FailedOrRetried
        );
        assert_eq!(
            classify_bead_row(&row("open", vec!["quarantine-2026-10"], None, 0)),
            PolicyOutcome::Quarantined
        );
        assert_eq!(
            classify_bead_row(&row("open", vec![], None, 0)),
            PolicyOutcome::Unmatured
        );
        assert_eq!(
            classify_bead_row(&row("deferred", vec![], None, 0)),
            PolicyOutcome::Unmatured
        );
    }

    #[test]
    fn candidate_document_is_stable_and_single_line_in_frontmatter() {
        let receipts = vec![receipt("ad-1", "admit-evidence")];
        let report = join_receipts_to_outcomes(&receipts, &|_| None);
        let verdict = evaluate_evidence(&report, &EvidenceThresholds::default());
        let document = candidate_policy_document(&CandidatePolicyInput {
            report: &report,
            verdict: &verdict,
            evidence_ref: "receipts.jsonl".to_owned(),
            scope: "multi\nline\nscope".to_owned(),
            reviewed_by: "fixture-reviewer".to_owned(),
            expires: None,
        });
        assert!(document.starts_with("---\n"));
        assert!(document.contains("id: triage-policy.proposal-triage-2026-10-23.1"));
        let frontmatter = document.split("\n---\n").next().unwrap_or_default();
        for line in frontmatter.lines() {
            assert!(!line.contains('\r'));
        }
        assert!(document.contains("Evidence verdict: hold"));
        // Deterministic for the same report.
        let again = candidate_policy_document(&CandidatePolicyInput {
            report: &report,
            verdict: &verdict,
            evidence_ref: "receipts.jsonl".to_owned(),
            scope: "multi line scope".to_owned(),
            reviewed_by: "fixture-reviewer".to_owned(),
            expires: None,
        });
        assert_eq!(document, again);
    }

    /// Real-data join over the live receipts file and workspace stores.
    ///
    /// Ignored by default: it reads host state outside the checkout
    /// (`~/.local/state/subgov/proposal-triage-receipts.jsonl`) and runs
    /// read-only `bead list` in every triaged workspace. Run it with
    /// `cargo test --lib triage_policy -- --ignored --nocapture` to produce
    /// the joined report for the bead record.
    #[test]
    #[ignore = "host-state join; needs the live receipts file and workspace stores"]
    fn real_receipts_join_report() {
        let path = std::env::var("NEEDLE_TRIAGE_RECEIPTS").unwrap_or_else(|_| {
            format!(
                "{}/.local/state/subgov/proposal-triage-receipts.jsonl",
                std::env::var("HOME").unwrap_or_default()
            )
        });
        let Ok(jsonl) = std::fs::read_to_string(&path) else {
            panic!("receipts file not found at {path}");
        };
        let receipts = parse_receipts(&jsonl);
        let mut by_workspace: std::collections::BTreeMap<&str, Vec<&TriageReceipt>> =
            std::collections::BTreeMap::new();
        for receipt in &receipts {
            by_workspace
                .entry(receipt.workspace.as_str())
                .or_default()
                .push(receipt);
        }
        let mut rows: std::collections::HashMap<(String, String), BeadStoreRow> =
            std::collections::HashMap::new();
        for workspace in by_workspace.keys() {
            let Ok(output) = std::process::Command::new("bead")
                .args(["list", "--json", "--limit", "20000"])
                .current_dir(workspace)
                .output()
            else {
                continue;
            };
            let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
                continue;
            };
            let empty = Vec::new();
            let items = match &parsed {
                serde_json::Value::Array(items) => items,
                value @ serde_json::Value::Object(_) => value
                    .get("issues")
                    .and_then(|issues| issues.as_array())
                    .unwrap_or(&empty),
                _ => &empty,
            };
            for item in items {
                if let Ok(row) = serde_json::from_value::<BeadStoreRow>(item.clone()) {
                    let id = item
                        .get("id")
                        .and_then(|id| id.as_str())
                        .unwrap_or_default()
                        .to_owned();
                    rows.insert(((*workspace).to_owned(), id), row);
                }
            }
        }
        let report = join_receipts_to_outcomes(&receipts, &|receipt| {
            rows.get(&(receipt.workspace.clone(), receipt.id.clone()))
                .map(classify_bead_row)
        });
        let verdict = evaluate_evidence(&report, &EvidenceThresholds::default());
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        println!("{verdict:?}");
        let document = candidate_policy_document(&CandidatePolicyInput {
            report: &report,
            verdict: &verdict,
            evidence_ref: path,
            scope: "Candidate policy: subgov proposal-triage admission rule.".to_owned(),
            reviewed_by: "operator request 2026-10-23 (needle-eea2a19a)".to_owned(),
            expires: None,
        });
        println!("{document}");
        assert!(report.total_receipts > 0);
    }
}
