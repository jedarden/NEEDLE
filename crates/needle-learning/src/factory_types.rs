//! Versioned canonical records exchanged with the learning kernel.
//!
//! These types deliberately contain observations and references, not handles
//! to operational systems.  In particular, evidence contains digests and
//! redacted summaries rather than raw transcripts or command output.

use core::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Current wire version for all factory records.
pub const CURRENT_SCHEMA_VERSION: u16 = 1;

/// A schema version stamped on every canonical record.
pub type SchemaVersion = u16;

/// Error returned when a stable identifier is empty or contains framing
/// characters that would make canonical records ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidId {
    value: String,
}

impl fmt::Display for InvalidId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid learning-kernel identifier {:?}",
            self.value
        )
    }
}

impl std::error::Error for InvalidId {}

macro_rules! id_type_runtime {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Construct an identifier after checking its wire-safe shape.
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidId> {
                let value = value.into();
                if value.is_empty() || value.chars().any(|character| character.is_control()) {
                    return Err(InvalidId { value });
                }
                Ok(Self(value))
            }

            /// Construct an identifier from a trusted static value.
            pub fn from_static(value: &'static str) -> Self {
                Self(value.to_owned())
            }

            /// Borrow the identifier's canonical text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = InvalidId;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
    };
}

id_type_runtime!(AttemptId, "Stable identity for one execution attempt.");
id_type_runtime!(BeadId, "Opaque identity of the desired work item.");
id_type_runtime!(
    ContentHash,
    "Content-addressed digest of an external artifact."
);
id_type_runtime!(
    Digest,
    "Opaque digest used for a canonical source or record."
);
id_type_runtime!(EvidenceId, "Stable identity for one evidence artifact.");
id_type_runtime!(
    ResolutionId,
    "Stable identity for one immutable resolution."
);
id_type_runtime!(SourceId, "Stable identity for a read-only source record.");
id_type_runtime!(
    Timestamp,
    "Caller-supplied canonical timestamp, normally RFC 3339."
);

/// Monotonic bead revision observed when an attempt starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub u64);

/// Claim fencing epoch observed when an attempt starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FencingEpoch(pub u64);

/// A policy, tool, or memory digest that was exposed to an attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyIdentity {
    /// Stable source identity for the policy manifest.
    pub source_id: SourceId,
    /// Digest of the exact manifest bytes.
    pub digest: Digest,
    /// Version supplied by the policy owner.
    pub version: String,
}

/// A tool identity captured in a context manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolIdentity {
    /// Adapter/tool name.
    pub name: String,
    /// Version or image identity supplied by the controller.
    pub version: String,
    /// Optional digest of the executable or adapter definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
}

/// A source-backed memory item exposed to the attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExposure {
    /// Stable source identity.
    pub source_id: SourceId,
    /// Exact source digest.
    pub digest: Digest,
    /// Explicit sensitivity classification.
    pub sensitivity: Sensitivity,
    /// Whether the supplied representation has been redacted.
    pub redacted: bool,
}

/// Sensitivity boundary recorded for an exposed memory source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Public,
    Internal,
    Sensitive,
}

/// A context manifest is the immutable policy/tool/memory view of an attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextManifest {
    /// Schema version of this record.
    pub schema_version: SchemaVersion,
    /// Attempt that consumed the manifest.
    pub attempt_id: AttemptId,
    /// Policy source and content identity.
    pub policy: PolicyIdentity,
    /// Tools and adapters visible to the attempt.
    #[serde(default)]
    pub tools: Vec<ToolIdentity>,
    /// Memory exposures, including source digests and sensitivity.
    #[serde(default)]
    pub memory: Vec<MemoryExposure>,
    /// Redaction rules applied before evidence left the controller.
    #[serde(default)]
    pub redactions: Vec<RedactionBoundary>,
}

/// A declared redaction boundary; raw secret material never crosses this API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionBoundary {
    /// Name of the redaction rule or sanitizer policy.
    pub rule: String,
    /// Digest of the rule set used by the controller.
    pub ruleset_digest: Digest,
}

/// A redacted text value. The constructor communicates that the caller has
/// already removed secret material; the kernel never accepts raw transcript
/// fields in its canonical evidence records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RedactedText(String);

impl RedactedText {
    /// Wrap text that has already passed the controller's redaction boundary.
    pub fn from_redacted(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the redacted text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One attempt, before it has a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    /// Schema version of this record.
    pub schema_version: SchemaVersion,
    /// Immutable execution identity; retries use a new ID.
    pub attempt_id: AttemptId,
    /// Desired work item identity.
    pub bead_id: BeadId,
    /// Bead revision read before claim/dispatch.
    pub bead_revision: Revision,
    /// Claim fencing epoch read before dispatch.
    pub fencing_epoch: FencingEpoch,
    /// Caller-supplied dispatch start time.
    pub started_at: Timestamp,
}

/// A process observation, never a semantic success decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessObservation {
    /// Exit status observed from the adapter process, if it exited normally.
    pub exit_code: Option<i32>,
    /// Wall-clock duration measured by the controller.
    pub duration_ms: u64,
    /// Digest of sanitized stdout, if captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_digest: Option<ContentHash>,
    /// Digest of sanitized stderr, if captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_digest: Option<ContentHash>,
    /// Whether the controller interrupted the process.
    pub interrupted: bool,
}

/// A reference to immutable evidence owned by a controller or evidence store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Stable evidence identity.
    pub evidence_id: EvidenceId,
    /// Digest of the exact evidence bytes.
    pub digest: ContentHash,
    /// Source namespace, path, or URI (never the evidence contents).
    pub source: SourceId,
}

/// A bounded gate observation with no raw command output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateObservation {
    /// Stable gate name.
    pub name: String,
    /// Gate result.
    pub status: GateStatus,
    /// Optional evidence reference for the report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<EvidenceRef>,
}

/// Semantic gate result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    Pass,
    Fail,
    InfrastructureFailure,
    NotRun,
}

/// Observable evidence for one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceBundle {
    /// Schema version of this record.
    pub schema_version: SchemaVersion,
    /// Attempt to which all evidence belongs.
    pub attempt_id: AttemptId,
    /// Adapter process observation; exit zero is not success by itself.
    pub process: ProcessObservation,
    /// Gate observations, in canonical name order.
    #[serde(default)]
    pub gates: Vec<GateObservation>,
    /// Opaque evidence references.
    #[serde(default)]
    pub references: Vec<EvidenceRef>,
    /// Optional already-redacted summary for operator display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_summary: Option<RedactedText>,
}

/// Semantic outcome of a resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    VerifiedSuccess,
    WorkFailure,
    InfrastructureFailure,
    Indeterminate,
    Cancelled,
}

/// Lifecycle action proposed for an effect-owning controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedAction {
    None,
    Release,
    Quarantine,
    Reevaluate,
}

/// State confirmed by the authoritative work controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmedState {
    Open,
    InProgress,
    Closed,
    Unknown,
}

/// An immutable semantic resolution; controllers apply its requested action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    /// Schema version of this record.
    pub schema_version: SchemaVersion,
    /// Stable resolution identity.
    pub resolution_id: ResolutionId,
    /// Attempt being resolved.
    pub attempt_id: AttemptId,
    /// Semantic result, independent of process exit code.
    pub outcome: Outcome,
    /// Action for an effect-owning controller to consider.
    pub requested_action: RequestedAction,
    /// State re-read and confirmed by the authoritative controller.
    pub confirmed_state: ConfirmedState,
    /// Evidence supporting the result.
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
    /// Resulting revision, when the controller observed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resulting_revision: Option<Revision>,
}

/// Canonical input to a pure kernel operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactoryInput {
    /// Schema version of the envelope.
    pub schema_version: SchemaVersion,
    /// Immutable attempt identity.
    pub attempt: Attempt,
    /// Exact context exposure for the attempt.
    pub context: ContextManifest,
    /// Observable evidence for the attempt.
    pub evidence: EvidenceBundle,
    /// Resolution is optional while evidence is still incomplete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

/// A typed intent returned for an effect-owning controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Intent {
    /// Request more evidence without performing the request in the kernel.
    RequestEvidence { reason: EvidenceGap },
    /// Ask a controller to expose a candidate to a review queue.
    SubmitForReview { proposal_id: SourceId },
}

/// A typed explanation for incomplete evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceGap {
    MissingResolution,
    MissingSupportingEvidence,
    UnconfirmedState,
}

/// A candidate learning proposal, never an applied policy change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearningProposal {
    /// Stable proposal identity derived from immutable inputs.
    pub proposal_id: SourceId,
    /// Attempt that supplied the observation.
    pub attempt_id: AttemptId,
    /// Candidate outcome to be reviewed.
    pub outcome: Outcome,
    /// Evidence references supporting the candidate.
    pub evidence: Vec<EvidenceRef>,
    /// Explicitly requires a controller or human review.
    pub review_required: bool,
}

/// A typed evaluation of one immutable attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evaluation {
    /// Attempt evaluated.
    pub attempt_id: AttemptId,
    /// Resolution outcome when one exists.
    pub outcome: Option<Outcome>,
    /// Stable evaluation state.
    pub status: EvaluationStatus,
    /// Deterministic score in basis points, never an unbounded float.
    pub score_basis_points: u16,
}

/// Evaluation state produced by the pure reducer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationStatus {
    Verified,
    Failed,
    Incomplete,
    Conflicting,
}

/// Receipt describing the kernel evaluation; it does not claim an external
/// effect occurred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// Stable receipt identity.
    pub receipt_id: SourceId,
    /// Attempt evaluated.
    pub attempt_id: AttemptId,
    /// Digest of the canonical input represented by this receipt.
    pub input_digest: Digest,
    /// Whether the receipt is merely a kernel result or confirms an effect.
    pub effect_confirmed: bool,
}

/// Canonical output from a pure kernel operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactoryOutput {
    /// Schema version of the envelope.
    pub schema_version: SchemaVersion,
    /// Attempt evaluated.
    pub attempt_id: AttemptId,
    /// Deterministic evaluation.
    pub evaluation: Evaluation,
    /// Requests for effect-owning controllers.
    #[serde(default)]
    pub intents: Vec<Intent>,
    /// Candidate proposals requiring review.
    #[serde(default)]
    pub proposals: Vec<LearningProposal>,
    /// Kernel receipt; `effect_confirmed` is always false here.
    pub receipt: Receipt,
}

impl Attempt {
    /// Validate the record version and required identities.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err("unsupported attempt schema version");
        }
        Ok(())
    }
}

impl ContextManifest {
    /// Validate the record version and attempt correlation.
    pub fn validate_for(&self, attempt_id: &AttemptId) -> Result<(), &'static str> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err("unsupported context schema version");
        }
        if &self.attempt_id != attempt_id {
            return Err("context manifest belongs to another attempt");
        }
        Ok(())
    }
}

impl EvidenceBundle {
    /// Validate the record version and attempt correlation.
    pub fn validate_for(&self, attempt_id: &AttemptId) -> Result<(), &'static str> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err("unsupported evidence schema version");
        }
        if &self.attempt_id != attempt_id {
            return Err("evidence belongs to another attempt");
        }
        Ok(())
    }
}

impl Resolution {
    /// Validate semantic invariants without applying any lifecycle effect.
    pub fn validate_for(&self, attempt_id: &AttemptId) -> Result<(), &'static str> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err("unsupported resolution schema version");
        }
        if &self.attempt_id != attempt_id {
            return Err("resolution belongs to another attempt");
        }
        if self.outcome == Outcome::VerifiedSuccess
            && (self.confirmed_state != ConfirmedState::Closed || self.evidence.is_empty())
        {
            return Err("verified success requires closed state and evidence");
        }
        Ok(())
    }
}

impl FactoryInput {
    /// Validate all cross-record correlations and schema versions.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err("unsupported factory input schema version");
        }
        self.attempt.validate()?;
        self.context.validate_for(&self.attempt.attempt_id)?;
        self.evidence.validate_for(&self.attempt.attempt_id)?;
        if let Some(resolution) = &self.resolution {
            resolution.validate_for(&self.attempt.attempt_id)?;
        }
        Ok(())
    }
}

impl FactoryOutput {
    /// Assert that a kernel-produced output never claims an external effect.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err("unsupported factory output schema version");
        }
        if self.receipt.effect_confirmed {
            return Err("kernel output cannot confirm an external effect");
        }
        Ok(())
    }
}
