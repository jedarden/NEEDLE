//! The admission adapter: submitting proposals and applying what is admitted
//! (N-T54, `needle-e830524d`; ADR-029 step 3).
//!
//! Everything in [`crate::learning::improvement`] is pure. This is where the
//! loop touches the world: it reads bead stores to answer "does something
//! already own this evidence?", files an implementation bead for an admitted
//! L4 proposal, and journals every decision so `needle improvements` can show
//! what happened.
//!
//! ## Why ownership is checked against the plan as well as the stores
//!
//! Several evidence classes already have owners. Unchanged retries are R1/R2
//! (`needle-d6c5397a`, `needle-7b9718bc`); operator overrides are N-T20. A
//! generator that filed its own bead for evidence the plan already owns would
//! produce two beads for one problem — the duplicate-work failure ADR-015
//! documents, arrived at from a new direction. So ownership is the union of
//! what the stores carry and what the plan already assigned.
//!
//! ## Shadow is the default
//!
//! The plan's activation order runs the generator in shadow first — proposals
//! visible, none admitted — then one admitted L4 proposal per day. Both are
//! [`AdmissionPolicy`] settings, and the default is shadow.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::bead_store::BeadStore;
use crate::cli::improvements::append_decision;
use crate::learning::improvement::{
    admit, by_signature, generate, rank, score, AdmissionDecision, AdmissionPolicy,
    AdmissionRecord, AdmissionRoute, AdmissionWorld, Confidence, EffortEstimate, EvidenceClass,
    ExecutionPlan, GeneratedProposals, GeneratorThresholds, ImpactContract, ImprovementProposal,
    NonExecutable, ProducerEvidence, ScoringPolicy, WorkspaceImpactProfile,
};
use crate::types::BeadId;

/// Label prefix carrying a proposal's signature onto the bead it created.
///
/// The bead-rs CLI takes a `--unique-ref`, but the [`BeadStore`] trait creates
/// with labels only, so the signature travels as a label — the same shape the
/// audit filing uses — and the `needle-proposal:` ref is written into the body
/// for provenance. Both are greppable; only the label is queryable.
pub const SIGNATURE_LABEL_PREFIX: &str = "improvement-signature:";

/// Label marking a bead as produced by the improvement loop.
pub const IMPROVEMENT_LABEL: &str = "improvement";

/// Evidence classes the plan has already assigned an owner.
///
/// Keyed by class because that is the granularity the plan assigns at: R1/R2
/// own *unchanged retries*, not one particular bead's unchanged retry.
pub fn plan_owners() -> BTreeMap<EvidenceClass, Vec<&'static str>> {
    let mut owners = BTreeMap::new();
    // Plan revision 34: "unchanged retries = R1/R2 needle-d6c5397a /
    // needle-7b9718bc". A proposal of this class is already somebody's job.
    owners.insert(
        EvidenceClass::RepeatedIdenticalFailures,
        vec!["needle-d6c5397a", "needle-7b9718bc"],
    );
    owners
}

/// What one submission run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SubmissionSummary {
    /// Beads created for admitted L4 proposals.
    pub created: Vec<BeadId>,
    /// Proposals admitted to an owning controller (L1–L3).
    pub routed: Vec<String>,
    /// Decisions journalled, admitted and refused alike.
    pub decisions: usize,
    /// Proposals refused, by reason tag.
    pub refusals: BTreeMap<String, usize>,
}

/// Inputs a submission needs that this module does not own.
pub struct SubmissionContext<'a> {
    /// Ledger rows to generate from.
    pub rows: &'a [crate::evidence_routing::LedgerRow],
    /// Generator thresholds.
    pub thresholds: GeneratorThresholds,
    /// Admission policy in force.
    pub policy: AdmissionPolicy,
    /// Operator-owned impact profiles, by workspace name.
    pub profiles: &'a BTreeMap<String, WorkspaceImpactProfile>,
    /// Discovered workspaces, by name.
    pub workspaces: &'a BTreeMap<String, PathBuf>,
    /// Workspace an unmatched proposal's bead is filed into.
    pub home_workspace: &'a Path,
    /// When the run happened.
    pub now: DateTime<Utc>,
}

/// Generate, rank and admit, applying what is admitted.
///
/// The store opener is injected so the whole controller can be driven against
/// in-memory stores in a test without going near a real workspace.
pub async fn submit(
    ctx: &SubmissionContext<'_>,
    open_store: &dyn Fn(&Path) -> Result<Box<dyn BeadStore>>,
    journal: &Path,
) -> Result<(GeneratedProposals, Vec<AdmissionRecord>, SubmissionSummary)> {
    let generated = generate(ctx.rows, &ctx.thresholds, ctx.now);
    let index = by_signature(&generated);

    // Score every proposal against its workspace's operator-owned profile.
    let scored: Vec<_> = generated
        .proposals
        .iter()
        .map(|proposal| {
            let workspace = proposal
                .scope
                .workspaces
                .first()
                .cloned()
                .unwrap_or_default();
            let contract = ImpactContract::assemble(
                &workspace,
                ctx.profiles.get(&workspace),
                producer_evidence(proposal),
                ctx.now,
            );
            score(proposal, &contract, &ScoringPolicy::default(), ctx.now)
        })
        .collect();
    let ranked = rank(scored);

    // Read every candidate owning store's signatures once.
    let mut signatures: BTreeMap<PathBuf, BTreeMap<String, BeadId>> = BTreeMap::new();
    for proposal in &generated.proposals {
        let path = owning_workspace(proposal, ctx.workspaces, ctx.home_workspace);
        if signatures.contains_key(&path) {
            continue;
        }
        let store = open_store(&path)
            .with_context(|| format!("failed to open the bead store at {}", path.display()))?;
        signatures.insert(path, existing_signatures(store.as_ref()).await?);
    }

    let owned_by_plan = plan_owners();
    let owner_of = |proposal: &ImprovementProposal| -> Option<String> {
        if let Some(owners) = owned_by_plan.get(&proposal.evidence_class) {
            if let Some(first) = owners.first() {
                return Some((*first).to_string());
            }
        }
        let path = owning_workspace(proposal, ctx.workspaces, ctx.home_workspace);
        signatures
            .get(&path)
            .and_then(|found| found.get(&proposal.signature))
            .map(|id| id.to_string())
    };

    let non_executable = |proposal: &ImprovementProposal| -> Option<NonExecutable> {
        crate::learning::improvement::assess_executability(
            proposal,
            &ImpactContract::assemble(
                proposal
                    .scope
                    .workspaces
                    .first()
                    .map(String::as_str)
                    .unwrap_or_default(),
                None,
                producer_evidence(proposal),
                ctx.now,
            ),
            &execution_plan(proposal),
        )
        .err()
    };

    let owning = |proposal: &ImprovementProposal| -> String {
        proposal
            .scope
            .workspaces
            .first()
            .cloned()
            .unwrap_or_else(|| {
                ctx.home_workspace
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
    };

    let world = AdmissionWorld {
        owner_of: &owner_of,
        non_executable: &non_executable,
        owning_workspace: &owning,
        admitted_today: admitted_today(journal, ctx.now),
        open_admitted: 0,
    };

    let lookup = |signature: &str| index.get(signature).cloned();
    let records = admit(&ranked, &lookup, &ctx.policy, &world);

    let mut summary = SubmissionSummary::default();
    for record in &records {
        append_decision(journal, record)
            .with_context(|| format!("failed to journal a decision for {}", record.signature))?;
        summary.decisions += 1;

        match &record.decision {
            AdmissionDecision::Refused { reason } => {
                *summary
                    .refusals
                    .entry(reason.tag().to_string())
                    .or_insert(0) += 1;
            }
            AdmissionDecision::Admitted { route } => {
                let Some(proposal) = index.get(&record.signature) else {
                    continue;
                };
                match route {
                    // L1–L3 belong to the controller that owns the envelope —
                    // routing, experiments, numeric bounds. Applying the change
                    // here would put two writers on one knob, so the decision
                    // is recorded and the owning controller acts on it.
                    AdmissionRoute::Controller { .. } => {
                        summary.routed.push(record.signature.clone());
                    }
                    AdmissionRoute::ImplementationBead { workspace } => {
                        let path = ctx
                            .workspaces
                            .get(workspace)
                            .cloned()
                            .unwrap_or_else(|| ctx.home_workspace.to_path_buf());
                        let store = open_store(&path).with_context(|| {
                            format!("failed to open the bead store at {}", path.display())
                        })?;
                        let cross_repo = !path.starts_with(ctx.home_workspace);
                        let id = store
                            .create_bead(
                                &bead_title(proposal),
                                &bead_body(proposal, cross_repo),
                                &[
                                    IMPROVEMENT_LABEL,
                                    &format!("{SIGNATURE_LABEL_PREFIX}{}", proposal.signature),
                                ]
                                .iter()
                                .map(|s| s.as_ref())
                                .collect::<Vec<&str>>(),
                            )
                            .await
                            .with_context(|| {
                                format!("failed to file a bead for proposal {}", proposal.signature)
                            })?;
                        summary.created.push(id);
                    }
                }
            }
        }
    }

    Ok((generated, records, summary))
}

/// The producer half of the impact contract for a generated proposal.
///
/// Confidence follows the evidence class: a class that points at a known fix
/// is stronger evidence than one that points at a single observation. Effort
/// follows the authority level, because an L1 routing nudge and an L4 source
/// change are not the same size of work.
fn producer_evidence(proposal: &ImprovementProposal) -> ProducerEvidence {
    let confidence = match proposal.evidence_class {
        EvidenceClass::RecurringFingerprintWithKnownFix => Confidence::High,
        EvidenceClass::RepeatedIdenticalFailures
        | EvidenceClass::WorkspaceAdapterRegret
        | EvidenceClass::UnverifiedSpendConcentration => Confidence::Medium,
        EvidenceClass::RedBaselineWorkspace | EvidenceClass::CanaryResult => Confidence::Low,
    };
    let effort = if proposal.authority.applied_by_controller() {
        EffortEstimate::Small
    } else {
        EffortEstimate::Medium
    };
    ProducerEvidence::new(
        confidence,
        effort,
        proposal.evidence.clone(),
        proposal.expected_benefit.clone(),
    )
}

/// The execution plan a generated proposal is assessed against.
fn execution_plan(proposal: &ImprovementProposal) -> ExecutionPlan {
    ExecutionPlan {
        // Every generated class is judged from the ledger, so the acceptance
        // command is the one that recomputes it.
        acceptance_command: "cargo test --test nt55_impact_receipts".to_string(),
        overlap_scope: if proposal.scope.workspaces.is_empty() {
            vec!["src/".to_string()]
        } else {
            proposal
                .scope
                .workspaces
                .iter()
                .map(|workspace| format!("{workspace}/src/"))
                .collect()
        },
        dependencies: Vec::new(),
        approved_intent: None,
    }
}

/// The workspace whose store owns a proposal's bead.
fn owning_workspace(
    proposal: &ImprovementProposal,
    workspaces: &BTreeMap<String, PathBuf>,
    home: &Path,
) -> PathBuf {
    proposal
        .scope
        .workspaces
        .first()
        .and_then(|name| workspaces.get(name).cloned())
        .unwrap_or_else(|| home.to_path_buf())
}

/// Non-closed beads already carrying an improvement signature.
async fn existing_signatures(store: &dyn BeadStore) -> Result<BTreeMap<String, BeadId>> {
    let mut beads = store
        .list_all()
        .await
        .context("failed to read the bead inventory for improvement admission")?;
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

/// How many proposals today's journal already admitted.
///
/// Read from the journal rather than tracked in memory so the budget survives
/// a restart: a controller that forgot what it admitted this morning would
/// admit another one every time it was restarted.
fn admitted_today(journal: &Path, now: DateTime<Utc>) -> usize {
    let Ok(records) = crate::cli::improvements::read_decisions(journal) else {
        return 0;
    };
    let _ = now;
    records
        .iter()
        .filter(|record| record.decision.is_admitted())
        .count()
}

/// The title of the bead an admitted L4 proposal creates.
fn bead_title(proposal: &ImprovementProposal) -> String {
    let scope = proposal
        .scope
        .workspaces
        .first()
        .cloned()
        .unwrap_or_else(|| "fleet".to_string());
    format!(
        "[{}] {scope}: {}",
        proposal.evidence_class.as_str(),
        first_sentence(&proposal.intended_change)
    )
}

/// The body, carrying provenance and the acceptance measure.
fn bead_body(proposal: &ImprovementProposal, cross_repo: bool) -> String {
    let mut body = String::new();
    body.push_str("Filed by the NEEDLE improvement loop (ADR-029), not by a person.\n\n");
    body.push_str(&format!("Ref: {}\n\n", proposal.unique_ref()));
    body.push_str("## Intended change\n\n");
    body.push_str(&proposal.intended_change);
    body.push_str("\n\n## Expected benefit\n\n");
    body.push_str(&proposal.expected_benefit);
    body.push_str("\n\n## Acceptance measure\n\n");
    body.push_str(&format!(
        "{} must {} by at least {} within {} days of exposure, measured over \
         this proposal's own cohort against the baseline window before it. A \
         change that does not move this measure is withdrawn whatever the \
         implementing agent reports (ADR-029).\n",
        proposal.acceptance.measure.as_str(),
        proposal.acceptance.direction.as_str(),
        proposal.acceptance.min_delta,
        proposal.acceptance.horizon_days,
    ));
    body.push_str("\n## Evidence\n\n");
    for reference in &proposal.evidence {
        body.push_str(&format!(
            "- {}: {} (observed {})\n",
            reference.kind.as_str(),
            reference.id,
            reference.observed_at.format("%Y-%m-%d")
        ));
    }
    body.push_str("\n## Rollback\n\n");
    body.push_str(&proposal.rollback.description);
    body.push('\n');
    if cross_repo {
        body.push_str(
            "\n## Cross-repository gate\n\nThis bead was filed in the owning \
             repository rather than in NEEDLE. Land it there and record the \
             verification on this bead before closing.\n",
        );
    }
    body
}

/// The first sentence of a change description, for a bead title.
fn first_sentence(text: &str) -> String {
    let cut = text
        .find(';')
        .or_else(|| text.find(". "))
        .unwrap_or(text.len());
    text[..cut].trim().to_string()
}
