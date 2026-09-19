//! The proposal generator: improvements proposed from ledger evidence
//! (N-T53, `needle-908c1f25`; ADR-029 step 2).
//!
//! Every improvement that produced a measurable gain between 2026-09-12 and
//! 2026-09-14 was a short wire on data the ledger already held, and a person
//! produced the list. This module is that person's job, written down: a pure
//! reduction from `attempt.resolved` rows to [`ImprovementProposal`] records.
//!
//! It is deliberately a *generator*, not a controller. It reads no file, opens
//! no store, spawns nothing and takes its clock as an argument. The only thing
//! it can do is describe what it saw and what would change it; admission
//! decides whether any of that becomes work.
//!
//! ## Fixture rows are excluded
//!
//! `state_dir::is_fixture_row` rows — test workers, fixture workspaces — are
//! dropped before any denominator is computed (ADR-030). A test harness that
//! contaminated the live ledger must not be able to produce a proposal about
//! itself.
//!
//! ## Refusals are part of the output
//!
//! A proposal whose acceptance measure cannot be computed is refused at
//! construction (plan section 4.10 step 2), and the refusal is *returned*
//! rather than dropped: a class of evidence that is being seen but can never
//! be acted on is exactly the thing an operator needs to know about.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::envelope::{
    AcceptanceMeasure, Direction, EvidenceClass, EvidenceKind, EvidenceRef, ImpactMeasure,
    ImprovementProposal, ProposalRejection, ProposalScope, Rollback,
};
use crate::cli::audit::factory::{
    costed, distinct, field, group_by, judged, row_workspace, verified, yield_points,
    INFRASTRUCTURE_FAILURE, VERIFIED_SUCCESS,
};
use crate::evidence_routing::LedgerRow;
use crate::state_dir;

/// Revision of this generator, stamped onto every proposal it emits.
///
/// A behaviour change here must be visible in the records produced, or a
/// receipt comparing two windows could be comparing two different generators.
pub const GENERATOR_VERSION: u32 = 1;

/// Thresholds the generator reduces against, all operator-owned.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GeneratorThresholds {
    /// Consecutive identical failures before a bead is worth a proposal.
    pub repeated_failure_min: usize,
    /// Attempts an adapter needs in a workspace before its rate is evidence.
    pub adapter_evidence_floor: u64,
    /// Percentage points of yield an alternative adapter must beat the
    /// incumbent by.
    pub adapter_regret_points: f64,
    /// Judged attempts a workspace needs before a zero-yield claim is
    /// evidence rather than noise.
    pub red_baseline_min_attempts: u64,
    /// Share of costed spend going to unverified attempts before it is worth
    /// a proposal, as a fraction.
    pub unverified_spend_share: f64,
    /// Dollars of unverified spend below which the share is not worth acting
    /// on however concentrated.
    pub unverified_spend_floor_usd: f64,
}

impl Default for GeneratorThresholds {
    fn default() -> Self {
        GeneratorThresholds {
            repeated_failure_min: 3,
            adapter_evidence_floor: 20,
            adapter_regret_points: 15.0,
            red_baseline_min_attempts: 10,
            unverified_spend_share: 0.40,
            unverified_spend_floor_usd: 25.0,
        }
    }
}

/// A proposal the generator wanted to emit but could not.
#[derive(Debug, Clone, PartialEq)]
pub struct RefusedProposal {
    /// The class the evidence belonged to.
    pub evidence_class: EvidenceClass,
    /// What the proposal would have been about.
    pub subject: String,
    /// Why construction refused it.
    pub rejection: ProposalRejection,
}

/// What one generator run produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeneratedProposals {
    /// Proposals, in deterministic order: class, then subject.
    pub proposals: Vec<ImprovementProposal>,
    /// Evidence that could not become a proposal, and why.
    pub refused: Vec<RefusedProposal>,
    /// Rows dropped as fixture contamination.
    pub fixture_rows_excluded: usize,
}

/// Generate proposals from a window of ledger rows.
///
/// Pure. `now` is the generation timestamp stamped on each record; the rows'
/// own timestamps bound the evidence.
pub fn generate(
    rows: &[LedgerRow],
    thresholds: &GeneratorThresholds,
    now: DateTime<Utc>,
) -> GeneratedProposals {
    let total = rows.len();
    let live: Vec<&LedgerRow> = rows
        .iter()
        .filter(|row| !state_dir::is_fixture_row(field(row, "worker"), field(row, "workspace")))
        .collect();
    let fixture_rows_excluded = total - live.len();

    let mut out = GeneratedProposals {
        fixture_rows_excluded,
        ..Default::default()
    };

    repeated_identical_failures(&live, thresholds, now, &mut out);
    workspace_adapter_regret(&live, thresholds, now, &mut out);
    unverified_spend_concentration(&live, thresholds, now, &mut out);
    red_baseline_workspaces(&live, thresholds, now, &mut out);

    // Deterministic ordering: the class, then the signature. Two runs over the
    // same window must produce byte-identical output or deduplication
    // downstream is meaningless.
    out.proposals.sort_by(|left, right| {
        left.evidence_class
            .cmp(&right.evidence_class)
            .then_with(|| left.signature.cmp(&right.signature))
    });
    out
}

/// Push a constructed proposal, or record why it could not be constructed.
#[allow(clippy::too_many_arguments)]
fn emit(
    out: &mut GeneratedProposals,
    class: EvidenceClass,
    subject: String,
    evidence: Vec<EvidenceRef>,
    scope: ProposalScope,
    intended_change: String,
    expected_benefit: String,
    acceptance: AcceptanceMeasure,
    rollback: Rollback,
    now: DateTime<Utc>,
) {
    match ImprovementProposal::new(
        class,
        evidence,
        scope,
        intended_change,
        class.max_authority(),
        expected_benefit,
        acceptance,
        rollback,
        now,
        GENERATOR_VERSION,
    ) {
        Ok(proposal) => out.proposals.push(proposal),
        Err(rejection) => out.refused.push(RefusedProposal {
            evidence_class: class,
            subject,
            rejection,
        }),
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Class 1 — one bead failing the same way, over and over
// ──────────────────────────────────────────────────────────────────────────

/// Beads whose most recent run of attempts failed identically `n` times.
///
/// "Identically" is the terminal reason: the same gate, the same exit code.
/// A bead failing three different ways is three problems and is not this
/// class; a bead failing the same way three times is a retry loop that is
/// spending money to re-learn what it already knows.
fn repeated_identical_failures(
    rows: &[&LedgerRow],
    thresholds: &GeneratorThresholds,
    now: DateTime<Utc>,
    out: &mut GeneratedProposals,
) {
    for (bead, bead_rows) in group_by(rows, |row| field(row, "bead_id").to_string()) {
        if bead.is_empty() {
            continue;
        }
        let failures: Vec<&&LedgerRow> = bead_rows
            .iter()
            .filter(|row| {
                let outcome = field(row, "outcome");
                // Decomposed is not a failure (ADR-030): the attempt split its
                // bead instead of delivering it, which earns neither success
                // nor failure credit. Counting it here produced six proposals
                // against the live ledger telling the fleet to stop "failing"
                // at work it had correctly decided to break up.
                outcome != VERIFIED_SUCCESS
                    && outcome != INFRASTRUCTURE_FAILURE
                    && outcome != crate::attempt_accounting::DECOMPOSED
            })
            .collect();
        if failures.len() < thresholds.repeated_failure_min {
            continue;
        }
        // One terminal reason across every failure, or it is not one problem.
        let reasons: Vec<&str> = failures
            .iter()
            .map(|row| field(row, "terminal_reason"))
            .collect();
        let Some(first) = reasons.first().copied() else {
            continue;
        };
        if first.is_empty() || !reasons.iter().all(|reason| *reason == first) {
            continue;
        }

        let workspace = failures
            .first()
            .map(|row| row_workspace(row))
            .unwrap_or_default();
        let mut evidence = vec![EvidenceRef::new(EvidenceKind::Bead, &bead, now)];
        for row in &failures {
            let attempt = field(row, "attempt_id");
            if !attempt.is_empty() {
                evidence.push(EvidenceRef::new(
                    EvidenceKind::Attempt,
                    attempt,
                    row.timestamp.unwrap_or(now),
                ));
            }
        }

        emit(
            out,
            EvidenceClass::RepeatedIdenticalFailures,
            bead.clone(),
            evidence,
            ProposalScope::new([workspace], []),
            format!(
                "{bead} has failed {} times with the identical terminal reason {first:?}; \
                 the next dispatch needs a changed intervention rather than a repeat",
                failures.len()
            ),
            format!(
                "the {first:?} fingerprint stops recurring on {bead}, and the attempts \
                 currently spent re-learning it become available to other work"
            ),
            AcceptanceMeasure {
                measure: ImpactMeasure::FingerprintRecurrence,
                direction: Direction::Decrease,
                min_delta: 0.5,
                horizon_days: 14,
            },
            Rollback {
                description: "restore the previous retry behaviour for this bead".to_string(),
                automatic: false,
            },
            now,
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Class 2 — a workspace on the wrong adapter
// ──────────────────────────────────────────────────────────────────────────

/// Workspaces where an alternative adapter verifies materially better than
/// the incumbent, both above the evidence floor.
fn workspace_adapter_regret(
    rows: &[&LedgerRow],
    thresholds: &GeneratorThresholds,
    now: DateTime<Utc>,
    out: &mut GeneratedProposals,
) {
    for (workspace, workspace_rows) in group_by(rows, row_workspace) {
        if workspace.is_empty() {
            continue;
        }
        let by_adapter = group_by(&workspace_rows, |row| field(row, "adapter").to_string());

        // Every adapter with enough attempts in this workspace to be evidence.
        let mut rates: Vec<(String, f64, u64)> = Vec::new();
        for (adapter, adapter_rows) in &by_adapter {
            if adapter.is_empty() {
                continue;
            }
            let attempts = judged(adapter_rows);
            if attempts < thresholds.adapter_evidence_floor {
                continue;
            }
            if let Some(points) = yield_points(adapter_rows) {
                rates.push((adapter.clone(), points, attempts));
            }
        }
        if rates.len() < 2 {
            continue;
        }

        // Highest rate wins; the name breaks ties so the answer is stable.
        rates.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let (best_name, best_rate, best_attempts) = rates[0].clone();
        let (worst_name, worst_rate, worst_attempts) =
            rates.last().cloned().unwrap_or_else(|| rates[0].clone());
        if best_rate - worst_rate < thresholds.adapter_regret_points {
            continue;
        }

        let evidence = vec![
            EvidenceRef::new(EvidenceKind::Workspace, &workspace, now),
            EvidenceRef::new(EvidenceKind::Adapter, &best_name, now),
            EvidenceRef::new(EvidenceKind::Adapter, &worst_name, now),
        ];

        emit(
            out,
            EvidenceClass::WorkspaceAdapterRegret,
            workspace.clone(),
            evidence,
            ProposalScope::new([workspace.clone()], [best_name.clone(), worst_name.clone()]),
            format!(
                "in {workspace}, {best_name} verifies {best_rate:.1}% over {best_attempts} \
                 attempts against {worst_name}'s {worst_rate:.1}% over {worst_attempts}; \
                 move this workspace's routing evidence towards {best_name}"
            ),
            format!(
                "verified-closure yield per attempt in {workspace} rises towards \
                 {best_rate:.1}%"
            ),
            AcceptanceMeasure {
                measure: ImpactMeasure::VerifiedYieldPerAttempt,
                direction: Direction::Increase,
                min_delta: 0.05,
                horizon_days: 7,
            },
            Rollback {
                description: format!("restore {worst_name} as {workspace}'s routing default"),
                automatic: true,
            },
            now,
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Class 3 — money spent on attempts that never verified
// ──────────────────────────────────────────────────────────────────────────

/// Workspaces spending a large share of their costed dollars on attempts that
/// produced no verified closure.
fn unverified_spend_concentration(
    rows: &[&LedgerRow],
    thresholds: &GeneratorThresholds,
    now: DateTime<Utc>,
    out: &mut GeneratedProposals,
) {
    for (workspace, workspace_rows) in group_by(rows, row_workspace) {
        if workspace.is_empty() {
            continue;
        }
        // Costed rows only: an uncosted attempt's cost is unknown, never zero
        // (ADR-030), so including it would understate the share.
        let costed_rows: Vec<&&LedgerRow> =
            workspace_rows.iter().filter(|row| costed(row)).collect();
        if costed_rows.is_empty() {
            continue;
        }

        let cost_of = |row: &LedgerRow| -> f64 {
            row.data
                .get("estimated_cost_usd")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
        };
        let total: f64 = costed_rows.iter().map(|row| cost_of(row)).sum();
        let unverified: f64 = costed_rows
            .iter()
            .filter(|row| field(row, "outcome") != VERIFIED_SUCCESS)
            .map(|row| cost_of(row))
            .sum();
        if total <= 0.0 || unverified < thresholds.unverified_spend_floor_usd {
            continue;
        }
        let share = unverified / total;
        if share < thresholds.unverified_spend_share {
            continue;
        }

        let adapters = distinct(&workspace_rows, "adapter");
        let mut evidence = vec![EvidenceRef::new(EvidenceKind::Workspace, &workspace, now)];
        for adapter in &adapters {
            evidence.push(EvidenceRef::new(EvidenceKind::Adapter, adapter, now));
        }

        emit(
            out,
            EvidenceClass::UnverifiedSpendConcentration,
            workspace.clone(),
            evidence,
            ProposalScope::new([workspace.clone()], adapters.clone()),
            format!(
                "{workspace} spent ${unverified:.2} of ${total:.2} costed dollars \
                 ({:.0}%) on attempts that never verified; cap the per-attempt spend \
                 so a failing attempt stops before it costs a succeeding one's budget",
                share * 100.0
            ),
            format!("verified closures per dollar in {workspace} rise"),
            AcceptanceMeasure {
                measure: ImpactMeasure::VerifiedYieldPerDollar,
                direction: Direction::Increase,
                min_delta: 0.01,
                horizon_days: 14,
            },
            Rollback {
                description: format!("restore the previous spend cap for {workspace}"),
                automatic: true,
            },
            now,
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Class 4 — a workspace where nothing can pass
// ──────────────────────────────────────────────────────────────────────────

/// Workspaces with enough judged attempts to be evidence and zero verified
/// closures among them.
fn red_baseline_workspaces(
    rows: &[&LedgerRow],
    thresholds: &GeneratorThresholds,
    now: DateTime<Utc>,
    out: &mut GeneratedProposals,
) {
    for (workspace, workspace_rows) in group_by(rows, row_workspace) {
        if workspace.is_empty() {
            continue;
        }
        let attempts = judged(&workspace_rows);
        if attempts < thresholds.red_baseline_min_attempts {
            continue;
        }
        if verified(&workspace_rows) > 0 {
            continue;
        }

        let mut evidence = vec![EvidenceRef::new(EvidenceKind::Workspace, &workspace, now)];
        // A bounded sample, chosen by sorted attempt id rather than by input
        // position. Taking the first five rows would make the signature depend
        // on the order the ledger files happened to be read in, and a
        // signature that moves with read order defeats deduplication: the same
        // red workspace would file a second bead on the next run.
        let mut sampled: Vec<(String, DateTime<Utc>)> = workspace_rows
            .iter()
            .map(|row| {
                (
                    field(row, "attempt_id").to_string(),
                    row.timestamp.unwrap_or(now),
                )
            })
            .filter(|(attempt, _)| !attempt.is_empty())
            .collect();
        sampled.sort();
        sampled.dedup();
        for (attempt, observed) in sampled.into_iter().take(5) {
            evidence.push(EvidenceRef::new(EvidenceKind::Attempt, attempt, observed));
        }

        emit(
            out,
            EvidenceClass::RedBaselineWorkspace,
            workspace.clone(),
            evidence,
            ProposalScope::new([workspace.clone()], []),
            format!(
                "{workspace} produced 0 verified closures across {attempts} judged \
                 attempts; establish whether its baseline is red or its gates cannot \
                 run before dispatching more work into it"
            ),
            format!("{workspace} starts producing verified closures at all"),
            AcceptanceMeasure {
                measure: ImpactMeasure::VerifiedYieldPerAttempt,
                direction: Direction::Increase,
                min_delta: 0.05,
                horizon_days: 7,
            },
            Rollback {
                description: format!("revert the baseline change in {workspace}"),
                automatic: false,
            },
            now,
        );
    }
}

/// Proposals indexed by signature, for admission's lookup.
pub fn by_signature(generated: &GeneratedProposals) -> BTreeMap<String, ImprovementProposal> {
    generated
        .proposals
        .iter()
        .map(|proposal| (proposal.signature.clone(), proposal.clone()))
        .collect()
}
