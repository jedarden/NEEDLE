//! Narrow compatibility adapters for pre-kernel observations.
//!
//! Legacy events are useful migration inputs but are never upgraded into an
//! authoritative [`crate::Attempt`]. In particular, a missing attempt ID is
//! not synthesized from a bead ID or timestamp.

use serde::{Deserialize, Serialize};

use crate::factory_types::{AttemptId, BeadId, Outcome, Timestamp};

/// A legacy terminal event accepted for read-only migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyAttemptEvent {
    /// Historical event name.
    #[serde(default)]
    pub event_type: String,
    /// Historical schema version, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u16>,
    /// Attempt identity, when the old producer already had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<AttemptId>,
    /// Bead identity, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bead_id: Option<BeadId>,
    /// Historical semantic label; it remains observational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<LegacyOutcome>,
    /// Process exit observation, not a success decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Historical timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<Timestamp>,
}

/// Legacy outcome labels retained for compatibility decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyOutcome {
    Success,
    Failure,
    Cancelled,
    Unknown,
}

/// Explicitly non-authoritative migration output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyObservation {
    /// Original event type.
    pub event_type: String,
    /// Existing attempt identity, if the old event supplied one.
    pub attempt_id: Option<AttemptId>,
    /// Bead identity, if the old event supplied one.
    pub bead_id: Option<BeadId>,
    /// Historical outcome label.
    pub outcome: Option<LegacyOutcome>,
    /// Process observation.
    pub exit_code: Option<i32>,
    /// Historical timestamp.
    pub recorded_at: Option<Timestamp>,
    /// Always false: legacy events cannot establish authoritative resolution.
    pub authoritative: bool,
}

impl LegacyAttemptEvent {
    /// Decode the event as an explicitly non-authoritative observation.
    pub fn into_observation(self) -> LegacyObservation {
        LegacyObservation {
            event_type: self.event_type,
            attempt_id: self.attempt_id,
            bead_id: self.bead_id,
            outcome: self.outcome,
            exit_code: self.exit_code,
            recorded_at: self.recorded_at,
            authoritative: false,
        }
    }

    /// Return a semantic observation only when the historical label maps
    /// without pretending that legacy success was verified success.
    pub fn observed_outcome(&self) -> Option<Outcome> {
        match self.outcome {
            Some(LegacyOutcome::Failure) => Some(Outcome::WorkFailure),
            Some(LegacyOutcome::Cancelled) => Some(Outcome::Cancelled),
            Some(LegacyOutcome::Success) | Some(LegacyOutcome::Unknown) | None => None,
        }
    }
}
