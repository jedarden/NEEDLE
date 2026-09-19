//! Deterministic expected-application-value ordering (N-T07, `needle-29f396d3`).
//!
//! Admission ranks proposals by expected application value: what the operator
//! says the work is worth, discounted by how sure the producer is, divided by
//! how much work it is. The ranking is a pure function of the contract and the
//! policy, so two runs over the same inputs order the queue identically and a
//! receipt can replay why.
//!
//! Three properties are load-bearing:
//!
//! - **A producer cannot self-promote.** Every term a producer supplies is
//!   bounded and can only *reduce* the operator-owned value: confidence is a
//!   multiplier in `(0, 1]` and effort is a divisor `>= 1`. There is no
//!   producer-supplied term that multiplies above one, so the ceiling on any
//!   proposal's score is the operator's own value for that workspace.
//! - **Stale evidence cannot raise rank.** Evidence older than the policy's
//!   window caps confidence at [`Confidence::Low`]; it never restores it.
//! - **Public visibility is a tie-break only.** It enters
//!   [`ScoredProposal::ordering_key`] after the score, so it can separate two
//!   equal proposals and can never outrank a higher-value one.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::envelope::ImprovementProposal;
use super::impact::{Confidence, ImpactContract};

/// Version of the scoring policy.
///
/// Recorded on every score. A ranking is only replayable if the reader knows
/// which policy produced it, so changing any weight below means bumping this.
pub const SCORING_POLICY_VERSION: u32 = 1;

/// The tunables of the ranking, all operator-owned.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScoringPolicy {
    /// Version stamped onto every score this policy produces.
    pub version: u32,
    /// Evidence older than this contributes nothing and caps confidence.
    pub evidence_max_age_days: i64,
}

impl Default for ScoringPolicy {
    fn default() -> Self {
        ScoringPolicy {
            version: SCORING_POLICY_VERSION,
            evidence_max_age_days: 30,
        }
    }
}

/// The components of one score, kept so the ranking is explainable.
///
/// A number with no decomposition is not an explanation, and an operator
/// reading a receipt needs to be able to say "it ranked there because the
/// workspace is high-severity and the producer was unsure", not just "0.31".
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScoreComponents {
    /// Operator-owned value: severity, time sensitivity, strategic fit.
    pub application_value: f64,
    /// Producer confidence as a multiplier in `(0, 1]`.
    pub confidence: f64,
    /// Producer effort as a divisor `>= 1`.
    pub effort_divisor: f64,
    /// Whether every piece of evidence was older than the policy window.
    pub evidence_stale: bool,
    /// Whether the workspace profile had lapsed.
    pub profile_expired: bool,
    /// Whether the workspace had no profile at all.
    pub profile_defaulted: bool,
}

/// A proposal with its computed rank.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredProposal {
    /// The proposal's signature, so a score can be joined to its record.
    pub signature: String,
    /// Expected application value. Higher ranks first.
    pub score: f64,
    /// How the score was reached.
    pub components: ScoreComponents,
    /// Which policy version produced it.
    pub policy_version: u32,
    /// Tie-break only (plan: never a substitute for application value).
    pub public_visibility: bool,
}

impl ScoredProposal {
    /// The total ordering key: score first, then visibility, then signature.
    ///
    /// The signature terminates the ordering so that two proposals that are
    /// equal on every meaningful axis still sort deterministically, rather
    /// than in whatever order the map iterated.
    pub fn ordering_key(&self) -> (std::cmp::Reverse<OrderedScore>, bool, &str) {
        (
            std::cmp::Reverse(OrderedScore(self.score)),
            !self.public_visibility,
            self.signature.as_str(),
        )
    }
}

/// A total order over scores.
///
/// Scores are finite by construction — every term is bounded and the divisor
/// is at least one — so a NaN here would be a bug in this module rather than
/// bad input. It is mapped to the bottom of the order instead of panicking,
/// because a ranking that aborts takes the whole admission run with it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderedScore(pub f64);

impl Eq for OrderedScore {}

impl PartialOrd for OrderedScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .partial_cmp(&other.0)
            .unwrap_or(std::cmp::Ordering::Less)
    }
}

/// Score one proposal against its assembled impact contract.
///
/// Pure: the only clock input is `now`, passed in so a fixture can pin it.
pub fn score(
    proposal: &ImprovementProposal,
    contract: &ImpactContract,
    policy: &ScoringPolicy,
    now: DateTime<Utc>,
) -> ScoredProposal {
    let cutoff = now - Duration::days(policy.evidence_max_age_days);
    // "Every piece of evidence is stale" rather than "any": one fresh
    // observation is enough to say the condition still holds. An empty
    // evidence list cannot occur — the envelope refuses it at construction —
    // but it is treated as stale rather than fresh if it ever did.
    let evidence_stale = proposal
        .evidence
        .iter()
        .all(|reference| reference.observed_at < cutoff);

    // Staleness can only lower confidence. Capping rather than overwriting
    // means a Low-confidence proposal with stale evidence stays Low instead
    // of being quietly raised to the cap.
    let effective_confidence = if evidence_stale {
        contract.producer.confidence.min(Confidence::Low)
    } else {
        contract.producer.confidence
    };

    let application_value = contract.application_value();
    let confidence = effective_confidence.fraction();
    let effort_divisor = contract.producer.effort.divisor();

    // Producer terms are a multiplier in (0, 1] and a divisor >= 1, so the
    // operator's application value is a ceiling the producer can approach but
    // never exceed.
    let score = application_value * confidence / effort_divisor;

    ScoredProposal {
        signature: proposal.signature.clone(),
        score,
        components: ScoreComponents {
            application_value,
            confidence,
            effort_divisor,
            evidence_stale,
            profile_expired: contract.profile_expired,
            profile_defaulted: contract.profile_defaulted,
        },
        policy_version: policy.version,
        public_visibility: contract.profile.public_visibility,
    }
}

/// Rank scored proposals, highest expected value first.
///
/// Stable and total: equal scores fall to the visibility tie-break, and equal
/// visibility falls to the signature.
pub fn rank(mut scored: Vec<ScoredProposal>) -> Vec<ScoredProposal> {
    scored.sort_by(|left, right| left.ordering_key().cmp(&right.ordering_key()));
    scored
}
