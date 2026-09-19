//! `needle improvements` — the read-only view of the improvement loop
//! (N-T56, `needle-65e66131`; ADR-029 step 6).
//!
//! Under ADR-029 the operator's job is to read receipts, not to curate
//! proposals. Nothing rendered them, which made the loop unauditable in
//! exactly the way the ADR exists to prevent: an automatic process whose
//! output nobody can see is indistinguishable from one that is not running.
//!
//! This command mutates nothing. It reads the ledger, regenerates the current
//! proposals from it, and reads the persisted decision and receipt journals.
//! Regenerating rather than reading a proposal cache is deliberate: a view
//! that could disagree with what the generator would produce right now is a
//! second source of truth, and the first thing it would hide is the generator
//! having stopped.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::cli::audit::factory::{costed, field, VERIFIED_SUCCESS};
use crate::config::Config;
use crate::evidence_routing::{timestamped_ledger_rows, LedgerRow};
use crate::learning::improvement::{
    generate, AdmissionRecord, GeneratedProposals, GeneratorThresholds, ImpactReceipt,
};
use crate::state_dir;

/// File admission decisions are appended to, under the state directory.
pub const DECISIONS_FILE: &str = "improvements/decisions.jsonl";

/// Default window, in days, the view regenerates proposals over.
pub const DEFAULT_WINDOW_DAYS: u32 = 7;

/// Proposals rendered in the operator view before it summarizes instead.
const HUMAN_PROPOSAL_LIMIT: usize = 15;

/// The rolling fleet trend an operator reads beside the receipts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetTrend {
    /// Costed attempts in the window.
    pub attempts: u64,
    /// Verified closures among them.
    pub verified: u64,
    /// Verified closures per costed attempt.
    pub yield_per_attempt: f64,
    /// Dollars spent on costed attempts.
    pub cost_usd: f64,
    /// Dollars per verified closure. `None` when nothing verified — an
    /// unknown cost per closure, never a cost of zero.
    pub cost_per_verified: Option<f64>,
}

/// Everything the command renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImprovementsReport {
    /// When the view was taken.
    pub collected_at: DateTime<Utc>,
    /// Days of ledger the proposals were regenerated over.
    pub window_days: u32,
    /// Proposals the generator produces from the current window.
    pub generated: GeneratedView,
    /// Persisted admission decisions, newest last.
    pub decisions: Vec<AdmissionRecord>,
    /// Persisted impact receipts, newest last.
    pub receipts: Vec<ImpactReceipt>,
    /// The rolling trend.
    pub trend: FleetTrend,
    /// Inputs that could not be read, named rather than silently skipped.
    pub inputs_unavailable: Vec<String>,
}

/// The generator's current output, flattened for rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedView {
    /// One row per proposal.
    pub proposals: Vec<ProposalRow>,
    /// Evidence that could not become a proposal.
    pub refused: Vec<RefusedRow>,
    /// Fixture rows excluded from every denominator.
    pub fixture_rows_excluded: usize,
}

/// One proposal, as rendered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalRow {
    /// Deduplication signature.
    pub signature: String,
    /// The evidence class that produced it.
    pub evidence_class: String,
    /// The autonomy level it needs.
    pub authority: String,
    /// Workspaces in scope.
    pub workspaces: Vec<String>,
    /// The change being asked for.
    pub intended_change: String,
    /// The measure that will decide it.
    pub acceptance_measure: String,
    /// The horizon that measure is read at.
    pub horizon_days: u32,
    /// Its state, joined from the decision and receipt journals.
    pub state: String,
}

/// One refusal at construction, as rendered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefusedRow {
    /// The class the evidence belonged to.
    pub evidence_class: String,
    /// What it was about.
    pub subject: String,
    /// Why it could not be constructed.
    pub reason: String,
}

/// Where admission decisions are journalled.
pub fn decisions_path() -> PathBuf {
    state_dir::state_root().join(DECISIONS_FILE)
}

/// Collect the view. Reads only.
pub fn collect(
    config: &Config,
    window_days: u32,
    now: DateTime<Utc>,
) -> Result<ImprovementsReport> {
    let mut inputs_unavailable = Vec::new();

    let logs = state_dir::logs_dir();
    let rows = timestamped_ledger_rows(&logs, window_days);
    if rows.is_empty() {
        inputs_unavailable.push(format!(
            "no attempt.resolved rows under {} in the last {window_days}d",
            logs.display()
        ));
    }

    let thresholds = GeneratorThresholds::default();
    let generated = generate(&rows, &thresholds, now);

    let decisions = read_decisions(&decisions_path()).unwrap_or_else(|error| {
        inputs_unavailable.push(format!("decisions journal unreadable: {error:#}"));
        Vec::new()
    });
    let receipts =
        crate::learning::improvement::read_receipts(&crate::learning::improvement::receipts_path())
            .unwrap_or_else(|error| {
                inputs_unavailable.push(format!("receipts journal unreadable: {error:#}"));
                Vec::new()
            });

    let _ = config;
    Ok(ImprovementsReport {
        collected_at: now,
        window_days,
        generated: flatten(&generated, &decisions, &receipts),
        decisions,
        receipts,
        trend: trend(&rows),
        inputs_unavailable,
    })
}

/// Join the generator's output with what has been decided about it.
fn flatten(
    generated: &GeneratedProposals,
    decisions: &[AdmissionRecord],
    receipts: &[ImpactReceipt],
) -> GeneratedView {
    let proposals = generated
        .proposals
        .iter()
        .map(|proposal| {
            // The newest word about a proposal wins: a receipt supersedes an
            // admission, which supersedes "generated but never decided".
            let state = receipts
                .iter()
                .rev()
                .find(|receipt| receipt.signature == proposal.signature)
                .map(|receipt| match &receipt.decision {
                    crate::learning::improvement::ReceiptDecision::Promote => {
                        "promoted".to_string()
                    }
                    crate::learning::improvement::ReceiptDecision::Withdraw { .. } => {
                        "withdrawn".to_string()
                    }
                    crate::learning::improvement::ReceiptDecision::Hold { .. } => {
                        "held".to_string()
                    }
                })
                .or_else(|| {
                    decisions
                        .iter()
                        .rev()
                        .find(|record| record.signature == proposal.signature)
                        .map(|record| match &record.decision {
                            crate::learning::improvement::AdmissionDecision::Admitted {
                                ..
                            } => "admitted".to_string(),
                            crate::learning::improvement::AdmissionDecision::Refused { reason } => {
                                format!("refused:{}", reason.tag())
                            }
                        })
                })
                .unwrap_or_else(|| "undecided".to_string());

            ProposalRow {
                signature: proposal.signature.clone(),
                evidence_class: proposal.evidence_class.as_str().to_string(),
                authority: proposal.authority.as_str().to_string(),
                workspaces: proposal.scope.workspaces.clone(),
                intended_change: proposal.intended_change.clone(),
                acceptance_measure: proposal.acceptance.measure.as_str().to_string(),
                horizon_days: proposal.acceptance.horizon_days,
                state,
            }
        })
        .collect();

    let refused = generated
        .refused
        .iter()
        .map(|refusal| RefusedRow {
            evidence_class: refusal.evidence_class.as_str().to_string(),
            subject: refusal.subject.clone(),
            reason: refusal.rejection.to_string(),
        })
        .collect();

    GeneratedView {
        proposals,
        refused,
        fixture_rows_excluded: generated.fixture_rows_excluded,
    }
}

/// The rolling fleet trend over costed, non-fixture rows.
fn trend(rows: &[LedgerRow]) -> FleetTrend {
    let live: Vec<&LedgerRow> = rows
        .iter()
        .filter(|row| !state_dir::is_fixture_row(field(row, "worker"), field(row, "workspace")))
        .filter(|row| field(row, "outcome") != crate::attempt_accounting::DECOMPOSED)
        .filter(|row| costed(row))
        .collect();

    let attempts = live.len() as u64;
    if attempts == 0 {
        return FleetTrend::default();
    }
    let verified = live
        .iter()
        .filter(|row| field(row, "outcome") == VERIFIED_SUCCESS)
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

    FleetTrend {
        attempts,
        verified,
        yield_per_attempt: verified as f64 / attempts as f64,
        cost_usd,
        cost_per_verified: (verified > 0).then(|| cost_usd / verified as f64),
    }
}

/// Read the admission decision journal.
pub fn read_decisions(path: &Path) -> Result<Vec<AdmissionRecord>> {
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

/// Append one admission decision to the journal.
///
/// Used by the admission adapter; kept here so the writer and the reader of
/// this file are defined together.
pub fn append_decision(path: &Path, record: &AdmissionRecord) -> Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut line =
        serde_json::to_string(record).context("failed to encode an admission decision")?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("failed to append to {}", path.display()))
}

/// The machine-readable report.
pub fn render_json(report: &ImprovementsReport) -> Result<String> {
    serde_json::to_string_pretty(report).context("failed to encode the improvements report")
}

/// The operator view.
pub fn render_human(report: &ImprovementsReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "NEEDLE improvements — {} ({}d window)\n\n",
        report.collected_at.format("%Y-%m-%d %H:%M UTC"),
        report.window_days
    ));

    // Trend first: it is the thing every receipt is ultimately about.
    let trend = &report.trend;
    out.push_str("FLEET TREND (costed rows only, decomposed and fixture excluded)\n");
    if trend.attempts == 0 {
        out.push_str("  no costed attempts in the window\n");
    } else {
        out.push_str(&format!(
            "  {} costed attempts, {} verified ({:.1}% yield)\n",
            trend.attempts,
            trend.verified,
            trend.yield_per_attempt * 100.0
        ));
        match trend.cost_per_verified {
            Some(cost) => out.push_str(&format!(
                "  ${:.2} spent, ${cost:.2} per verified closure\n",
                trend.cost_usd
            )),
            None => out.push_str(&format!(
                "  ${:.2} spent, cost per verified closure unknown (nothing verified)\n",
                trend.cost_usd
            )),
        }
    }

    out.push_str("\nPROPOSALS FROM CURRENT EVIDENCE\n");
    if report.generated.proposals.is_empty() {
        out.push_str("  none: no evidence class cleared its threshold in this window\n");
    } else {
        // Summary first, then a bounded list. At fleet scale this class runs to
        // dozens of proposals, and a detector that prints sixty
        // indistinguishable lines is one nobody reads to the end. The JSON
        // report carries all of them.
        let mut by_class: std::collections::BTreeMap<&str, usize> =
            std::collections::BTreeMap::new();
        for proposal in &report.generated.proposals {
            *by_class
                .entry(proposal.evidence_class.as_str())
                .or_insert(0) += 1;
        }
        out.push_str(&format!("  {} total:", report.generated.proposals.len()));
        for (class, count) in &by_class {
            out.push_str(&format!(" {class}={count}"));
        }
        out.push('\n');
        if report.generated.proposals.len() > HUMAN_PROPOSAL_LIMIT {
            out.push_str(&format!(
                "  showing the first {HUMAN_PROPOSAL_LIMIT}; --json for all\n"
            ));
        }
        for proposal in report.generated.proposals.iter().take(HUMAN_PROPOSAL_LIMIT) {
            out.push_str(&format!(
                "  [{}] {} {} — {}\n      {}\n      decided by {} at {}d\n",
                proposal.state,
                proposal.authority,
                proposal.signature,
                proposal.evidence_class,
                proposal.intended_change,
                proposal.acceptance_measure,
                proposal.horizon_days,
            ));
        }
    }

    if !report.generated.refused.is_empty() {
        out.push_str("\nEVIDENCE THAT COULD NOT BECOME A PROPOSAL\n");
        for refusal in &report.generated.refused {
            out.push_str(&format!(
                "  {} {}: {}\n",
                refusal.evidence_class, refusal.subject, refusal.reason
            ));
        }
    }

    out.push_str("\nRECEIPTS\n");
    if report.receipts.is_empty() {
        out.push_str("  none yet\n");
    } else {
        for receipt in report.receipts.iter().rev().take(20) {
            let decision = match &receipt.decision {
                crate::learning::improvement::ReceiptDecision::Promote => "PROMOTE".to_string(),
                crate::learning::improvement::ReceiptDecision::Withdraw { detail } => {
                    format!("WITHDRAW ({detail})")
                }
                crate::learning::improvement::ReceiptDecision::Hold { detail } => {
                    format!("HOLD ({detail})")
                }
            };
            out.push_str(&format!(
                "  {} {} {} delta={:+.4} — {}\n",
                receipt.decided_at.format("%Y-%m-%d"),
                receipt.signature,
                receipt.evidence_class.as_str(),
                receipt.delta,
                decision
            ));
        }
    }

    if report.generated.fixture_rows_excluded > 0 {
        out.push_str(&format!(
            "\n{} fixture row(s) excluded from every denominator\n",
            report.generated.fixture_rows_excluded
        ));
    }

    if !report.inputs_unavailable.is_empty() {
        out.push_str("\nINPUTS UNAVAILABLE\n");
        for reason in &report.inputs_unavailable {
            out.push_str(&format!("  {reason}\n"));
        }
    }

    out
}
