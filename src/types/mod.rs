//! Core types and enums for NEEDLE.
//!
//! This is a leaf module — it depends on nothing else in the crate.
//! Enums that may gain variants in the future are marked `#[non_exhaustive]`.
//!
//! Design invariant: no wildcard (`_`) arms in any `match` on these enums.
//! Every variant must be explicitly handled at every call site.

use std::fmt;
use std::ops::Deref;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// BeadId newtype
// ──────────────────────────────────────────────────────────────────────────────

/// A validated bead identifier (e.g., `needle-gob`).
///
/// Wraps `String` with `Display`, `FromStr`, `Hash`, and `Eq` impls.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BeadId(String);

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

impl From<BeadId> for String {
    fn from(id: BeadId) -> Self {
        id.0
    }
}

impl AsRef<str> for BeadId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Deref for BeadId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// WorkerId type alias
// ──────────────────────────────────────────────────────────────────────────────

/// Identifies a worker instance (e.g., `needle-01`).
pub type WorkerId = String;

// ──────────────────────────────────────────────────────────────────────────────
// Priority type alias
// ──────────────────────────────────────────────────────────────────────────────

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
    /// `bf` (bead-forge) has been observed emitting `"completed"` for done
    /// beads on some workspaces (bf's own `Status` enum has no `Completed`
    /// variant — it falls through to an untagged `Custom(String)`, so this
    /// slips through bf-side validation). Accept it as an alias so a single
    /// such bead doesn't fail `bf list --json` deserialization for every
    /// other bead in the same call — see needle-weave-completed-status.
    #[serde(alias = "completed")]
    Done,
    /// `br show --json` emits `"closed"` for done beads. Treat as equivalent
    /// to `Done` so deserialization succeeds.
    Closed,
    Blocked,
}

impl BeadStatus {
    /// Returns true if the bead is finished (either `Done` or `Closed`).
    pub fn is_done(&self) -> bool {
        matches!(self, BeadStatus::Done | BeadStatus::Closed)
    }
}

impl fmt::Display for BeadStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BeadStatus::Open => write!(f, "open"),
            BeadStatus::InProgress => write!(f, "in_progress"),
            BeadStatus::Done | BeadStatus::Closed => write!(f, "done"),
            BeadStatus::Blocked => write!(f, "blocked"),
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
#[non_exhaustive]
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
        let s = match self {
            WorkerState::Booting => "BOOTING",
            WorkerState::Selecting => "SELECTING",
            WorkerState::Claiming => "CLAIMING",
            WorkerState::Retrying => "RETRYING",
            WorkerState::Building => "BUILDING",
            WorkerState::Dispatching => "DISPATCHING",
            WorkerState::Executing => "EXECUTING",
            WorkerState::Handling => "HANDLING",
            WorkerState::Logging => "LOGGING",
            WorkerState::Exhausted => "EXHAUSTED",
            WorkerState::Stopped => "STOPPED",
            WorkerState::Errored => "ERRORED",
        };
        f.write_str(s)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Outcome
// ──────────────────────────────────────────────────────────────────────────────

/// The classified outcome of an agent process.
///
/// Every exit code maps to exactly one variant via `Outcome::classify()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Exit 0 — agent completed work successfully.
    Success,
    /// Non-zero exit indicating a failure (includes app errors 1-99 and unknown codes).
    Failure,
    /// Exit 100 or 124 — timeout wrapper expired.
    Timeout,
    /// Exit 126 or 127 — agent binary not found or not executable.
    AgentNotFound,
    /// Exit 130 (SIGINT) or 143 (SIGTERM) — agent was interrupted.
    Interrupted,
    /// Negative exit code — process crashed or was killed by a signal.
    Crash(i32),
}

impl Outcome {
    /// Classify an exit code into an `Outcome` variant.
    ///
    /// Every exit code range has an explicit match arm — no wildcards.
    /// The `was_interrupted` flag takes precedence over exit code analysis.
    ///
    /// # Mapping (per spec)
    /// - `was_interrupted=true` → `Interrupted` (checked first)
    /// - exit 0 → `Success`
    /// - exit 1 → `Failure`
    /// - exit 124 → `Timeout`
    /// - exit 127 → `AgentNotFound`
    /// - exit >128 → `Crash(exit_code)`
    /// - exit <0 → `Crash(exit_code)`
    /// - all other → `Failure`
    pub fn classify(exit_code: i32, was_interrupted: bool) -> Self {
        // Interrupted flag takes precedence (graceful shutdown path).
        if was_interrupted {
            return Outcome::Interrupted;
        }

        // Explicit mapping for every exit code range — NO wildcards.
        // Each range is explicitly enumerated to ensure compile errors
        // if a new Outcome variant is added without updating this match.
        match exit_code {
            // Success
            0 => Outcome::Success,
            // Explicit failure code
            1 => Outcome::Failure,
            // Timeout (GNU timeout exit code)
            124 => Outcome::Timeout,
            // Agent not found (shell exit code for missing command)
            127 => Outcome::AgentNotFound,
            // Failure range: 2-123
            2..=123 => Outcome::Failure,
            // Failure: 125-128 (not >128 per spec, so 128 is not Crash)
            125..=128 => Outcome::Failure,
            // Signal exits: >128 (128 + signal number)
            // These are all crashes per the spec.
            129..=i32::MAX => Outcome::Crash(exit_code),
            // Negative exit codes (abnormal termination)
            i32::MIN..=-1 => Outcome::Crash(exit_code),
        }
    }

    /// Return a string representation for telemetry/logging.
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Failure => "failure",
            Outcome::Timeout => "timeout",
            Outcome::AgentNotFound => "agent_not_found",
            Outcome::Interrupted => "interrupted",
            Outcome::Crash(_) => "crash",
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// StrandError / StrandResult
// ──────────────────────────────────────────────────────────────────────────────

/// Error returned by a strand evaluation.
#[derive(Debug)]
pub enum StrandError {
    StoreError(anyhow::Error),
    ConfigError(String),
}

impl fmt::Display for StrandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StrandError::StoreError(e) => {
                // Show the full error chain, not just the top-level context.
                // This surfaces the actual stderr/stdout from bf/br commands.
                write!(f, "bead store error: {}", e)?;
                // Append any error causes from the chain (the actual bf/br stderr)
                for cause in e.chain().skip(1) {
                    write!(f, "\n  caused by: {}", cause)?;
                }
                Ok(())
            }
            StrandError::ConfigError(s) => write!(f, "strand configuration error: {}", s),
        }
    }
}

impl std::error::Error for StrandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StrandError::StoreError(e) => Some(e.as_ref()),
            StrandError::ConfigError(_) => None,
        }
    }
}

/// Result of a strand evaluation in the waterfall.
#[non_exhaustive]
#[derive(Debug)]
pub enum StrandResult {
    /// One or more candidate beads were found.
    BeadFound(Vec<Bead>),
    /// The strand synthesized new work (e.g., mitosis created child beads).
    WorkCreated,
    /// This strand found nothing; continue to the next strand.
    NoWork,
    /// The strand encountered an error during evaluation.
    Error(StrandError),
    /// The strand was skipped because its prerequisites are not met.
    ///
    /// This is distinct from `NoWork` (which means "ran and found nothing")
    /// and `Error` (which means "ran but failed"). `Skipped` means "did not
    /// run because the environment doesn't support it" — e.g., a strand that
    /// requires a home bead store when the worker is configured without one.
    Skipped { reason: String },
    /// A bead with too many consecutive failures should be split.
    ///
    /// Contains the bead to split and the current failure count.
    Split(Box<Bead>, u32),
    /// Candidates were found but all were excluded/assigned — short retry needed.
    ///
    /// This is distinct from `NoWork` (which means "truly no candidates").
    /// `FoundButExcluded` means "candidates exist but this worker can't claim
    /// them right now" — they're assigned to other workers or blocked by labels.
    /// This signals the worker to use short retry backoff instead of long idle.
    FoundButExcluded,
}

// ──────────────────────────────────────────────────────────────────────────────
// ClaimResult / ClaimOutcome
// ──────────────────────────────────────────────────────────────────────────────

/// Result of a single claim attempt for one bead.
#[non_exhaustive]
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
    /// Claim CLI failed with a store/CLI error (distinct from race-lost).
    ClaimError {
        /// Human-readable error message.
        reason: String,
    },
    /// A claim error threshold was reached — bead/store is suspect.
    ///
    /// This variant is emitted when a bead has failed claim attempts N times
    /// with errors (not race-lost conditions). The bead should be skipped and
    /// marked as suspect rather than silently cycling.
    Suspect {
        /// The bead ID that hit the error threshold.
        bead_id: BeadId,
        /// The number of consecutive claim errors.
        consecutive_errors: u32,
        /// The most recent error message.
        last_error: String,
    },
}

/// Aggregate outcome after exhausting all candidates for a selection cycle.
#[non_exhaustive]
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ClaimOutcome {
    /// Successfully claimed a bead.
    Claimed(Bead),
    /// Raced every candidate and lost every time.
    AllRaceLost,
    /// The strand returned no candidates.
    NoCandidates,
    /// The bead store returned an error.
    StoreError(anyhow::Error),
    /// A claim error threshold was reached — bead/store is suspect.
    ///
    /// This variant is emitted when a bead has failed claim attempts N times
    /// with errors (not race-lost conditions). The bead should be skipped and
    /// marked as suspect rather than silently cycling.
    Suspect {
        /// The bead ID that hit the error threshold.
        bead_id: BeadId,
        /// The number of consecutive claim errors.
        consecutive_errors: u32,
        /// The most recent error message.
        last_error: String,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// BrDependency
// ──────────────────────────────────────────────────────────────────────────────

/// A bead dependency as returned from the `br` JSON output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrDependency {
    pub id: BeadId,
    pub title: String,
    pub status: String,
    pub priority: Priority,
    pub dependency_type: String,
}

// ──────────────────────────────────────────────────────────────────────────────
// Bead struct
// ──────────────────────────────────────────────────────────────────────────────

/// Serde helper: treats an empty JSON string as `None`.
fn empty_string_as_none<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = Option::<String>::deserialize(d)?;
    Ok(s.filter(|s| !s.is_empty()))
}

/// A bead as returned from the bead store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bead {
    pub id: BeadId,
    pub title: String,
    /// Stored as `description` in br JSON output.
    #[serde(rename = "description")]
    pub body: Option<String>,
    pub priority: Priority,
    pub status: BeadStatus,
    /// br emits `""` for unassigned beads; normalize to `None`.
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub assignee: Option<String>,
    /// br may omit this field when empty.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Stored as `source_repo` in br JSON output.
    #[serde(rename = "source_repo", default)]
    pub workspace: std::path::PathBuf,
    #[serde(default)]
    pub dependencies: Vec<BrDependency>,
    #[serde(default)]
    pub dependents: Vec<BrDependency>,
    pub created_at: DateTime<Utc>,
    // br ready --json omits updated_at; default to now so explore can deserialize it
    #[serde(default = "chrono::Utc::now")]
    pub updated_at: DateTime<Utc>,
}

// ──────────────────────────────────────────────────────────────────────────────
// AgentOutcome
// ──────────────────────────────────────────────────────────────────────────────

/// Raw output from an agent process (before outcome classification).
#[derive(Debug, Clone)]
pub struct AgentOutcome {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

// ──────────────────────────────────────────────────────────────────────────────
// NeedleError
// ──────────────────────────────────────────────────────────────────────────────

/// Top-level NEEDLE error type.
///
/// Tier is encoded in the variant, so recovery routing is type-driven.
#[derive(Debug)]
pub enum NeedleError {
    /// Transient: retry after backoff (network hiccup, lock contention).
    Transient {
        message: String,
        bead_id: Option<BeadId>,
    },
    /// Bead-scoped: abandon this bead; other beads can proceed.
    BeadScoped { message: String, bead_id: BeadId },
    /// Worker-scoped: this worker should shut down; fleet continues.
    WorkerScoped {
        message: String,
        worker_id: WorkerId,
    },
}

impl fmt::Display for NeedleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NeedleError::Transient {
                message,
                bead_id: Some(id),
            } => {
                write!(f, "transient error (bead {}): {}", id, message)
            }
            NeedleError::Transient {
                message,
                bead_id: None,
            } => {
                write!(f, "transient error: {}", message)
            }
            NeedleError::BeadScoped { message, bead_id } => {
                write!(f, "bead-scoped error (bead {}): {}", bead_id, message)
            }
            NeedleError::WorkerScoped { message, worker_id } => {
                write!(f, "worker-scoped error (worker {}): {}", worker_id, message)
            }
        }
    }
}

impl std::error::Error for NeedleError {}

// ──────────────────────────────────────────────────────────────────────────────
// InputMethod
// ──────────────────────────────────────────────────────────────────────────────

/// How the prompt is passed to the agent binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
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
// HeartbeatFile / PeerStatus
// ──────────────────────────────────────────────────────────────────────────────

/// Path reference to a worker's heartbeat file on disk.
#[derive(Debug, Clone)]
pub struct HeartbeatFile {
    pub path: std::path::PathBuf,
}

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
    Dead { heartbeat_file: HeartbeatFile },
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bead_id_roundtrip() {
        // From<String>
        let id = BeadId::from("needle-gob".to_string());
        // Display
        assert_eq!(id.to_string(), "needle-gob");
        // FromStr
        let parsed: BeadId = "needle-gob".parse().unwrap();
        assert_eq!(id, parsed);
        // From<BeadId> for String
        let s: String = id.clone().into();
        assert_eq!(s, "needle-gob");
        // From<&str>
        let id2 = BeadId::from("needle-gob");
        assert_eq!(id, id2);
        // Deref
        assert_eq!(&*id, "needle-gob");
        // AsRef<str>
        let _: &str = id.as_ref();
    }

    #[test]
    fn worker_state_serialization() {
        // Verify SCREAMING_SNAKE_CASE serialization
        let json = serde_json::to_string(&WorkerState::Booting).unwrap();
        assert_eq!(json, r#""BOOTING""#);
        let json = serde_json::to_string(&WorkerState::Selecting).unwrap();
        assert_eq!(json, r#""SELECTING""#);
        let json = serde_json::to_string(&WorkerState::Exhausted).unwrap();
        assert_eq!(json, r#""EXHAUSTED""#);
    }

    #[test]
    fn bead_status_serialization() {
        // Verify snake_case serialization
        let json = serde_json::to_string(&BeadStatus::Open).unwrap();
        assert_eq!(json, r#""open""#);
        let json = serde_json::to_string(&BeadStatus::InProgress).unwrap();
        assert_eq!(json, r#""in_progress""#);
        let json = serde_json::to_string(&BeadStatus::Done).unwrap();
        assert_eq!(json, r#""done""#);
        let json = serde_json::to_string(&BeadStatus::Blocked).unwrap();
        assert_eq!(json, r#""blocked""#);
    }

    #[test]
    fn outcome_classify_key_codes() {
        // Core mappings per spec
        assert_eq!(Outcome::classify(0, false), Outcome::Success);
        assert_eq!(Outcome::classify(1, false), Outcome::Failure);
        assert_eq!(Outcome::classify(124, false), Outcome::Timeout);
        assert_eq!(Outcome::classify(127, false), Outcome::AgentNotFound);
    }

    #[test]
    fn outcome_classify_ranges() {
        // 2..=123 map to Failure (except 124 which is Timeout)
        assert_eq!(Outcome::classify(2, false), Outcome::Failure);
        assert_eq!(Outcome::classify(50, false), Outcome::Failure);
        assert_eq!(Outcome::classify(99, false), Outcome::Failure);
        assert_eq!(Outcome::classify(100, false), Outcome::Failure); // NOT timeout per spec
        assert_eq!(Outcome::classify(123, false), Outcome::Failure);
        // 125-126 -> Failure (not AgentNotFound per spec)
        assert_eq!(Outcome::classify(125, false), Outcome::Failure);
        assert_eq!(Outcome::classify(126, false), Outcome::Failure);
        // >128 -> Crash (signal exits)
        assert_eq!(Outcome::classify(128, false), Outcome::Failure); // 128 is NOT >128 per spec
        assert_eq!(Outcome::classify(129, false), Outcome::Crash(129));
        assert_eq!(Outcome::classify(130, false), Outcome::Crash(130)); // SIGINT -> Crash
        assert_eq!(Outcome::classify(137, false), Outcome::Crash(137)); // SIGKILL
        assert_eq!(Outcome::classify(143, false), Outcome::Crash(143)); // SIGTERM -> Crash
        assert_eq!(Outcome::classify(255, false), Outcome::Crash(255));
        // negative -> Crash
        assert_eq!(Outcome::classify(-1, false), Outcome::Crash(-1));
        assert_eq!(Outcome::classify(-9, false), Outcome::Crash(-9));
        // Large positive values >255 -> Crash per spec (exit > 128)
        assert_eq!(Outcome::classify(256, false), Outcome::Crash(256));
        assert_eq!(Outcome::classify(1000, false), Outcome::Crash(1000));
    }

    #[test]
    fn outcome_classify_interrupted_flag() {
        // was_interrupted=true always returns Interrupted, regardless of exit code
        assert_eq!(Outcome::classify(0, true), Outcome::Interrupted);
        assert_eq!(Outcome::classify(1, true), Outcome::Interrupted);
        assert_eq!(Outcome::classify(127, true), Outcome::Interrupted);
        assert_eq!(Outcome::classify(-1, true), Outcome::Interrupted);
        assert_eq!(Outcome::classify(137, true), Outcome::Interrupted);
    }

    #[test]
    fn outcome_as_str() {
        assert_eq!(Outcome::Success.as_str(), "success");
        assert_eq!(Outcome::Failure.as_str(), "failure");
        assert_eq!(Outcome::Timeout.as_str(), "timeout");
        assert_eq!(Outcome::AgentNotFound.as_str(), "agent_not_found");
        assert_eq!(Outcome::Interrupted.as_str(), "interrupted");
        assert_eq!(Outcome::Crash(137).as_str(), "crash");
    }

    #[test]
    fn bead_deserialization_from_br_json() {
        // Matches the field names br actually emits (description, source_repo)
        let json = r#"{
            "id": "needle-abc",
            "title": "Test bead",
            "description": "Do something useful",
            "priority": 2,
            "status": "open",
            "assignee": null,
            "source_repo": "/home/coding/NEEDLE",
            "dependencies": [],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }"#;
        let bead: Bead = serde_json::from_str(json).unwrap();
        assert_eq!(bead.id, BeadId::from("needle-abc"));
        assert_eq!(bead.title, "Test bead");
        assert_eq!(bead.body, Some("Do something useful".to_string()));
        assert_eq!(bead.priority, 2);
        assert_eq!(bead.status, BeadStatus::Open);
        assert_eq!(
            bead.workspace,
            std::path::PathBuf::from("/home/coding/NEEDLE")
        );
        assert!(bead.labels.is_empty());
        assert!(bead.dependencies.is_empty());
    }

    #[test]
    fn needle_error_display() {
        let e = NeedleError::Transient {
            message: "connection refused".to_string(),
            bead_id: Some(BeadId::from("needle-xyz")),
        };
        let s = e.to_string();
        assert!(s.contains("needle-xyz"), "expected bead id in: {}", s);
        assert!(
            s.contains("connection refused"),
            "expected message in: {}",
            s
        );

        let e2 = NeedleError::BeadScoped {
            message: "parse failure".to_string(),
            bead_id: BeadId::from("needle-123"),
        };
        let s2 = e2.to_string();
        assert!(s2.contains("needle-123"), "expected bead id in: {}", s2);
        assert!(s2.contains("parse failure"), "expected message in: {}", s2);

        let e3 = NeedleError::WorkerScoped {
            message: "fatal config error".to_string(),
            worker_id: "needle-worker-01".to_string(),
        };
        let s3 = e3.to_string();
        assert!(
            s3.contains("needle-worker-01"),
            "expected worker id in: {}",
            s3
        );
        assert!(
            s3.contains("fatal config error"),
            "expected message in: {}",
            s3
        );
    }

    #[test]
    fn needle_error_transient_without_bead_id() {
        let e = NeedleError::Transient {
            message: "lock contention".to_string(),
            bead_id: None,
        };
        let s = e.to_string();
        assert!(
            s.contains("transient error: lock contention"),
            "expected plain transient format in: {}",
            s
        );
        assert!(!s.contains("bead"), "should not mention bead in: {}", s);
    }

    #[test]
    fn bead_status_is_done() {
        assert!(!BeadStatus::Open.is_done());
        assert!(!BeadStatus::InProgress.is_done());
        assert!(BeadStatus::Done.is_done());
        assert!(BeadStatus::Closed.is_done());
        assert!(!BeadStatus::Blocked.is_done());
    }

    #[test]
    fn bead_status_display() {
        assert_eq!(BeadStatus::Open.to_string(), "open");
        assert_eq!(BeadStatus::InProgress.to_string(), "in_progress");
        assert_eq!(BeadStatus::Done.to_string(), "done");
        assert_eq!(BeadStatus::Closed.to_string(), "done"); // Closed displays as done
        assert_eq!(BeadStatus::Blocked.to_string(), "blocked");
    }

    #[test]
    fn bead_status_closed_deserialization() {
        // br emits "closed" for done beads — must deserialize correctly
        let status: BeadStatus = serde_json::from_str(r#""closed""#).unwrap();
        assert_eq!(status, BeadStatus::Closed);
        assert!(status.is_done());
    }

    #[test]
    fn bead_status_completed_deserialization() {
        // bf emits "completed" for some done beads (via its untagged Custom(String)
        // fallback, since bf's own Status enum has no Completed variant) — must
        // deserialize correctly instead of aborting the whole `bf list --json` parse.
        let status: BeadStatus = serde_json::from_str(r#""completed""#).unwrap();
        assert_eq!(status, BeadStatus::Done);
        assert!(status.is_done());
    }

    #[test]
    fn worker_state_display_all_variants() {
        assert_eq!(WorkerState::Booting.to_string(), "BOOTING");
        assert_eq!(WorkerState::Selecting.to_string(), "SELECTING");
        assert_eq!(WorkerState::Claiming.to_string(), "CLAIMING");
        assert_eq!(WorkerState::Retrying.to_string(), "RETRYING");
        assert_eq!(WorkerState::Building.to_string(), "BUILDING");
        assert_eq!(WorkerState::Dispatching.to_string(), "DISPATCHING");
        assert_eq!(WorkerState::Executing.to_string(), "EXECUTING");
        assert_eq!(WorkerState::Handling.to_string(), "HANDLING");
        assert_eq!(WorkerState::Logging.to_string(), "LOGGING");
        assert_eq!(WorkerState::Exhausted.to_string(), "EXHAUSTED");
        assert_eq!(WorkerState::Stopped.to_string(), "STOPPED");
        assert_eq!(WorkerState::Errored.to_string(), "ERRORED");
    }

    #[test]
    fn worker_state_deserialization_roundtrip() {
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
            let parsed: WorkerState = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, state, "roundtrip failed for {:?}", state);
        }
    }

    #[test]
    fn strand_error_display() {
        let store_err =
            StrandError::StoreError(anyhow::anyhow!("database disk image is malformed"));
        assert!(store_err
            .to_string()
            .contains("database disk image is malformed"));
        assert!(store_err.to_string().starts_with("bead store error:"));

        let config_err = StrandError::ConfigError("missing workspace path".to_string());
        assert!(config_err.to_string().contains("missing workspace path"));
        assert!(config_err
            .to_string()
            .starts_with("strand configuration error:"));
    }

    #[test]
    fn strand_error_source() {
        let store_err = StrandError::StoreError(anyhow::anyhow!("broken"));
        assert!(
            std::error::Error::source(&store_err).is_some(),
            "StoreError should have a source"
        );

        let config_err = StrandError::ConfigError("bad".to_string());
        assert!(
            std::error::Error::source(&config_err).is_none(),
            "ConfigError should not have a source"
        );
    }

    #[test]
    fn outcome_classify_boundary_values() {
        // Exact boundaries between ranges
        assert_eq!(Outcome::classify(0, false), Outcome::Success);
        assert_eq!(Outcome::classify(1, false), Outcome::Failure);
        assert_eq!(Outcome::classify(2, false), Outcome::Failure);
        assert_eq!(Outcome::classify(123, false), Outcome::Failure);
        assert_eq!(Outcome::classify(124, false), Outcome::Timeout);
        assert_eq!(Outcome::classify(125, false), Outcome::Failure);
        assert_eq!(Outcome::classify(126, false), Outcome::Failure);
        assert_eq!(Outcome::classify(127, false), Outcome::AgentNotFound);
        assert_eq!(Outcome::classify(128, false), Outcome::Failure);
        assert_eq!(Outcome::classify(129, false), Outcome::Crash(129)); // First crash code

        // i32 extremes
        assert_eq!(Outcome::classify(i32::MAX, false), Outcome::Crash(i32::MAX));
        assert_eq!(Outcome::classify(i32::MIN, false), Outcome::Crash(i32::MIN));
    }

    #[test]
    fn outcome_classify_common_signals() {
        // SIGHUP (128 + 1 = 129)
        assert_eq!(Outcome::classify(129, false), Outcome::Crash(129));
        // SIGINT (128 + 2 = 130)
        assert_eq!(Outcome::classify(130, false), Outcome::Crash(130));
        // SIGKILL (128 + 9 = 137)
        assert_eq!(Outcome::classify(137, false), Outcome::Crash(137));
        // SIGTERM (128 + 15 = 143)
        assert_eq!(Outcome::classify(143, false), Outcome::Crash(143));
        // SIGSEGV (128 + 11 = 139)
        assert_eq!(Outcome::classify(139, false), Outcome::Crash(139));
    }

    #[test]
    fn bead_action_display() {
        assert_eq!(BeadAction::Released.to_string(), "released");
        assert_eq!(BeadAction::Deferred.to_string(), "deferred");
        assert_eq!(BeadAction::Alerted.to_string(), "alerted");
        assert_eq!(BeadAction::None.to_string(), "none");
    }

    #[test]
    fn idle_action_default_is_wait() {
        assert_eq!(IdleAction::default(), IdleAction::Wait);
    }

    #[test]
    fn idle_action_serde_roundtrip() {
        let wait_json = serde_json::to_string(&IdleAction::Wait).unwrap();
        assert_eq!(wait_json, r#""wait""#);
        let exit_json = serde_json::to_string(&IdleAction::Exit).unwrap();
        assert_eq!(exit_json, r#""exit""#);

        let parsed: IdleAction = serde_json::from_str(r#""wait""#).unwrap();
        assert_eq!(parsed, IdleAction::Wait);
        let parsed: IdleAction = serde_json::from_str(r#""exit""#).unwrap();
        assert_eq!(parsed, IdleAction::Exit);
    }

    #[test]
    fn identifier_scheme_default_is_hostname_random() {
        assert_eq!(
            IdentifierScheme::default(),
            IdentifierScheme::HostnameRandom
        );
    }

    #[test]
    fn identifier_scheme_serde_roundtrip() {
        let schemes = vec![
            (IdentifierScheme::HostnameRandom, r#""hostname_random""#),
            (IdentifierScheme::Sequential, r#""sequential""#),
            (IdentifierScheme::Uuid, r#""uuid""#),
        ];
        for (scheme, expected_json) in schemes {
            let json = serde_json::to_string(&scheme).unwrap();
            assert_eq!(json, expected_json, "serialize {:?}", scheme);
            let parsed: IdentifierScheme = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, scheme, "roundtrip {:?}", scheme);
        }
    }

    #[test]
    fn exhaustion_diagnosis_display() {
        assert_eq!(
            ExhaustionDiagnosis::NoBeadsExist.to_string(),
            "no_beads_exist"
        );
        assert_eq!(ExhaustionDiagnosis::AllClaimed.to_string(), "all_claimed");
        assert_eq!(ExhaustionDiagnosis::Invisible.to_string(), "invisible");
    }

    #[test]
    fn exhaustion_diagnosis_serde_roundtrip() {
        let variants = vec![
            ExhaustionDiagnosis::NoBeadsExist,
            ExhaustionDiagnosis::AllClaimed,
            ExhaustionDiagnosis::Invisible,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let parsed: ExhaustionDiagnosis = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant, "roundtrip failed for {:?}", variant);
        }
    }

    #[test]
    fn input_method_serde_roundtrip() {
        let stdin = InputMethod::Stdin;
        let json = serde_json::to_string(&stdin).unwrap();
        let parsed: InputMethod = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, stdin);

        let file = InputMethod::File {
            path_template: "/tmp/{bead_id}.md".to_string(),
        };
        let json = serde_json::to_string(&file).unwrap();
        let parsed: InputMethod = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, file);

        let args = InputMethod::Args {
            flag: "--prompt".to_string(),
        };
        let json = serde_json::to_string(&args).unwrap();
        let parsed: InputMethod = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, args);
    }

    #[test]
    fn bead_deserialization_with_labels_and_closed_status() {
        let json = r#"{
            "id": "needle-xyz",
            "title": "Labeled bead",
            "description": null,
            "priority": 1,
            "status": "closed",
            "assignee": "worker-01",
            "labels": ["deferred", "mitosis-child"],
            "source_repo": "/tmp/workspace",
            "dependencies": [],
            "created_at": "2026-03-20T12:00:00Z",
            "updated_at": "2026-03-21T08:30:00Z"
        }"#;
        let bead: Bead = serde_json::from_str(json).unwrap();
        assert_eq!(bead.id, BeadId::from("needle-xyz"));
        assert!(bead.body.is_none());
        assert!(bead.status.is_done());
        assert_eq!(bead.status, BeadStatus::Closed);
        assert_eq!(bead.assignee, Some("worker-01".to_string()));
        assert_eq!(bead.labels, vec!["deferred", "mitosis-child"]);
    }

    #[test]
    fn bead_deserialization_missing_optional_fields() {
        // br may omit labels and source_repo when empty
        let json = r#"{
            "id": "needle-min",
            "title": "Minimal bead",
            "description": null,
            "priority": 3,
            "status": "open",
            "assignee": null,
            "dependencies": [],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }"#;
        let bead: Bead = serde_json::from_str(json).unwrap();
        assert!(bead.labels.is_empty());
        assert_eq!(bead.workspace, std::path::PathBuf::from(""));
    }

    #[test]
    fn br_dependency_deserialization() {
        let json = r#"{
            "id": "needle-dep",
            "title": "Dependency bead",
            "status": "open",
            "priority": 1,
            "dependency_type": "blocks"
        }"#;
        let dep: BrDependency = serde_json::from_str(json).unwrap();
        assert_eq!(dep.id, BeadId::from("needle-dep"));
        assert_eq!(dep.title, "Dependency bead");
        assert_eq!(dep.status, "open");
        assert_eq!(dep.priority, 1);
        assert_eq!(dep.dependency_type, "blocks");
    }

    #[test]
    fn bead_deserialization_with_both_dependencies_and_dependents() {
        // br show --json emits both "dependencies" and "dependents" as separate arrays.
        // They must deserialize into separate fields (not alias each other).
        let json = r#"{
            "id": "needle-both",
            "title": "Both fields test",
            "description": null,
            "priority": 1,
            "status": "open",
            "assignee": null,
            "dependencies": [
                {"id": "needle-blocker", "title": "Blocker", "status": "closed", "priority": 1, "dependency_type": "blocks"}
            ],
            "dependents": [
                {"id": "needle-child", "title": "Child", "status": "open", "priority": 1, "dependency_type": "blocks"}
            ],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }"#;
        let bead: Bead = serde_json::from_str(json).unwrap();
        assert_eq!(bead.dependencies.len(), 1);
        assert_eq!(bead.dependencies[0].id, BeadId::from("needle-blocker"));
        assert_eq!(bead.dependents.len(), 1);
        assert_eq!(bead.dependents[0].id, BeadId::from("needle-child"));
    }

    #[test]
    fn bead_id_hash_equality() {
        use std::collections::HashSet;
        let id1 = BeadId::from("needle-abc");
        let id2 = BeadId::from("needle-abc");
        let id3 = BeadId::from("needle-xyz");

        let mut set = HashSet::new();
        set.insert(id1.clone());
        assert!(set.contains(&id2));
        assert!(!set.contains(&id3));
        set.insert(id3.clone());
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn bead_id_serde_transparent() {
        // serde(transparent) means it serializes as a plain string, not an object
        let id = BeadId::from("needle-test");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""needle-test""#);
        let parsed: BeadId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn needle_error_is_error_trait() {
        // Verify NeedleError implements std::error::Error
        let e = NeedleError::Transient {
            message: "test".to_string(),
            bead_id: None,
        };
        let _: &dyn std::error::Error = &e;
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // OutputCapture tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn output_capture_success_with_zero_exit() {
        let capture = OutputCapture {
            stdout: "running 1 test\ntest foo ... ok".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: Duration::from_secs(1),
        };
        assert!(capture.success());
        assert!(!capture.failed());
    }

    #[test]
    fn output_capture_failed_with_nonzero_exit() {
        let capture = OutputCapture {
            stdout: String::new(),
            stderr: "error: test failed".to_string(),
            exit_code: Some(1),
            duration: Duration::from_millis(500),
        };
        assert!(!capture.success());
        assert!(capture.failed());
    }

    #[test]
    fn output_capture_no_exit_code_is_failure() {
        let capture = OutputCapture {
            stdout: String::new(),
            stderr: "killed by signal".to_string(),
            exit_code: None,
            duration: Duration::from_secs(10),
        };
        assert!(!capture.success());
        assert!(capture.failed());
    }

    #[test]
    fn output_capture_duration_ms() {
        let capture = OutputCapture {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: Duration::from_millis(2500),
        };
        assert_eq!(capture.duration_ms(), 2500);
    }

    #[test]
    fn output_capture_total_output_len() {
        let capture = OutputCapture {
            stdout: "test output\n".to_string(),
            stderr: "error message".to_string(),
            exit_code: Some(1),
            duration: Duration::ZERO,
        };
        assert_eq!(capture.total_output_len(), 12 + 13); // stdout len + stderr len
    }

    #[test]
    fn output_capture_serde_serialization() {
        let capture = OutputCapture {
            stdout: "test output".to_string(),
            stderr: "test warnings".to_string(),
            exit_code: Some(0),
            duration: Duration::from_millis(1500),
        };

        // Serialize to JSON
        let json = serde_json::to_string(&capture).unwrap();
        assert!(json.contains("\"stdout\":\"test output\""));
        assert!(json.contains("\"exit_code\":0"));
        assert!(json.contains("\"duration\":1500"));

        // Deserialize from JSON
        let deserialized: OutputCapture = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.stdout, "test output");
        assert_eq!(deserialized.stderr, "test warnings");
        assert_eq!(deserialized.exit_code, Some(0));
        assert_eq!(deserialized.duration_ms(), 1500);
    }

    #[test]
    fn output_capture_serde_with_none_exit_code() {
        let capture = OutputCapture {
            stdout: String::new(),
            stderr: "killed".to_string(),
            exit_code: None,
            duration: Duration::from_secs(5),
        };

        let json = serde_json::to_string(&capture).unwrap();
        assert!(json.contains("\"exit_code\":null"));

        let deserialized: OutputCapture = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.exit_code, None);
        assert_eq!(deserialized.duration.as_secs(), 5);
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // CompilationError tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn compilation_error_rust_error_description() {
        let error = CompilationError::RustError {
            code: "E0308".to_string(),
            message: "mismatched types".to_string(),
            file: Some("src/main.rs".to_string()),
            line: Some(10),
            column: Some(5),
        };
        assert_eq!(error.description(), "E0308: mismatched types");
        assert_eq!(error.error_code(), Some("E0308"));
        assert!(error.has_location());
    }

    #[test]
    fn compilation_error_general_description() {
        let error = CompilationError::General {
            message: "could not compile `my_crate`".to_string(),
        };
        assert_eq!(error.description(), "could not compile `my_crate`");
        assert!(error.error_code().is_none());
        assert!(!error.has_location());
    }

    #[test]
    fn compilation_error_abort_description() {
        let error = CompilationError::Abort { error_count: 3 };
        assert_eq!(error.description(), "aborting due to 3 previous error(s)");
        assert!(error.error_code().is_none());
        assert!(!error.has_location());
    }

    #[test]
    fn compilation_error_location_string() {
        let error = CompilationError::RustError {
            code: "E0382".to_string(),
            message: "use of moved value".to_string(),
            file: Some("src/lib.rs".to_string()),
            line: Some(42),
            column: Some(10),
        };
        assert_eq!(
            error.location_string(),
            Some("src/lib.rs:42:10".to_string())
        );
    }

    #[test]
    fn compilation_error_location_string_no_line() {
        let error = CompilationError::RustError {
            code: "E0001".to_string(),
            message: "some error".to_string(),
            file: Some("src/main.rs".to_string()),
            line: None,
            column: None,
        };
        assert_eq!(error.location_string(), Some("src/main.rs".to_string()));
    }

    #[test]
    fn compilation_error_location_string_no_file() {
        let error = CompilationError::RustError {
            code: "E0001".to_string(),
            message: "some error".to_string(),
            file: None,
            line: None,
            column: None,
        };
        assert!(error.location_string().is_none());
    }

    #[test]
    fn compilation_error_display_rust_error() {
        let error = CompilationError::RustError {
            code: "E0308".to_string(),
            message: "mismatched types".to_string(),
            file: Some("src/main.rs".to_string()),
            line: Some(10),
            column: Some(5),
        };
        let display = format!("{}", error);
        assert!(display.contains("E0308"));
        assert!(display.contains("mismatched types"));
        assert!(display.contains("src/main.rs:10:5"));
    }

    #[test]
    fn compilation_error_display_general() {
        let error = CompilationError::General {
            message: "compilation failed".to_string(),
        };
        let display = format!("{}", error);
        assert_eq!(display, "compilation failed");
    }

    #[test]
    fn compilation_error_display_abort() {
        let error = CompilationError::Abort { error_count: 5 };
        let display = format!("{}", error);
        assert_eq!(display, "aborting due to 5 previous error(s)");
    }

    #[test]
    fn compilation_error_serde_rust_error() {
        let error = CompilationError::RustError {
            code: "E0308".to_string(),
            message: "mismatched types".to_string(),
            file: Some("src/main.rs".to_string()),
            line: Some(10),
            column: Some(5),
        };

        let json = serde_json::to_string(&error).unwrap();
        assert!(json.contains("\"rust_error\""));
        assert!(json.contains("\"E0308\""));
        assert!(json.contains("mismatched types"));

        let deserialized: CompilationError = serde_json::from_str(&json).unwrap();
        match deserialized {
            CompilationError::RustError {
                code,
                message,
                file,
                line,
                column,
            } => {
                assert_eq!(code, "E0308");
                assert_eq!(message, "mismatched types");
                assert_eq!(file, Some("src/main.rs".to_string()));
                assert_eq!(line, Some(10));
                assert_eq!(column, Some(5));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn compilation_error_serde_general() {
        let error = CompilationError::General {
            message: "compilation failed".to_string(),
        };

        let json = serde_json::to_string(&error).unwrap();
        assert!(json.contains("\"general\""));
        assert!(json.contains("compilation failed"));

        let deserialized: CompilationError = serde_json::from_str(&json).unwrap();
        match deserialized {
            CompilationError::General { message } => {
                assert_eq!(message, "compilation failed");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn compilation_error_serde_abort() {
        let error = CompilationError::Abort { error_count: 3 };

        let json = serde_json::to_string(&error).unwrap();
        assert!(json.contains("\"abort\""));
        assert!(json.contains("\"error_count\":3"));

        let deserialized: CompilationError = serde_json::from_str(&json).unwrap();
        match deserialized {
            CompilationError::Abort { error_count } => {
                assert_eq!(error_count, 3);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn compilation_error_all_variants_have_display_impl() {
        // Verify all variants can be displayed
        let errors = vec![
            CompilationError::RustError {
                code: "E0308".to_string(),
                message: "msg".to_string(),
                file: None,
                line: None,
                column: None,
            },
            CompilationError::General {
                message: "msg".to_string(),
            },
            CompilationError::Abort { error_count: 1 },
        ];

        for error in errors {
            let _ = format!("{}", error); // Should not panic
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// IdleAction
// ──────────────────────────────────────────────────────────────────────────────

/// What a worker does when the queue is empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdleAction {
    /// Poll again after `idle_timeout` seconds.
    #[default]
    Wait,
    /// Exit cleanly.
    Exit,
}

// ──────────────────────────────────────────────────────────────────────────────
// BeadAction
// ──────────────────────────────────────────────────────────────────────────────

/// Action taken on a bead by the outcome handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeadAction {
    /// Bead was released back to open status.
    Released,
    /// Bead was deferred (e.g., timeout with deferred label).
    Deferred,
    /// An alert bead was created.
    Alerted,
    /// No action taken (e.g., success with bead already closed).
    None,
}

impl fmt::Display for BeadAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BeadAction::Released => write!(f, "released"),
            BeadAction::Deferred => write!(f, "deferred"),
            BeadAction::Alerted => write!(f, "alerted"),
            BeadAction::None => write!(f, "none"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// HandlerResult
// ──────────────────────────────────────────────────────────────────────────────

/// Result of handling an agent outcome.
#[derive(Debug)]
pub struct HandlerResult {
    /// The classified outcome.
    pub outcome: Outcome,
    /// Action taken on the bead.
    pub bead_action: BeadAction,
    /// Telemetry events emitted during handling.
    pub telemetry_events: Vec<crate::telemetry::EventKind>,
}

// ──────────────────────────────────────────────────────────────────────────────
// IdentifierScheme
// ──────────────────────────────────────────────────────────────────────────────

/// How workers generate their unique names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdentifierScheme {
    /// Use the hostname plus a short random suffix.
    #[default]
    HostnameRandom,
    /// Use a sequential integer suffix.
    Sequential,
    /// Use a UUID v4.
    Uuid,
}

// ──────────────────────────────────────────────────────────────────────────────
// ExhaustionDiagnosis
// ──────────────────────────────────────────────────────────────────────────────

/// Diagnosis from the Knot strand when all work-finding strategies are exhausted.
///
/// This three-state model prevents false-positive starvation alerts by
/// distinguishing between "queue genuinely empty" vs "all work claimed by
/// other workers" vs "beads exist but are invisible due to configuration."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExhaustionDiagnosis {
    /// No beads exist in the workspace at all — queue is genuinely empty.
    /// This is normal idle, not an alert condition.
    NoBeadsExist,
    /// All beads are claimed by other workers — wait for them to finish.
    /// This is normal congestion, not an alert condition.
    AllClaimed,
    /// Open beads exist but Pluck found none — indicates a config error.
    /// This is an alert condition: beads may be invisible due to label filters.
    Invisible,
}

impl fmt::Display for ExhaustionDiagnosis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExhaustionDiagnosis::NoBeadsExist => write!(f, "no_beads_exist"),
            ExhaustionDiagnosis::AllClaimed => write!(f, "all_claimed"),
            ExhaustionDiagnosis::Invisible => write!(f, "invisible"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Cargo test output capture
// ──────────────────────────────────────────────────────────────────────────────

/// Captured output from a cargo test execution.
///
/// This struct contains the complete output from running `cargo test`,
/// including stdout, stderr, exit code, and duration. It is designed to be
/// serializable for storage and transmission.
///
/// ## Example
///
/// ```no_run
/// use needle::types::OutputCapture;
/// use std::time::Duration;
///
/// let capture = OutputCapture {
///     stdout: "running 1 test\ntest test_foo ... ok".to_string(),
///     stderr: String::new(),
///     exit_code: Some(0),
///     duration: Duration::from_millis(1500),
/// };
///
/// assert!(capture.success());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputCapture {
    /// Captured stdout from the test execution.
    pub stdout: String,
    /// Captured stderr from the test execution.
    pub stderr: String,
    /// Exit code from the test process. `None` if killed by signal.
    pub exit_code: Option<i32>,
    /// Duration of the test execution.
    #[serde(with = "serde_duration")]
    pub duration: Duration,
}

impl OutputCapture {
    /// Returns true if the test exited with code 0.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// Returns true if the test failed (non-zero exit code).
    pub fn failed(&self) -> bool {
        !self.success()
    }

    /// Get the duration as milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.duration.as_millis() as u64
    }

    /// Get the total length of stdout and stderr in bytes.
    pub fn total_output_len(&self) -> usize {
        self.stdout.len() + self.stderr.len()
    }
}

/// Serde serialization module for Duration.
///
/// Duration is serialized as milliseconds (u64) for JSON compatibility.
mod serde_duration {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let millis = duration.as_millis() as u64;
        millis.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let millis = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(millis))
    }
}

/// A single compilation error detected from cargo test output.
///
/// Represents a Rust compiler error with code, message, and optional file location.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilationError {
    /// A Rust compiler error with error code (e.g., E0308).
    RustError {
        /// The error code (e.g., "E0308").
        code: String,
        /// The error message.
        message: String,
        /// Optional file path where the error occurred.
        file: Option<String>,
        /// Optional line number where the error occurred.
        line: Option<usize>,
        /// Optional column number where the error occurred.
        column: Option<usize>,
    },
    /// General compilation failure without specific error code.
    General {
        /// Description of the failure.
        message: String,
    },
    /// Abort message indicating the number of errors.
    Abort {
        /// Number of errors that caused the abort.
        error_count: usize,
    },
}

impl CompilationError {
    /// Returns a human-readable description of the error.
    pub fn description(&self) -> String {
        match self {
            CompilationError::RustError { code, message, .. } => {
                format!("{}: {}", code, message)
            }
            CompilationError::General { message } => message.clone(),
            CompilationError::Abort { error_count } => {
                format!("aborting due to {} previous error(s)", error_count)
            }
        }
    }

    /// Returns the error code if this is a Rust compiler error.
    pub fn error_code(&self) -> Option<&str> {
        match self {
            CompilationError::RustError { code, .. } => Some(code),
            _ => None,
        }
    }

    /// Returns true if this error has a file location.
    pub fn has_location(&self) -> bool {
        matches!(self, CompilationError::RustError { file: Some(_), .. })
    }

    /// Get the file location as "file:line:column" if available.
    pub fn location_string(&self) -> Option<String> {
        match self {
            CompilationError::RustError {
                file, line, column, ..
            } => {
                if let Some(f) = file {
                    let mut loc = f.clone();
                    if let Some(l) = line {
                        loc.push(':');
                        loc.push_str(&l.to_string());
                        if let Some(c) = column {
                            loc.push(':');
                            loc.push_str(&c.to_string());
                        }
                    }
                    Some(loc)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl fmt::Display for CompilationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompilationError::RustError {
                code,
                message,
                file: _,
                line: _,
                column: _,
            } => {
                if let Some(loc) = self.location_string() {
                    write!(f, "error[{}][{}]: {}", code, loc, message)
                } else {
                    write!(f, "error[{}]: {}", code, message)
                }
            }
            CompilationError::General { message } => write!(f, "{}", message),
            CompilationError::Abort { error_count } => {
                write!(f, "aborting due to {} previous error(s)", error_count)
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Label utilities
// ──────────────────────────────────────────────────────────────────────────────

/// Extract labels suitable for propagation to child or related beads.
///
/// Returns labels that represent project/domain context. Excludes ephemeral
/// state labels ("in-progress", "ready", "alert", "crash", "signal-*") that
/// are set per-bead by NEEDLE and would be inappropriate on a derived bead.
pub fn extract_stitch_labels(labels: &[String]) -> Vec<String> {
    const EXCLUDED: &[&str] = &[
        "alert",
        "crash",
        "in-progress",
        "ready",
        "blocked",
        "done",
        "closed",
    ];
    labels
        .iter()
        .filter(|l| {
            let lower = l.to_lowercase();
            !EXCLUDED.contains(&lower.as_str()) && !lower.starts_with("signal-")
        })
        .cloned()
        .collect()
}
