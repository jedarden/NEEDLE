//! Impact receipts: an admitted change is kept only if it measurably worked
//! (N-T55, `needle-c4e6424a`; ADR-029 step 5).
//!
//! This is the stage that makes the loop *measured* rather than merely
//! automatic. An implementing agent's report that a change helped is not
//! evidence — six false closes were detected in eleven hours on 2026-09-12 —
//! so a receipt recomputes the proposal's own acceptance measure over its own
//! cohort and decides promote or withdraw against a baseline taken before
//! exposure.
//!
//! Three rules shape it:
//!
//! - **The proposal chose its own falsification condition.** A receipt reads
//!   exactly the [`AcceptanceMeasure`] the proposal declared and nothing else.
//!   There is no path by which a change that missed its measure argues that a
//!   different measure improved.
//! - **A contaminated cohort holds.** If an operator commit or a second
//!   proposal touched the same cohort during the horizon, the measured delta
//!   is not attributable, and attributing it anyway would teach the loop from
//!   noise. Holding is not a failure state; it is the honest one.
//! - **Receipts are append-only.** A receipt is evidence about a decision that
//!   was already made. Rewriting one would make the audit trail a record of
//!   what the loop currently believes rather than of what it did.
//!
//! Per Gate D, every measure here is computed from `costed = true` rows only,
//! excluding decomposed and fixture rows (ADR-030): an uncosted attempt's cost
//! is unknown rather than zero, and counting it as zero flatters every
//! per-dollar figure.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::envelope::{
    AcceptanceMeasure, EvidenceClass, ImpactMeasure, ImprovementProposal, ProposalScope,
};
use crate::cli::audit::factory::{
    costed, field, row_workspace, INFRASTRUCTURE_FAILURE, VERIFIED_SUCCESS,
};
use crate::evidence_routing::LedgerRow;
use crate::state_dir;

/// Schema version of a persisted receipt.
pub const IMPACT_RECEIPT_SCHEMA_VERSION: u32 = 1;

/// bead-rs ref namespace linking a receipt to the bead it judged.
pub const RECEIPT_REF_NAMESPACE: &str = "needle-receipt";

/// File receipts are appended to, under the state directory.
pub const RECEIPTS_FILE: &str = "improvements/receipts.jsonl";

/// Thresholds governing promote/withdraw.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReceiptThresholds {
    /// Costed attempts a cohort needs in both windows before a delta means
    /// anything.
    pub min_attempts_per_window: u64,
}

impl Default for ReceiptThresholds {
    fn default() -> Self {
        ReceiptThresholds {
            min_attempts_per_window: 10,
        }
    }
}

/// The population a receipt measures.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cohort {
    /// Workspaces exposed to the change.
    pub workspaces: Vec<String>,
    /// Adapters exposed to the change.
    pub adapters: Vec<String>,
    /// Beads exposed to the change.
    #[serde(default)]
    pub beads: Vec<String>,
}

impl Cohort {
    /// The cohort a proposal's scope describes.
    pub fn from_scope(scope: &ProposalScope, beads: Vec<String>) -> Self {
        Cohort {
            workspaces: scope.workspaces.clone(),
            adapters: scope.adapters.clone(),
            beads,
        }
    }

    /// Whether a ledger row belongs to this cohort.
    ///
    /// An empty dimension does not constrain: a cohort naming only workspaces
    /// matches every adapter in them. A cohort naming nothing matches nothing
    /// — the alternative would silently measure the whole fleet.
    pub fn contains(&self, row: &LedgerRow) -> bool {
        if self.workspaces.is_empty() && self.adapters.is_empty() {
            return false;
        }
        let workspace_ok =
            self.workspaces.is_empty() || self.workspaces.contains(&row_workspace(row));
        let adapter_ok =
            self.adapters.is_empty() || self.adapters.contains(&field(row, "adapter").to_string());
        workspace_ok && adapter_ok
    }
}

/// The section 10 measures over one window of one cohort.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CohortMeasures {
    /// Costed, judged attempts in the window.
    pub attempts: u64,
    /// Verified closures among them.
    pub verified: u64,
    /// Dollars spent.
    pub cost_usd: f64,
    /// Verified closures per judged attempt.
    pub yield_per_attempt: f64,
    /// Verified closures per dollar.
    pub yield_per_dollar: f64,
    /// Share of attempts that repeated the target fingerprint.
    pub fingerprint_recurrence: f64,
    /// Share of attempts that resolved as infrastructure failures.
    pub infrastructure_share: f64,
    /// Share of verified closures later reopened.
    pub false_close_rate: f64,
}

impl CohortMeasures {
    /// The value of one measure.
    pub fn value(&self, measure: ImpactMeasure) -> f64 {
        match measure {
            ImpactMeasure::VerifiedYieldPerAttempt => self.yield_per_attempt,
            ImpactMeasure::VerifiedYieldPerDollar => self.yield_per_dollar,
            ImpactMeasure::FingerprintRecurrence => self.fingerprint_recurrence,
            ImpactMeasure::FalseCloseRate => self.false_close_rate,
            ImpactMeasure::InfrastructureShare => self.infrastructure_share,
        }
    }
}

/// Measure a cohort over a window of ledger rows.
///
/// `target_fingerprint` is the terminal reason a fingerprint-recurrence
/// proposal is trying to eliminate; `reopened` names beads later reopened,
/// which the ledger itself does not record.
pub fn measure(
    rows: &[LedgerRow],
    cohort: &Cohort,
    target_fingerprint: Option<&str>,
    reopened: &[String],
) -> CohortMeasures {
    let live: Vec<&LedgerRow> = rows
        .iter()
        .filter(|row| !state_dir::is_fixture_row(field(row, "worker"), field(row, "workspace")))
        .filter(|row| field(row, "outcome") != crate::attempt_accounting::DECOMPOSED)
        // Gate D: uncosted rows are excluded before any receipt is trusted.
        .filter(|row| costed(row))
        .filter(|row| cohort.contains(row))
        .collect();

    let attempts = live.len() as u64;
    if attempts == 0 {
        return CohortMeasures::default();
    }

    let verified = live
        .iter()
        .filter(|row| field(row, "outcome") == VERIFIED_SUCCESS)
        .count() as u64;
    let infrastructure = live
        .iter()
        .filter(|row| field(row, "outcome") == INFRASTRUCTURE_FAILURE)
        .count() as u64;
    let cost_usd: f64 = live
        .iter()
        .map(|row| {
            row.data
                .get("estimated_cost_usd")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
        })
        .sum();
    let recurrences = target_fingerprint
        .map(|target| {
            live.iter()
                .filter(|row| field(row, "terminal_reason") == target)
                .count() as u64
        })
        .unwrap_or(0);
    let reopened_here = live
        .iter()
        .filter(|row| field(row, "outcome") == VERIFIED_SUCCESS)
        .filter(|row| reopened.iter().any(|bead| bead == field(row, "bead_id")))
        .count() as u64;

    CohortMeasures {
        attempts,
        verified,
        cost_usd,
        yield_per_attempt: verified as f64 / attempts as f64,
        yield_per_dollar: if cost_usd > 0.0 {
            verified as f64 / cost_usd
        } else {
            0.0
        },
        fingerprint_recurrence: recurrences as f64 / attempts as f64,
        infrastructure_share: infrastructure as f64 / attempts as f64,
        false_close_rate: if verified > 0 {
            reopened_here as f64 / verified as f64
        } else {
            0.0
        },
    }
}

/// What contaminated a cohort, if anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Contamination {
    /// An operator commit landed in the cohort during the horizon.
    OperatorCommit {
        /// The commit, for the operator reading the receipt.
        commit: String,
    },
    /// Another admitted proposal overlapped this cohort.
    ConcurrentProposal {
        /// The other proposal's signature.
        signature: String,
    },
}

/// What a receipt concluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum ReceiptDecision {
    /// The acceptance measure moved far enough: keep the change.
    Promote,
    /// It did not: undo the change through a revert proposal.
    Withdraw {
        /// Why, in the operator's terms.
        detail: String,
    },
    /// Not decidable yet or not attributable: decide later.
    Hold {
        /// Why the decision was deferred.
        detail: String,
    },
}

/// One receipt: what was changed, over what, measured how, and decided what.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpactReceipt {
    /// Schema version of this record.
    pub schema_version: u32,
    /// The proposal this judges.
    pub signature: String,
    /// The evidence class it came from.
    pub evidence_class: EvidenceClass,
    /// The population measured.
    pub cohort: Cohort,
    /// The falsification condition the proposal declared.
    pub acceptance: AcceptanceMeasure,
    /// Measures before exposure.
    pub baseline: CohortMeasures,
    /// Measures at the horizon.
    pub observed: CohortMeasures,
    /// Observed minus baseline, in the measure's own units and sign.
    pub delta: f64,
    /// Anything that made the delta unattributable.
    pub contamination: Vec<Contamination>,
    /// The decision.
    pub decision: ReceiptDecision,
    /// When the receipt was written.
    pub decided_at: DateTime<Utc>,
}

impl ImpactReceipt {
    /// The bead-rs unique ref linking this receipt to its bead.
    pub fn unique_ref(&self) -> String {
        format!("{RECEIPT_REF_NAMESPACE}:{}", self.signature)
    }
}

/// Decide promote, withdraw or hold for one admitted proposal.
///
/// Pure. The contamination list is supplied by the caller, which is what knows
/// about commits and other admissions.
pub fn decide(
    proposal: &ImprovementProposal,
    cohort: Cohort,
    baseline: CohortMeasures,
    observed: CohortMeasures,
    contamination: Vec<Contamination>,
    thresholds: &ReceiptThresholds,
    decided_at: DateTime<Utc>,
) -> ImpactReceipt {
    let measure = proposal.acceptance.measure;
    let before = baseline.value(measure);
    let after = observed.value(measure);
    // Signed in the measure's own direction: positive is always "moved the way
    // the proposal wanted", so a reader does not have to remember whether
    // lower is better for this particular measure.
    let delta = match proposal.acceptance.direction {
        super::envelope::Direction::Increase => after - before,
        super::envelope::Direction::Decrease => before - after,
    };

    let decision = if !contamination.is_empty() {
        // Contamination is checked first: an unattributable delta must not be
        // promoted *or* withdrawn, whichever way it happens to point.
        ReceiptDecision::Hold {
            detail: format!(
                "{} change(s) touched this cohort during the horizon, so the \
                 delta is not attributable to the proposal",
                contamination.len()
            ),
        }
    } else if baseline.attempts < thresholds.min_attempts_per_window
        || observed.attempts < thresholds.min_attempts_per_window
    {
        ReceiptDecision::Hold {
            detail: format!(
                "cohort has {} costed attempts before and {} after, below the \
                 {} needed for a delta to mean anything",
                baseline.attempts, observed.attempts, thresholds.min_attempts_per_window
            ),
        }
    } else if proposal
        .acceptance
        .direction
        .satisfied(before, after, proposal.acceptance.min_delta)
    {
        ReceiptDecision::Promote
    } else {
        ReceiptDecision::Withdraw {
            detail: format!(
                "{measure} moved {delta:+.4} against a required {:+.4}; a change \
                 that does not move its acceptance measure is withdrawn whatever \
                 its author reports",
                proposal.acceptance.min_delta
            ),
        }
    };

    ImpactReceipt {
        schema_version: IMPACT_RECEIPT_SCHEMA_VERSION,
        signature: proposal.signature.clone(),
        evidence_class: proposal.evidence_class,
        cohort,
        acceptance: proposal.acceptance,
        baseline,
        observed,
        delta,
        contamination,
        decision,
        decided_at,
    }
}

/// The revert proposal a withdrawal emits.
///
/// Delivered through the same admission and delivery path as any other
/// proposal, at the original authority level — a withdrawal is a change like
/// any other and does not get a privileged route. Returns `None` for a
/// receipt that did not withdraw.
pub fn revert_proposal(
    receipt: &ImpactReceipt,
    original: &ImprovementProposal,
    now: DateTime<Utc>,
) -> Option<ImprovementProposal> {
    let ReceiptDecision::Withdraw { detail } = &receipt.decision else {
        return None;
    };

    ImprovementProposal::new(
        original.evidence_class,
        original.evidence.clone(),
        original.scope.clone(),
        format!(
            "revert {}: {}",
            original.signature, original.rollback.description
        ),
        original.authority,
        format!("restores the pre-change baseline ({detail})"),
        original.acceptance,
        super::envelope::Rollback {
            description: format!("re-apply {}", original.signature),
            automatic: original.rollback.automatic,
        },
        now,
        original.generator_version,
    )
    .ok()
}

/// Where receipts are appended.
pub fn receipts_path() -> PathBuf {
    state_dir::state_root().join(RECEIPTS_FILE)
}

/// Append a receipt, creating the file if needed.
///
/// Append-only by construction: there is no update or delete here. A receipt
/// records a decision that was already taken, and a rewritable audit trail is
/// not an audit trail.
pub fn append(path: &Path, receipt: &ImpactReceipt) -> Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut line = serde_json::to_string(receipt).context("failed to encode an impact receipt")?;
    line.push('\n');

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("failed to append to {}", path.display()))
}

/// Read every receipt, oldest first.
///
/// A malformed line is skipped rather than failing the read: one bad record
/// must not make every other receipt unreadable.
pub fn read_all(path: &Path) -> Result<Vec<ImpactReceipt>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}
