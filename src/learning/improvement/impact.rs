//! The versioned, operator-owned impact contract (N-T07, `needle-9c5ee565`).
//!
//! Admission has to rank proposals, and ranking needs a statement of what the
//! work is worth. The whole difficulty is that the thing being ranked is also
//! the thing that would most like a high rank: a generator that could state
//! its own importance would learn to state it high, and the loop would
//! optimize for self-reported value instead of measured impact.
//!
//! So the contract is split by *who may write each half*:
//!
//! - [`WorkspaceImpactProfile`] is **operator-owned**. It carries the weights
//!   — objective, affected systems, severity, time sensitivity, strategic fit,
//!   public visibility — and it is the only place those can be set. It is
//!   versioned and carries a review expiry, because a weight nobody has looked
//!   at for a year is not an operator's judgement any more.
//! - [`ProducerEvidence`] is what a generator or a target workspace may supply:
//!   its confidence, its effort estimate, and references to the evidence. All
//!   of it bounded, none of it able to reach a profile field.
//!
//! [`ImpactContract::assemble`] joins them and is the only constructor. There
//! is no code path by which producer input edits a profile band, which is why
//! "generators may supply evidence but must not be able to raise their own
//! operator-owned weights" is a property of the type rather than a rule
//! somebody has to remember.
//!
//! Two smaller rules the acceptance criteria call out:
//!
//! - **Unprofiled work is not starved.** A workspace with no profile gets
//!   [`WorkspaceImpactProfile::neutral`] — mid-band everywhere — rather than
//!   zero, so an unprofiled workspace ranks below a deliberately-important one
//!   and above a deliberately-unimportant one instead of never being worked.
//! - **An expired profile degrades to neutral**, never to its stale bands.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::envelope::EvidenceRef;

/// Schema version of the impact contract.
pub const IMPACT_CONTRACT_SCHEMA_VERSION: u32 = 1;

/// Maximum bytes retained for one free-text field.
const IMPACT_FIELD_BYTES: usize = 300;

/// A bounded 0–4 band.
///
/// Every operator-owned weight is one of these. The bound is the point: an
/// unbounded number is a lever, and a lever next to a producer eventually gets
/// pulled. Construction clamps rather than failing, so a mis-typed profile
/// value degrades to the nearest legal band instead of refusing to load and
/// taking the workspace's whole queue with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Band(u8);

impl Band {
    /// The lowest band.
    pub const MIN: Band = Band(0);
    /// The neutral mid band, used wherever a value is unknown.
    pub const NEUTRAL: Band = Band(2);
    /// The highest band.
    pub const MAX: Band = Band(4);

    /// A band, clamped into range.
    pub fn new(value: u8) -> Self {
        Band(value.min(4))
    }

    /// The raw 0–4 value.
    pub fn get(self) -> u8 {
        self.0
    }

    /// The band as a 0.0–1.0 fraction, for scoring.
    pub fn fraction(self) -> f64 {
        f64::from(self.0) / 4.0
    }
}

impl Default for Band {
    fn default() -> Self {
        Band::NEUTRAL
    }
}

/// What a workspace's work is for, and how much it matters — operator-owned.
///
/// Nothing a generator emits can construct or modify one of these: they are
/// read from operator configuration. A proposal is *scored against* a profile;
/// it never supplies one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceImpactProfile {
    /// Version of this record's schema.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// The workspace this profile governs.
    pub workspace: String,
    /// What the application is for, in the operator's words.
    #[serde(default)]
    pub objective: String,
    /// Users or systems affected when this workspace's work lands or breaks.
    #[serde(default)]
    pub affected: Vec<String>,
    /// How bad it is when this workspace is broken.
    #[serde(default)]
    pub severity: Band,
    /// How much sooner it matters than later.
    #[serde(default)]
    pub time_sensitivity: Band,
    /// How well the work fits the operator's current priorities.
    #[serde(default)]
    pub strategic_fit: Band,
    /// Whether the work is publicly visible.
    ///
    /// A tie-break only: [`super::scoring`] may use it to order two otherwise
    /// equal proposals and may never use it to outrank a higher-value one.
    #[serde(default)]
    pub public_visibility: bool,
    /// When the operator's judgement here stops counting.
    ///
    /// `None` means it does not expire. A profile past this date degrades to
    /// [`WorkspaceImpactProfile::neutral`] rather than continuing to assert
    /// bands nobody has reviewed.
    #[serde(default)]
    pub review_expiry: Option<DateTime<Utc>>,
}

fn default_schema_version() -> u32 {
    IMPACT_CONTRACT_SCHEMA_VERSION
}

impl WorkspaceImpactProfile {
    /// The bounded neutral default for unprofiled work.
    ///
    /// Mid-band everywhere, not zero: an unprofiled workspace must rank below
    /// one the operator marked important and above one they marked
    /// unimportant. Scoring it at zero would starve every workspace nobody has
    /// got round to profiling, which is most of them.
    pub fn neutral(workspace: impl Into<String>) -> Self {
        WorkspaceImpactProfile {
            schema_version: IMPACT_CONTRACT_SCHEMA_VERSION,
            workspace: workspace.into(),
            objective: String::new(),
            affected: Vec::new(),
            severity: Band::NEUTRAL,
            time_sensitivity: Band::NEUTRAL,
            strategic_fit: Band::NEUTRAL,
            public_visibility: false,
            review_expiry: None,
        }
    }

    /// Whether the operator's review has lapsed as of `now`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.review_expiry.is_some_and(|expiry| now > expiry)
    }

    /// This profile if it is still in review, otherwise the neutral default.
    ///
    /// Callers rank against this rather than against the raw profile, so an
    /// expired profile can only ever move a proposal *towards* neutral. A
    /// lapsed high band must not keep promoting work, and a lapsed low band
    /// must not keep suppressing it.
    pub fn effective(&self, now: DateTime<Utc>) -> Self {
        if self.is_expired(now) {
            let mut neutral = WorkspaceImpactProfile::neutral(self.workspace.clone());
            // The objective is descriptive, not a weight, so it survives
            // expiry — it is what an operator reads to re-profile.
            neutral.objective = self.objective.clone();
            neutral
        } else {
            self.clone()
        }
    }
}

/// How confident a producer is in its own proposal.
///
/// Bounded on purpose, and deliberately coarse: this is the one number a
/// producer supplies that affects rank, so the distance between the best and
/// worst thing it can claim is one band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// A single observation, or a class the loop has not calibrated yet.
    Low,
    /// Repeated observations consistent with each other.
    Medium,
    /// Repeated observations plus a known fix for the same fingerprint.
    High,
}

impl Confidence {
    /// The wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::Low => "low",
            Confidence::Medium => "medium",
            Confidence::High => "high",
        }
    }

    /// The 0.0–1.0 weight used in scoring.
    pub fn fraction(&self) -> f64 {
        match self {
            Confidence::Low => 0.25,
            Confidence::Medium => 0.6,
            Confidence::High => 1.0,
        }
    }
}

/// Roughly how much work the change is.
///
/// Effort divides the value in scoring, so this is the one producer-supplied
/// field where understating helps. It is bounded and coarse for that reason,
/// and admission separately requires a machine-checkable acceptance command,
/// which is what actually stops a "small" label being attached to unbounded
/// work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortEstimate {
    /// A configuration or single-call change.
    Small,
    /// A focused change with its own test.
    Medium,
    /// A change spanning modules, or one needing new contracts.
    Large,
}

impl EffortEstimate {
    /// The wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            EffortEstimate::Small => "small",
            EffortEstimate::Medium => "medium",
            EffortEstimate::Large => "large",
        }
    }

    /// The divisor applied to value in scoring.
    pub fn divisor(&self) -> f64 {
        match self {
            EffortEstimate::Small => 1.0,
            EffortEstimate::Medium => 2.0,
            EffortEstimate::Large => 4.0,
        }
    }
}

/// Everything a producer is allowed to state about its own proposal.
///
/// Note what is absent: no severity, no strategic fit, no visibility, no
/// weight of any kind. A producer describes its evidence and its own
/// uncertainty; the operator's profile says what that is worth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProducerEvidence {
    /// How sure the producer is.
    pub confidence: Confidence,
    /// How big the producer thinks the change is.
    pub effort: EffortEstimate,
    /// Immutable references into the evidence, carried from the proposal.
    pub evidence: Vec<EvidenceRef>,
    /// What the producer observed, for the operator reading a receipt.
    pub summary: String,
}

impl ProducerEvidence {
    /// Producer-supplied evidence, with its text bounded.
    pub fn new(
        confidence: Confidence,
        effort: EffortEstimate,
        evidence: Vec<EvidenceRef>,
        summary: impl Into<String>,
    ) -> Self {
        ProducerEvidence {
            confidence,
            effort,
            evidence,
            summary: bound(summary.into()),
        }
    }
}

/// The assembled contract admission ranks against.
///
/// Built only by [`ImpactContract::assemble`], from an operator profile plus
/// producer evidence, with the profile's bands copied across verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpactContract {
    /// Version of this record's schema.
    pub schema_version: u32,
    /// The profile in force, after expiry was applied.
    pub profile: WorkspaceImpactProfile,
    /// What the producer supplied.
    pub producer: ProducerEvidence,
    /// Whether the profile had lapsed when this contract was assembled.
    ///
    /// Reported so a receipt can say *why* a proposal ranked the way it did
    /// rather than leaving an operator to rediscover the expiry.
    pub profile_expired: bool,
    /// Whether a profile existed at all.
    pub profile_defaulted: bool,
}

impl ImpactContract {
    /// Join an operator profile with producer evidence.
    ///
    /// `profile` of `None` is unprofiled work and takes the neutral default.
    /// An expired profile is reduced to neutral before anything reads a band,
    /// so neither absence nor staleness can carry a weight.
    pub fn assemble(
        workspace: &str,
        profile: Option<&WorkspaceImpactProfile>,
        producer: ProducerEvidence,
        now: DateTime<Utc>,
    ) -> Self {
        let (effective, expired, defaulted) = match profile {
            Some(profile) => (profile.effective(now), profile.is_expired(now), false),
            None => (WorkspaceImpactProfile::neutral(workspace), false, true),
        };

        ImpactContract {
            schema_version: IMPACT_CONTRACT_SCHEMA_VERSION,
            profile: effective,
            producer,
            profile_expired: expired,
            profile_defaulted: defaulted,
        }
    }

    /// The operator-owned value of the work, 0.0–1.0.
    ///
    /// Severity, time sensitivity and strategic fit, equally weighted. Public
    /// visibility is deliberately not in here — it is a tie-break
    /// ([`super::scoring`]), never a component of value.
    pub fn application_value(&self) -> f64 {
        let severity = self.profile.severity.fraction();
        let urgency = self.profile.time_sensitivity.fraction();
        let fit = self.profile.strategic_fit.fraction();
        (severity + urgency + fit) / 3.0
    }
}

/// Truncate a free-text field on a character boundary.
fn bound(mut text: String) -> String {
    if text.len() <= IMPACT_FIELD_BYTES {
        return text;
    }
    let mut end = IMPACT_FIELD_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}
