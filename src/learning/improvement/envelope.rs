//! The typed proposal envelope and the immutable producer evidence contract
//! (N-T07, `needle-2b0a309b`; ADR-029 step 2).
//!
//! An [`ImprovementProposal`] is the only shape in which the improvement loop
//! may ask for a change. Everything downstream — admission, delivery, the
//! impact receipt — addresses the proposal by its [`signature`], which is
//! derived from the evidence rather than supplied, so two generators that saw
//! the same facts cannot file two different proposals about them.
//!
//! ## What "immutable producer evidence" means here
//!
//! A producer states *what it saw* and *what it wants changed*. It does not
//! get to state how much that is worth:
//!
//! - There is no weight, priority, score or rank field on this type. Those are
//!   operator-owned and live in the workspace impact profile
//!   ([`super::impact`]), which a proposal can be scored against but never
//!   writes to.
//! - [`ImprovementProposal::new`] is the only constructor, it validates, and
//!   it *derives* the signature. A producer cannot hand-pick a signature to
//!   dodge deduplication or to collide with another proposal's receipts.
//! - [`AcceptanceMeasure`] admits no free-text variant. Every measure is a
//!   named quantity computable from `attempt.resolved` ledger rows, which is
//!   what makes "a change that does not move its acceptance measure is
//!   withdrawn" (ADR-029) mechanically decidable rather than a matter of the
//!   author's report.
//!
//! The envelope is a pure record: nothing here reads a file, a store, a clock
//! or the network. Effects belong to the controllers (plan section 4.10).

use std::collections::BTreeSet;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema version of the serialized proposal record.
///
/// Bump this — and the fixture it is checked against — whenever a field
/// changes meaning or availability, exactly as the `attempt.resolved` ledger
/// row does.
pub const IMPROVEMENT_PROPOSAL_SCHEMA_VERSION: u32 = 1;

/// bead-rs ref namespace carrying a proposal's signature onto the bead it
/// produced, so an admitted proposal and its implementation bead can always
/// be joined back together.
pub const PROPOSAL_REF_NAMESPACE: &str = "needle-proposal";

/// Maximum bytes retained for one free-text field on a proposal.
///
/// Prose is for the operator reading a receipt, not a channel for smuggling a
/// payload into a bead body; every text field is bounded at construction.
pub const PROPOSAL_FIELD_BYTES: usize = 600;

/// Autonomy level a proposal requires (plan section 5.7).
///
/// The level is a property of the *change*, not a lever the producer can pull:
/// [`ImprovementProposal::new`] refuses a proposal whose evidence class cannot
/// justify the level it claims, and ADR-027 forbids any automatic change from
/// widening authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityLevel {
    /// Retrieve reviewed memory and report evaluation.
    L0,
    /// Re-rank memory, routing and EvalCase selection.
    L1,
    /// Canary an already approved prompt/policy variant.
    L2,
    /// Tune cadence, retry or numeric parameters inside approved bounds.
    L3,
    /// Propose a new policy, gate, OutcomeContract or source change.
    L4,
    /// Apply/deploy new policy or code, widen authority, weaken a safety
    /// bound. Refused by admission until Gate D (plan section 9).
    L5,
}

impl AuthorityLevel {
    /// The wire string, shared by the record, telemetry and the CLI so a
    /// reader joining them never has to translate.
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthorityLevel::L0 => "L0",
            AuthorityLevel::L1 => "L1",
            AuthorityLevel::L2 => "L2",
            AuthorityLevel::L3 => "L3",
            AuthorityLevel::L4 => "L4",
            AuthorityLevel::L5 => "L5",
        }
    }

    /// Whether a controller may apply this level's change itself.
    ///
    /// L1–L3 are applied by the controller that owns the envelope; L4 becomes
    /// an ordinary implementation bead worked by the fleet; L5 is refused
    /// (plan section 4.10 step 3).
    pub fn applied_by_controller(&self) -> bool {
        matches!(
            self,
            AuthorityLevel::L1 | AuthorityLevel::L2 | AuthorityLevel::L3
        )
    }
}

impl fmt::Display for AuthorityLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The recognised evidence classes (plan section 4.10 step 2).
///
/// A class is not a topic label: it names the *shape of the ledger query* that
/// produced the proposal, which is what lets admission tell an already-owned
/// evidence class from a genuinely new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    /// One bead failing the same way three or more times in a row.
    RepeatedIdenticalFailures,
    /// A workspace whose attempts verify materially worse on the adapter it
    /// uses than on one it could use.
    WorkspaceAdapterRegret,
    /// Spend concentrated on attempts that never verified.
    UnverifiedSpendConcentration,
    /// A workspace whose baseline is red, or whose gates cannot run, so no
    /// attempt in it can earn a verified closure.
    RedBaselineWorkspace,
    /// A failure fingerprint that recurs although a candidate lesson (N-T50)
    /// already records what fixed it.
    RecurringFingerprintWithKnownFix,
    /// A prompt canary that reached its decision threshold.
    CanaryResult,
}

impl EvidenceClass {
    /// The wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            EvidenceClass::RepeatedIdenticalFailures => "repeated_identical_failures",
            EvidenceClass::WorkspaceAdapterRegret => "workspace_adapter_regret",
            EvidenceClass::UnverifiedSpendConcentration => "unverified_spend_concentration",
            EvidenceClass::RedBaselineWorkspace => "red_baseline_workspace",
            EvidenceClass::RecurringFingerprintWithKnownFix => {
                "recurring_fingerprint_with_known_fix"
            }
            EvidenceClass::CanaryResult => "canary_result",
        }
    }

    /// Every class, for exhaustive iteration and vocabulary tests.
    pub const ALL: [EvidenceClass; 6] = [
        EvidenceClass::RepeatedIdenticalFailures,
        EvidenceClass::WorkspaceAdapterRegret,
        EvidenceClass::UnverifiedSpendConcentration,
        EvidenceClass::RedBaselineWorkspace,
        EvidenceClass::RecurringFingerprintWithKnownFix,
        EvidenceClass::CanaryResult,
    ];

    /// The highest authority level this class of evidence can justify.
    ///
    /// This is the structural half of "an experiment cannot promote itself"
    /// (plan section 5.7): a canary result can move a routing weight, but no
    /// amount of canary evidence can authorize deploying new code. A producer
    /// claiming more than its evidence class allows is refused at
    /// construction, not at admission, so the refusal cannot be skipped by
    /// calling a different controller.
    pub fn max_authority(&self) -> AuthorityLevel {
        match self {
            // Routing and canary evidence moves the knobs their own
            // controllers own.
            EvidenceClass::WorkspaceAdapterRegret => AuthorityLevel::L1,
            EvidenceClass::CanaryResult => AuthorityLevel::L2,
            // Spend concentration tunes numeric bounds already approved.
            EvidenceClass::UnverifiedSpendConcentration => AuthorityLevel::L3,
            // The rest describe work that needs a change someone implements.
            EvidenceClass::RepeatedIdenticalFailures
            | EvidenceClass::RedBaselineWorkspace
            | EvidenceClass::RecurringFingerprintWithKnownFix => AuthorityLevel::L4,
        }
    }
}

impl fmt::Display for EvidenceClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What kind of thing an evidence reference points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// An `attempt.resolved` row's `attempt_id`.
    Attempt,
    /// A failure fingerprint.
    Fingerprint,
    /// A workspace directory name.
    Workspace,
    /// An agent adapter name.
    Adapter,
    /// A bead id.
    Bead,
    /// A prompt canary experiment id.
    Canary,
}

impl EvidenceKind {
    /// The wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            EvidenceKind::Attempt => "attempt",
            EvidenceKind::Fingerprint => "fingerprint",
            EvidenceKind::Workspace => "workspace",
            EvidenceKind::Adapter => "adapter",
            EvidenceKind::Bead => "bead",
            EvidenceKind::Canary => "canary",
        }
    }
}

/// One immutable pointer into the evidence that produced a proposal.
///
/// It is a *reference*, never a copy: the ledger row stays the authority, and
/// a receipt re-reads it at the horizon rather than trusting a snapshot the
/// producer embedded. `observed_at` is the evidence's own timestamp, not the
/// generation time, so a replay can bound the window it must re-read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// What kind of thing `id` names.
    pub kind: EvidenceKind,
    /// The identifier, verbatim from the ledger.
    pub id: String,
    /// When the referenced evidence was observed.
    pub observed_at: DateTime<Utc>,
}

impl EvidenceRef {
    /// A reference to one piece of evidence.
    pub fn new(kind: EvidenceKind, id: impl Into<String>, observed_at: DateTime<Utc>) -> Self {
        EvidenceRef {
            kind,
            id: id.into(),
            observed_at,
        }
    }

    /// The stable form hashed into a proposal signature.
    fn signature_term(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.id)
    }
}

/// A quantity the loop can compute from the ledger, and therefore decide on.
///
/// Every variant is a section 10 measure. There is deliberately no
/// `Other(String)`: a proposal whose acceptance measure cannot be computed
/// from the ledger is refused at construction (plan section 4.10 step 2), and
/// the cheapest way to guarantee that is to make an uncomputable measure
/// unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactMeasure {
    /// Verified closures per judged attempt.
    VerifiedYieldPerAttempt,
    /// Verified closures per dollar, over costed rows only (ADR-030).
    VerifiedYieldPerDollar,
    /// Recurrence rate of the target failure fingerprint.
    FingerprintRecurrence,
    /// False-close / reopen rate.
    FalseCloseRate,
    /// Share of attempts resolving as `infrastructure_failure`.
    InfrastructureShare,
}

impl ImpactMeasure {
    /// The wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            ImpactMeasure::VerifiedYieldPerAttempt => "verified_yield_per_attempt",
            ImpactMeasure::VerifiedYieldPerDollar => "verified_yield_per_dollar",
            ImpactMeasure::FingerprintRecurrence => "fingerprint_recurrence",
            ImpactMeasure::FalseCloseRate => "false_close_rate",
            ImpactMeasure::InfrastructureShare => "infrastructure_share",
        }
    }

    /// Every measure, for exhaustive iteration.
    pub const ALL: [ImpactMeasure; 5] = [
        ImpactMeasure::VerifiedYieldPerAttempt,
        ImpactMeasure::VerifiedYieldPerDollar,
        ImpactMeasure::FingerprintRecurrence,
        ImpactMeasure::FalseCloseRate,
        ImpactMeasure::InfrastructureShare,
    ];
}

impl fmt::Display for ImpactMeasure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which way a measure has to move for the change to have worked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Higher is better (yield).
    Increase,
    /// Lower is better (recurrence, false closes, infrastructure share).
    Decrease,
}

impl Direction {
    /// The wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Increase => "increase",
            Direction::Decrease => "decrease",
        }
    }

    /// Whether `observed` beats `baseline` by at least `min_delta` in this
    /// direction.
    pub fn satisfied(&self, baseline: f64, observed: f64, min_delta: f64) -> bool {
        match self {
            Direction::Increase => observed - baseline >= min_delta,
            Direction::Decrease => baseline - observed >= min_delta,
        }
    }
}

/// The measure, threshold and horizon that will decide promote or withdraw.
///
/// This is the proposal's own falsification condition, fixed before the change
/// is made. A receipt (N-T55) recomputes exactly this and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceMeasure {
    /// The section 10 quantity to compare.
    pub measure: ImpactMeasure,
    /// Which way it has to move.
    pub direction: Direction,
    /// How far it has to move, in the measure's own units, to count.
    pub min_delta: f64,
    /// How long after exposure the measure is read.
    pub horizon_days: u32,
}

/// How the change is undone if its receipt withdraws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollback {
    /// What undoing it consists of, for the operator reading the receipt.
    pub description: String,
    /// Whether the owning controller can perform the rollback itself.
    ///
    /// L1–L3 changes flip a value their controller owns and revert
    /// automatically; an L4 source change reverts through the normal delivery
    /// path, which is a revert proposal, not a flag flip.
    pub automatic: bool,
}

/// Where a proposal's change would land.
///
/// The cohort a receipt later measures is derived from this, so it is part of
/// the signature: the same evidence class about two different workspaces is
/// two proposals, not one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalScope {
    /// Workspace names the change would affect, sorted and deduplicated.
    #[serde(default)]
    pub workspaces: Vec<String>,
    /// Adapter names the change would affect, sorted and deduplicated.
    #[serde(default)]
    pub adapters: Vec<String>,
}

impl ProposalScope {
    /// A scope over the given workspaces and adapters, normalized.
    pub fn new(
        workspaces: impl IntoIterator<Item = String>,
        adapters: impl IntoIterator<Item = String>,
    ) -> Self {
        let workspaces: BTreeSet<String> = workspaces.into_iter().collect();
        let adapters: BTreeSet<String> = adapters.into_iter().collect();
        ProposalScope {
            workspaces: workspaces.into_iter().collect(),
            adapters: adapters.into_iter().collect(),
        }
    }

    /// Whether the scope names nothing at all.
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty() && self.adapters.is_empty()
    }
}

/// Why a proposal could not be constructed.
///
/// Construction failures are part of the contract: the generator is expected
/// to produce some of these, and the count of each is what tells an operator
/// that a class of evidence is being seen but cannot be acted on.
#[derive(Debug, Clone, PartialEq)]
pub enum ProposalRejection {
    /// No evidence reference was supplied.
    NoEvidence,
    /// The intended change was empty.
    NoIntendedChange,
    /// The scope named neither a workspace nor an adapter.
    EmptyScope,
    /// `min_delta` was not a finite, strictly positive number, so the measure
    /// could never decide anything.
    UnusableThreshold {
        /// The threshold as supplied.
        min_delta: f64,
    },
    /// `horizon_days` was zero: a measure read immediately after exposure
    /// reads the baseline again.
    ZeroHorizon,
    /// The proposal claimed more authority than its evidence class allows.
    AuthorityExceedsEvidence {
        /// What was claimed.
        claimed: AuthorityLevel,
        /// The most the class can justify.
        allowed: AuthorityLevel,
    },
}

impl fmt::Display for ProposalRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProposalRejection::NoEvidence => {
                write!(f, "proposal names no evidence")
            }
            ProposalRejection::NoIntendedChange => {
                write!(f, "proposal states no intended change")
            }
            ProposalRejection::EmptyScope => {
                write!(f, "proposal scope names neither a workspace nor an adapter")
            }
            ProposalRejection::UnusableThreshold { min_delta } => write!(
                f,
                "acceptance threshold {min_delta} is not a positive finite delta, \
                 so the measure could never decide promote or withdraw"
            ),
            ProposalRejection::ZeroHorizon => write!(
                f,
                "acceptance horizon is zero days, so the measure would re-read the baseline"
            ),
            ProposalRejection::AuthorityExceedsEvidence { claimed, allowed } => write!(
                f,
                "evidence class justifies at most {allowed}, proposal claims {claimed}"
            ),
        }
    }
}

/// A proposed improvement, addressed by the evidence that produced it.
///
/// Construct with [`ImprovementProposal::new`]; the fields are public to read
/// and there is no mutation API, because a proposal that changed after its
/// signature was derived would no longer be the thing its receipt measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImprovementProposal {
    /// Version of this record's schema.
    pub schema_version: u32,
    /// Deterministic identity, derived from the evidence class, the evidence
    /// references and the scope. Not supplied by the producer.
    pub signature: String,
    /// Which ledger query produced this.
    pub evidence_class: EvidenceClass,
    /// The evidence itself, sorted and deduplicated.
    pub evidence: Vec<EvidenceRef>,
    /// Where the change would land.
    pub scope: ProposalScope,
    /// The exact change being asked for.
    pub intended_change: String,
    /// The autonomy level the change requires (plan section 5.7).
    pub authority: AuthorityLevel,
    /// What the change is expected to do, in section 10 terms.
    pub expected_benefit: String,
    /// The falsification condition.
    pub acceptance: AcceptanceMeasure,
    /// How it gets undone.
    pub rollback: Rollback,
    /// When the generator emitted this record.
    pub generated_at: DateTime<Utc>,
    /// Which generator revision emitted it, so a behaviour change in the
    /// generator is visible in the record it produced.
    pub generator_version: u32,
}

impl ImprovementProposal {
    /// Validate and build a proposal, deriving its signature.
    ///
    /// Refuses, rather than repairing, anything that would make the proposal
    /// undecidable later: no evidence, no change, an empty scope, a threshold
    /// or horizon that cannot decide, or an authority claim the evidence class
    /// does not support.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        evidence_class: EvidenceClass,
        evidence: Vec<EvidenceRef>,
        scope: ProposalScope,
        intended_change: impl Into<String>,
        authority: AuthorityLevel,
        expected_benefit: impl Into<String>,
        acceptance: AcceptanceMeasure,
        rollback: Rollback,
        generated_at: DateTime<Utc>,
        generator_version: u32,
    ) -> Result<Self, ProposalRejection> {
        if evidence.is_empty() {
            return Err(ProposalRejection::NoEvidence);
        }
        if scope.is_empty() {
            return Err(ProposalRejection::EmptyScope);
        }
        if !acceptance.min_delta.is_finite() || acceptance.min_delta <= 0.0 {
            return Err(ProposalRejection::UnusableThreshold {
                min_delta: acceptance.min_delta,
            });
        }
        if acceptance.horizon_days == 0 {
            return Err(ProposalRejection::ZeroHorizon);
        }
        let allowed = evidence_class.max_authority();
        if authority > allowed {
            return Err(ProposalRejection::AuthorityExceedsEvidence {
                claimed: authority,
                allowed,
            });
        }

        let intended_change = bound(intended_change.into());
        if intended_change.trim().is_empty() {
            return Err(ProposalRejection::NoIntendedChange);
        }

        // Sorted and deduplicated so two generators that saw the same rows in
        // a different order derive the same signature.
        let evidence: Vec<EvidenceRef> = evidence
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let signature = derive_signature(evidence_class, &evidence, &scope);

        Ok(ImprovementProposal {
            schema_version: IMPROVEMENT_PROPOSAL_SCHEMA_VERSION,
            signature,
            evidence_class,
            evidence,
            scope,
            intended_change,
            authority,
            expected_benefit: bound(expected_benefit.into()),
            acceptance,
            rollback,
            generated_at,
            generator_version,
        })
    }

    /// The bead-rs unique ref that carries this proposal onto its bead.
    pub fn unique_ref(&self) -> String {
        format!("{PROPOSAL_REF_NAMESPACE}:{}", self.signature)
    }

    /// Evidence ids of one kind, in signature order.
    pub fn evidence_ids(&self, kind: EvidenceKind) -> Vec<&str> {
        self.evidence
            .iter()
            .filter(|reference| reference.kind == kind)
            .map(|reference| reference.id.as_str())
            .collect()
    }
}

/// Derive a proposal's identity from what it saw and where it would act.
///
/// Deliberately excludes the intended change, the prose and the timestamps:
/// re-running the generator over an extended window must produce the *same*
/// signature for the same evidence, or deduplication would file a second bead
/// every time the wording moved.
fn derive_signature(
    evidence_class: EvidenceClass,
    evidence: &[EvidenceRef],
    scope: &ProposalScope,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(evidence_class.as_str().as_bytes());
    hasher.update([0u8]);
    for reference in evidence {
        hasher.update(reference.signature_term().as_bytes());
        hasher.update([0u8]);
    }
    hasher.update(b"scope");
    for workspace in &scope.workspaces {
        hasher.update(workspace.as_bytes());
        hasher.update([0u8]);
    }
    for adapter in &scope.adapters {
        hasher.update(adapter.as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    // 16 hex chars is the same width the audit signature uses; it is a
    // deduplication key, not a cryptographic commitment.
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Truncate a free-text field on a character boundary.
fn bound(mut text: String) -> String {
    if text.len() <= PROPOSAL_FIELD_BYTES {
        return text;
    }
    let mut end = PROPOSAL_FIELD_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}
