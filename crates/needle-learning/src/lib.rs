//! Pure, dependency-isolated learning kernel for NEEDLE.
//!
//! The crate contains versioned records and deterministic transformations only.
//! It has no process, filesystem, network, scheduler, Git, or bead-store
//! integration.  Controllers in the `needle` package own those effects and
//! exchange data with this crate through the records and read-only source
//! traits exported here.

#![forbid(unsafe_code)]

pub mod adapters;
pub mod factory_types;
pub mod kernel;
pub mod source;

pub use adapters::{LegacyAttemptEvent, LegacyObservation, LegacyOutcome};
pub use factory_types::{
    Attempt, AttemptId, BeadId, ConfirmedState, ContentHash, ContextManifest, Digest, Evaluation,
    EvaluationStatus, EvidenceBundle, EvidenceGap, EvidenceId, EvidenceRef, FactoryInput,
    FactoryOutput, FencingEpoch, GateObservation, GateStatus, Intent, InvalidId, LearningProposal,
    MemoryExposure, Outcome, PolicyIdentity, ProcessObservation, Receipt, RedactedText,
    RedactionBoundary, RequestedAction, Resolution, ResolutionId, Revision, SchemaVersion,
    Sensitivity, SourceId, Timestamp, ToolIdentity, CURRENT_SCHEMA_VERSION,
};
pub use kernel::{evaluate, KernelError};
pub use source::{AttemptSource, ContextSource, EvidenceSource, ReadOnlySource, ResolutionSource};
