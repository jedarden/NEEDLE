//! The admission controller: explicit decisions, budgets, deduplication and
//! backpressure (N-T07, `needle-43c0d818` and `needle-f754b4cb`).
//!
//! Admission is where a proposal stops being an observation and starts being
//! work, so every outcome here is a *recorded decision with a reason* — there
//! is no silent drop. An operator reading `needle improvements` has to be able
//! to see that something was seen and refused, and why, exactly as clearly as
//! they can see what was admitted. A proposal that vanished because a budget
//! was full looks identical, from the outside, to a generator that never ran.
//!
//! The four things admission enforces, in the order they are checked:
//!
//! 1. **Authority.** L5 is refused until Gate D (plan section 9). This is
//!    checked first because no amount of budget or novelty makes a
//!    self-deploying change admissible.
//! 2. **Deduplication.** Against existing beads *and* plan leaves. The plan
//!    already owns unchanged retries (R1/R2), operator overrides (N-T20) and
//!    several other classes; a generator that filed its own bead for owned
//!    evidence would recreate the duplicate-work failure ADR-015 documents.
//! 3. **Backpressure.** If admitted work is already piling up unfinished, more
//!    proposals make the pile deeper, not the factory faster.
//! 4. **Budget.** `improvements.admission.per_day`, default 1.
//!
//! Pure: callers pass the world in as [`AdmissionWorld`] and a timestamp.

use serde::{Deserialize, Serialize};

use super::envelope::{AuthorityLevel, ImprovementProposal};
use super::executable::NonExecutable;
use super::scoring::ScoredProposal;

/// Default proposals admitted per day (plan section 4.10 activation order).
pub const DEFAULT_ADMISSION_PER_DAY: usize = 1;

/// Default ceiling on admitted-but-unfinished proposals.
pub const DEFAULT_MAX_OPEN_ADMITTED: usize = 3;

/// The operator-owned admission policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionPolicy {
    /// How many proposals may be admitted per day.
    pub per_day: usize,
    /// How many admitted proposals may be open before backpressure applies.
    pub max_open_admitted: usize,
    /// Whether Gate D has been accepted. Until then L5 is refused.
    pub gate_d_satisfied: bool,
    /// Shadow mode: decide everything, admit nothing.
    ///
    /// The plan's activation order runs N-T53 in shadow first — "proposals
    /// visible but none admitted" — so shadow is the default rather than an
    /// afterthought.
    pub shadow: bool,
}

impl Default for AdmissionPolicy {
    fn default() -> Self {
        AdmissionPolicy {
            per_day: DEFAULT_ADMISSION_PER_DAY,
            max_open_admitted: DEFAULT_MAX_OPEN_ADMITTED,
            gate_d_satisfied: false,
            shadow: true,
        }
    }
}

/// Where an admitted proposal goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "route")]
pub enum AdmissionRoute {
    /// L1–L3: applied by the controller that owns the envelope.
    Controller {
        /// The level, which selects the owning controller.
        authority: AuthorityLevel,
    },
    /// L4: becomes an ordinary implementation bead worked by the fleet.
    ImplementationBead {
        /// The workspace whose store owns the bead.
        workspace: String,
    },
}

/// Why a proposal was refused.
///
/// Every variant names something an operator could act on. "Refused" with no
/// reason would make the loop unauditable, which is the thing ADR-029 is
/// trying to avoid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum RefusalReason {
    /// The evidence class already has an owner: a bead or a plan leaf.
    AlreadyOwned {
        /// The bead id or plan leaf that owns it.
        owner: String,
    },
    /// An earlier proposal in this same run carried the same signature.
    DuplicateInRun,
    /// The daily budget is spent.
    BudgetExhausted {
        /// The budget that was hit.
        per_day: usize,
    },
    /// Too much admitted work is still open.
    Backpressure {
        /// How many admitted proposals are open.
        open: usize,
        /// The ceiling.
        ceiling: usize,
    },
    /// The level is not admissible yet (L5 before Gate D).
    AuthorityNotPermitted {
        /// The level claimed.
        authority: AuthorityLevel,
    },
    /// The proposal could not be turned into finishable work.
    NotExecutable {
        /// The executability failure, rendered.
        detail: String,
    },
    /// Shadow mode: decided, deliberately not admitted.
    ShadowMode,
}

impl RefusalReason {
    /// A short wire tag, for telemetry and the CLI.
    pub fn tag(&self) -> &'static str {
        match self {
            RefusalReason::AlreadyOwned { .. } => "already_owned",
            RefusalReason::DuplicateInRun => "duplicate_in_run",
            RefusalReason::BudgetExhausted { .. } => "budget_exhausted",
            RefusalReason::Backpressure { .. } => "backpressure",
            RefusalReason::AuthorityNotPermitted { .. } => "authority_not_permitted",
            RefusalReason::NotExecutable { .. } => "not_executable",
            RefusalReason::ShadowMode => "shadow_mode",
        }
    }
}

/// What admission decided about one proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum AdmissionDecision {
    /// Admitted, with where it goes.
    Admitted {
        /// Controller or implementation bead.
        route: AdmissionRoute,
    },
    /// Refused, with why.
    Refused {
        /// The reason.
        reason: RefusalReason,
    },
}

impl AdmissionDecision {
    /// Whether this decision admitted the proposal.
    pub fn is_admitted(&self) -> bool {
        matches!(self, AdmissionDecision::Admitted { .. })
    }
}

/// One decision, addressed to the proposal that caused it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionRecord {
    /// The proposal's signature.
    pub signature: String,
    /// What was decided.
    pub decision: AdmissionDecision,
    /// The rank this proposal held when the decision was made, 0-based.
    ///
    /// Recorded because "refused: budget exhausted" is only interpretable
    /// beside the queue position it was refused at.
    pub rank: usize,
}

/// The state of the world admission reads.
///
/// A struct of plain data rather than live lookups, so the whole policy is
/// provable against a fixture with no store, clock or filesystem anywhere near
/// it.
pub struct AdmissionWorld<'a> {
    /// Answers "does a bead or plan leaf already own this evidence class?".
    pub owner_of: &'a dyn Fn(&ImprovementProposal) -> Option<String>,
    /// Answers "is this proposal executable?" — `None` when it is.
    pub non_executable: &'a dyn Fn(&ImprovementProposal) -> Option<NonExecutable>,
    /// Resolves the workspace whose store owns an L4 bead.
    pub owning_workspace: &'a dyn Fn(&ImprovementProposal) -> String,
    /// How many proposals have already been admitted today.
    pub admitted_today: usize,
    /// How many admitted proposals are still open.
    pub open_admitted: usize,
}

/// Decide every proposal, in rank order.
///
/// Returns one record per proposal — never fewer. A caller can therefore
/// always account for every proposal the generator produced, which is what
/// makes the shadow-mode phase of the activation order meaningful: shadow
/// produces a full set of decisions and admits none of them.
pub fn admit(
    ranked: &[ScoredProposal],
    proposals: &dyn Fn(&str) -> Option<ImprovementProposal>,
    policy: &AdmissionPolicy,
    world: &AdmissionWorld<'_>,
) -> Vec<AdmissionRecord> {
    let mut records = Vec::with_capacity(ranked.len());
    let mut admitted_this_run = 0usize;
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for (rank, scored) in ranked.iter().enumerate() {
        let Some(proposal) = proposals(&scored.signature) else {
            continue;
        };

        let decision = decide(&proposal, policy, world, admitted_this_run, &mut seen);
        if decision.is_admitted() {
            admitted_this_run += 1;
        }
        records.push(AdmissionRecord {
            signature: scored.signature.clone(),
            decision,
            rank,
        });
    }

    records
}

/// The decision for one proposal, in the documented check order.
fn decide(
    proposal: &ImprovementProposal,
    policy: &AdmissionPolicy,
    world: &AdmissionWorld<'_>,
    admitted_this_run: usize,
    seen: &mut std::collections::BTreeSet<String>,
) -> AdmissionDecision {
    // 1. Authority. Checked first: no budget or novelty makes L5 admissible.
    if proposal.authority == AuthorityLevel::L5 && !policy.gate_d_satisfied {
        return AdmissionDecision::Refused {
            reason: RefusalReason::AuthorityNotPermitted {
                authority: proposal.authority,
            },
        };
    }

    // 2. Deduplication — first inside this run, then against the estate.
    if !seen.insert(proposal.signature.clone()) {
        return AdmissionDecision::Refused {
            reason: RefusalReason::DuplicateInRun,
        };
    }
    if let Some(owner) = (world.owner_of)(proposal) {
        return AdmissionDecision::Refused {
            reason: RefusalReason::AlreadyOwned { owner },
        };
    }

    // 3. Executability. A proposal that cannot be finished is refused before
    //    it can consume a budget slot a workable one could have used.
    if let Some(problem) = (world.non_executable)(proposal) {
        return AdmissionDecision::Refused {
            reason: RefusalReason::NotExecutable {
                detail: problem.to_string(),
            },
        };
    }

    // 4. Backpressure, then budget.
    if world.open_admitted >= policy.max_open_admitted {
        return AdmissionDecision::Refused {
            reason: RefusalReason::Backpressure {
                open: world.open_admitted,
                ceiling: policy.max_open_admitted,
            },
        };
    }
    if world.admitted_today + admitted_this_run >= policy.per_day {
        return AdmissionDecision::Refused {
            reason: RefusalReason::BudgetExhausted {
                per_day: policy.per_day,
            },
        };
    }

    // 5. Shadow mode is last, so a shadow run still reports exactly which
    //    proposal *would* have been admitted rather than refusing everything
    //    at the door.
    if policy.shadow {
        return AdmissionDecision::Refused {
            reason: RefusalReason::ShadowMode,
        };
    }

    let route = if proposal.authority.applied_by_controller() {
        AdmissionRoute::Controller {
            authority: proposal.authority,
        }
    } else {
        AdmissionRoute::ImplementationBead {
            workspace: (world.owning_workspace)(proposal),
        }
    };
    AdmissionDecision::Admitted { route }
}
