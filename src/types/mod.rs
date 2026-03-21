//! Core types and enums for NEEDLE.
//!
//! This is a leaf module — it depends on nothing else in the crate.
//! All types here derive Debug, Clone, PartialEq, and Serialize/Deserialize.
//! Enums that may gain variants in the future are marked `#[non_exhaustive]`.
//!
//! Design invariant: no wildcard (`_`) arms in any `match` on these enums.
//! Every variant must be explicitly handled at every call site.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ──────────────────────────────────────────────────────────────────────────────
// BeadId newtype
// ──────────────────────────────────────────────────────────────────────────────

/// A validated bead identifier (e.g., `needle-gob`).
///
/// Wraps `String` with `Display`, `FromStr`, `Hash`, and `Eq` impls.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BeadId(String);

impl BeadId {
    /// Create a `BeadId` from any string-like value.
    pub fn new(s: impl Into<String>) -> Self {
        BeadId(s.into())
    }

    /// Return the inner string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BeadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for BeadId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(BeadId(s.to_owned()))
    }
}

impl From<String> for BeadId {
    fn from(s: String) -> Self {
        BeadId(s)
    }
}

impl From<&str> for BeadId {
    fn from(s: &str) -> Self {
        BeadId(s.to_owned())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Priority
// ──────────────────────────────────────────────────────────────────────────────

/// Unique identifier for a worker instance (e.g., `needle-alpha`).
pub type WorkerId = String;

/// Priority level of a bead. Lower number = higher priority (P1 > P2 > P3).
pub type Priority = u8;

// ──────────────────────────────────────────────────────────────────────────────
// BeadStatus
// ──────────────────────────────────────────────────────────────────────────────

/// Lifecycle status of a bead in the store.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadStatus {
    Open,
    InProgress,
    Done,
    Blocked,
}

impl fmt::Display for BeadStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BeadStatus::Open => write!(f, "open"),
            BeadStatus::InProgress => write!(f, "in_progress"),
            BeadStatus::Done => write!(f, "done"),
            BeadStatus::Blocked => write!(f, "blocked"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Bead struct
// ──────────────────────────────────────────────────────────────────────────────

/// A bead as returned from the bead store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bead {
    pub id: BeadId,
    pub title: String,
    pub body: Option<String>,
    pub priority: Priority,
    pub status: BeadStatus,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub workspace: Option<String>,
    pub dependencies: Vec<BeadId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Bead {
    /// Convenience constructor for testing.
    pub fn stub(id: impl Into<String>, title: impl Into<String>) -> Self {
        Bead {
            id: BeadId::new(id),
            title: title.into(),
            body: None,
            priority: 2,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec![],
            workspace: None,
            dependencies: vec![],
            created_at: DateTime::from_timestamp(0, 0).unwrap(),
            updated_at: DateTime::from_timestamp(0, 0).unwrap(),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// WorkerState
// ──────────────────────────────────────────────────────────────────────────────

/// Worker finite-state-machine states.
///
/// Every state has defined entry conditions, actions, and exit transitions.
/// There are no implicit states or fallthrough paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkerState {
    Booting,
    Selecting,
    Claiming,
    Retrying,
    Building,
    Dispatching,
    Executing,
    Handling,
    Logging,
    /// All strands returned empty — worker has nothing to do.
    Exhausted,
    /// Received graceful shutdown signal.
    Stopped,
    /// Unrecoverable error.
    Errored,
}

impl fmt::Display for WorkerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkerState::Booting => write!(f, "BOOTING"),
            WorkerState::Selecting => write!(f, "SELECTING"),
            WorkerState::Claiming => write!(f, "CLAIMING"),
            WorkerState::Retrying => write!(f, "RETRYING"),
            WorkerState::Building => write!(f, "BUILDING"),
            WorkerState::Dispatching => write!(f, "DISPATCHING"),
            WorkerState::Executing => write!(f, "EXECUTING"),
            WorkerState::Handling => write!(f, "HANDLING"),
            WorkerState::Logging => write!(f, "LOGGING"),
            WorkerState::Exhausted => write!(f, "EXHAUSTED"),
            WorkerState::Stopped => write!(f, "STOPPED"),
            WorkerState::Errored => write!(f, "ERRORED"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Outcome
// ──────────────────────────────────────────────────────────────────────────────

/// The classified outcome of an agent process.
///
/// Every exit code maps to exactly one variant via `Outcome::classify()`.
/// There is no catch-all variant — all codes are handled explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Outcome {
    /// Exit 0 — agent completed work successfully.
    Success,
    /// Exit 1 — agent reported a non-fatal failure.
    Failure,
    /// Exit 124 — timeout wrapper expired.
    Timeout,
    /// Exit 126 or 127 — agent binary not found or not executable.
    AgentNotFound,
    /// Exit 130 (SIGINT) or 143 (SIGTERM) — agent was interrupted.
    Interrupted,
    /// Any other non-zero exit code — process crashed or was killed.
    Crash {
        /// Raw exit code returned by the process.
        code: i32,
    },
}

impl Outcome {
    /// Map an exit code to an `Outcome` variant.
    ///
    /// Every possible `i32` value maps to exactly one variant.
    /// There is no wildcard — all ranges are handled explicitly.
    pub fn classify(code: i32) -> Self {
        match code {
            0 => Outcome::Success,
            1 => Outcome::Failure,
            124 => Outcome::Timeout,
            126 | 127 => Outcome::AgentNotFound,
            130 | 143 => Outcome::Interrupted,
            other => Outcome::Crash { code: other },
        }
    }

    /// Return `true` if the outcome is terminal (no retry possible).
    pub fn is_terminal(&self) -> bool {
        match self {
            Outcome::Success => true,
            Outcome::Failure => false,
            Outcome::Timeout => false,
            Outcome::AgentNotFound => true,
            Outcome::Interrupted => false,
            Outcome::Crash { code: _ } => false,
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Success => write!(f, "success"),
            Outcome::Failure => write!(f, "failure"),
            Outcome::Timeout => write!(f, "timeout"),
            Outcome::AgentNotFound => write!(f, "agent_not_found"),
            Outcome::Interrupted => write!(f, "interrupted"),
            Outcome::Crash { code } => write!(f, "crash({})", code),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// StrandResult / StrandError
// ──────────────────────────────────────────────────────────────────────────────

/// Error returned by a strand evaluation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StrandError {
    #[error("bead store error: {0}")]
    StoreError(String),
    #[error("strand configuration error: {0}")]
    ConfigError(String),
}

/// Result of a strand evaluation in the waterfall.
#[derive(Debug, Clone)]
pub enum StrandResult {
    /// One or more candidate beads were found.
    BeadFound(Vec<Bead>),
    /// The strand synthesized new work (e.g., mitosis created child beads).
    WorkCreated,
    /// This strand found nothing; continue to the next strand.
    NoWork,
    /// The strand encountered an error during evaluation.
    Error(StrandError),
}

// ──────────────────────────────────────────────────────────────────────────────
// ClaimResult / ClaimOutcome
// ──────────────────────────────────────────────────────────────────────────────

/// Result of a single claim attempt for one bead.
#[derive(Debug, Clone)]
pub enum ClaimResult {
    /// This worker successfully claimed the bead.
    Claimed(Bead),
    /// Another worker claimed the bead first.
    RaceLost {
        /// Assignee that won the race.
        claimed_by: String,
    },
    /// The bead cannot be claimed (not open, blocked, etc.).
    NotClaimable {
        /// Human-readable reason.
        reason: String,
    },
}

/// Aggregate outcome after exhausting all candidates for a selection cycle.
#[derive(Debug, Clone)]
pub enum ClaimOutcome {
    /// Successfully claimed a bead.
    Claimed(Bead),
    /// Raced every candidate and lost every time.
    AllRaceLost,
    /// The strand returned no candidates.
    NoCandidates,
    /// The bead store returned an error.
    StoreError(String),
}

// ──────────────────────────────────────────────────────────────────────────────
// InputMethod
// ──────────────────────────────────────────────────────────────────────────────

/// How the prompt is passed to the agent binary.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "method")]
pub enum InputMethod {
    /// Write the prompt to the agent's stdin.
    Stdin,
    /// Write the prompt to a temp file and pass the path.
    File {
        /// Template for the temp file path. `{bead_id}` is substituted.
        path_template: String,
    },
    /// Pass the prompt as a CLI argument.
    Args {
        /// Flag name (e.g., `--prompt`).
        flag: String,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// PeerStatus
// ──────────────────────────────────────────────────────────────────────────────

/// Health status of a peer worker as inferred from its heartbeat file.
#[derive(Debug, Clone)]
pub enum PeerStatus {
    /// Heartbeat is fresh — peer is considered alive.
    Alive {
        last_seen: DateTime<Utc>,
        current_bead: Option<BeadId>,
    },
    /// Heartbeat TTL has elapsed — peer may be stuck.
    Stale {
        last_seen: DateTime<Utc>,
        claimed_bead: Option<BeadId>,
    },
    /// Heartbeat file is missing — peer is dead or never started.
    Dead {
        /// Path of the expected heartbeat file.
        heartbeat_file: std::path::PathBuf,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// ProcessOutput
// ──────────────────────────────────────────────────────────────────────────────

/// Raw output from an agent process (before outcome classification).
#[derive(Debug, Clone)]
pub struct ProcessOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ProcessOutput {
    /// Classify the exit code into an `Outcome`.
    pub fn classify(&self) -> Outcome {
        Outcome::classify(self.exit_code)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// NeedleError
// ──────────────────────────────────────────────────────────────────────────────

/// Tier of a NEEDLE error — determines recovery strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorTier {
    /// Transient: retry after backoff (network hiccup, lock contention).
    Transient,
    /// Bead-scoped: this bead should be abandoned; other beads can proceed.
    BeadScoped,
    /// Worker-scoped: this worker should shut down; fleet continues.
    WorkerScoped,
}

/// Top-level NEEDLE error type.
///
/// Every variant includes context about which bead and/or workspace was
/// involved, so error messages are actionable without a stack trace.
#[allow(clippy::enum_variant_names)]
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum NeedleError {
    #[error("bead store error for bead {bead_id} in {workspace}: {message}")]
    BeadStoreError {
        /// `"<none>"` when no specific bead is implicated.
        bead_id: String,
        workspace: String,
        message: String,
        tier: ErrorTier,
    },

    #[error("claim failed for bead {bead_id}: {reason}")]
    ClaimError {
        bead_id: BeadId,
        reason: String,
        tier: ErrorTier,
    },

    #[error("dispatch failed for bead {bead_id}: {message}")]
    DispatchError {
        bead_id: BeadId,
        message: String,
        tier: ErrorTier,
    },

    #[error("configuration error: {message}")]
    ConfigError { message: String, tier: ErrorTier },

    #[error("health monitor error: {message}")]
    HealthError { message: String, tier: ErrorTier },
}

impl NeedleError {
    /// Return the error tier for recovery routing.
    pub fn tier(&self) -> &ErrorTier {
        match self {
            NeedleError::BeadStoreError { tier, .. } => tier,
            NeedleError::ClaimError { tier, .. } => tier,
            NeedleError::DispatchError { tier, .. } => tier,
            NeedleError::ConfigError { tier, .. } => tier,
            NeedleError::HealthError { tier, .. } => tier,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bead_id_roundtrip() {
        let id = BeadId::new("needle-gob");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""needle-gob""#);
        let decoded: BeadId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, decoded);
    }

    #[test]
    fn bead_id_display_and_fromstr() {
        let id = BeadId::new("needle-0ez");
        assert_eq!(id.to_string(), "needle-0ez");
        let parsed: BeadId = "needle-0ez".parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn outcome_classify_all_variants() {
        assert_eq!(Outcome::classify(0), Outcome::Success);
        assert_eq!(Outcome::classify(1), Outcome::Failure);
        assert_eq!(Outcome::classify(124), Outcome::Timeout);
        assert_eq!(Outcome::classify(126), Outcome::AgentNotFound);
        assert_eq!(Outcome::classify(127), Outcome::AgentNotFound);
        assert_eq!(Outcome::classify(130), Outcome::Interrupted);
        assert_eq!(Outcome::classify(143), Outcome::Interrupted);
        assert_eq!(Outcome::classify(137), Outcome::Crash { code: 137 });
        assert_eq!(Outcome::classify(2), Outcome::Crash { code: 2 });
    }

    #[test]
    fn outcome_roundtrip() {
        let cases = vec![
            Outcome::Success,
            Outcome::Failure,
            Outcome::Timeout,
            Outcome::AgentNotFound,
            Outcome::Interrupted,
            Outcome::Crash { code: 42 },
        ];
        for outcome in cases {
            let json = serde_json::to_string(&outcome).unwrap();
            let decoded: Outcome = serde_json::from_str(&json).unwrap();
            assert_eq!(outcome, decoded, "roundtrip failed for {:?}", outcome);
        }
    }

    #[test]
    fn worker_state_roundtrip() {
        let states = vec![
            WorkerState::Booting,
            WorkerState::Selecting,
            WorkerState::Claiming,
            WorkerState::Retrying,
            WorkerState::Building,
            WorkerState::Dispatching,
            WorkerState::Executing,
            WorkerState::Handling,
            WorkerState::Logging,
            WorkerState::Exhausted,
            WorkerState::Stopped,
            WorkerState::Errored,
        ];
        for state in states {
            let json = serde_json::to_string(&state).unwrap();
            let decoded: WorkerState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, decoded, "roundtrip failed for {:?}", state);
        }
    }

    #[test]
    fn bead_status_roundtrip() {
        let statuses = vec![
            BeadStatus::Open,
            BeadStatus::InProgress,
            BeadStatus::Done,
            BeadStatus::Blocked,
        ];
        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let decoded: BeadStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, decoded, "roundtrip failed for {:?}", status);
        }
    }

    #[test]
    fn bead_stub_serializes() {
        let bead = Bead::stub("needle-test", "Test bead");
        let json = serde_json::to_string(&bead).unwrap();
        let decoded: Bead = serde_json::from_str(&json).unwrap();
        assert_eq!(bead.id, decoded.id);
        assert_eq!(bead.title, decoded.title);
    }

    #[test]
    fn needle_error_display() {
        let err = NeedleError::ClaimError {
            bead_id: BeadId::new("needle-gob"),
            reason: "race lost".to_string(),
            tier: ErrorTier::Transient,
        };
        let msg = err.to_string();
        assert!(msg.contains("needle-gob"), "expected needle-gob in: {msg}");
        assert!(err.to_string().contains("race lost"));
        assert_eq!(err.tier(), &ErrorTier::Transient);
    }

    #[test]
    fn input_method_roundtrip() {
        let methods = vec![
            InputMethod::Stdin,
            InputMethod::File {
                path_template: "/tmp/needle-{bead_id}.txt".to_string(),
            },
            InputMethod::Args {
                flag: "--prompt".to_string(),
            },
        ];
        for method in methods {
            let json = serde_json::to_string(&method).unwrap();
            let decoded: InputMethod = serde_json::from_str(&json).unwrap();
            assert_eq!(method, decoded, "roundtrip failed for {:?}", method);
        }
    }
}
