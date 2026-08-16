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
    /// `bf` (bead-forge) emits `"deferred"` for beads deliberately postponed
    /// rather than blocked by a dependency. Distinct from `Blocked`: a
    /// deferred bead has no unmet dependency, it was just set aside — see
    /// GitHub issue jedarden/NEEDLE#10. Without this variant, `bf list --json`
    /// fails deserialization for every such bead, and it silently disappears
    /// from strand/supervise visibility with no surfaced error.
    Deferred,
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
            BeadStatus::Deferred => write!(f, "deferred"),
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
// DecompositionMode
// ──────────────────────────────────────────────────────────────────────────────

/// Mode distinguishing timeout-driven decomposition from ordinary failure handling.
///
/// This enum defines the API boundary between two distinct failure-handling paths:
/// - **Timeout mode**: Activated when a bead times out. The bead is decomposed
///   into smaller child beads via `ChildBeadProposal`, each representing a
///   phase of the original work. This is a recovery strategy for work that
///   exceeded its time budget but may still be completable in smaller pieces.
/// - **OrdinaryFailure mode**: Activated for any non-timeout failure (compilation
///   error, test failure, agent crash, etc.). The bead is NOT decomposed.
///   It is either retried, released, or escalated based on the failure type
///   and consecutive failure count.
///
/// # When to use each mode
///
/// **Use `Timeout` mode when:**
/// - An agent process exits with code 124 (GNU timeout) or times out via another wrapper
/// - The bead has made progress but exceeded its time budget
/// - The work can be meaningfully split into discrete phases
/// - Child beads can complete the work with the same or reduced timeout
///
/// **Use `OrdinaryFailure` mode when:**
/// - Exit codes 1-123, 125-128 (failure codes) or 127 (agent not found)
/// - Exit codes >128 (signal crashes)
/// - Negative exit codes (abnormal termination)
/// - The failure is NOT time-related and splitting won't help
///
/// # API boundary
///
/// This enum is the sole discriminator for whether the decomposition path is
/// activated. No other code should check timeout status or exit codes to make
/// this decision — all decomposition logic should route through this enum.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecompositionMode {
    /// Timeout-driven decomposition mode.
    ///
    /// Contains timeout-specific context for use in generating child bead
    /// proposals (e.g., original timeout value, elapsed time, progress indicators).
    Timeout {
        /// The timeout duration that was exceeded (in seconds).
        timeout_seconds: u64,
        /// Optional hint about what phase was being worked on when timeout occurred.
        /// This can be used to name the first child bead (e.g., "Phase 1: Routing Rules").
        current_phase_hint: Option<String>,
    },
    /// Ordinary failure mode — no decomposition.
    ///
    /// Used for all non-timeout failures where splitting the bead into child
    /// beads would not help. The bead should be retried, released, or escalated
    /// via the normal failure-handling path.
    OrdinaryFailure,
}

impl DecompositionMode {
    /// Returns true if this is timeout decomposition mode.
    ///
    /// This is the primary query method for routing to the decomposition path.
    /// All code that needs to know whether to decompose should call this method
    /// rather than inspecting the enum directly.
    ///
    /// # Examples
    ///
    /// ```
    /// use needle::types::DecompositionMode;
    ///
    /// let mode = DecompositionMode::Timeout {
    ///     timeout_seconds: 120,
    ///     current_phase_hint: Some("Phase 1".to_string()),
    /// };
    /// assert!(mode.is_timeout());
    ///
    /// let mode = DecompositionMode::OrdinaryFailure;
    /// assert!(!mode.is_timeout());
    /// ```
    pub fn is_timeout(&self) -> bool {
        matches!(self, DecompositionMode::Timeout { .. })
    }

    /// Returns true if this is ordinary failure mode.
    ///
    /// This is the primary query method for routing to the normal failure
    /// handling path (retry, release, or escalate without decomposition).
    ///
    /// # Examples
    ///
    /// ```
    /// use needle::types::DecompositionMode;
    ///
    /// let mode = DecompositionMode::OrdinaryFailure;
    /// assert!(mode.is_ordinary_failure());
    ///
    /// let mode = DecompositionMode::Timeout {
    ///     timeout_seconds: 120,
    ///     current_phase_hint: None,
    /// };
    /// assert!(!mode.is_ordinary_failure());
    /// ```
    pub fn is_ordinary_failure(&self) -> bool {
        matches!(self, DecompositionMode::OrdinaryFailure)
    }

    /// Returns the timeout duration if in timeout mode, None otherwise.
    ///
    /// This is a convenience method for extracting the timeout value without
    /// pattern matching. Returns `None` for `OrdinaryFailure` mode.
    ///
    /// # Examples
    ///
    /// ```
    /// use needle::types::DecompositionMode;
    ///
    /// let mode = DecompositionMode::Timeout {
    ///     timeout_seconds: 120,
    ///     current_phase_hint: None,
    /// };
    /// assert_eq!(mode.timeout_seconds(), Some(120));
    ///
    /// let mode = DecompositionMode::OrdinaryFailure;
    /// assert_eq!(mode.timeout_seconds(), None);
    /// ```
    pub fn timeout_seconds(&self) -> Option<u64> {
        match self {
            DecompositionMode::Timeout {
                timeout_seconds, ..
            } => Some(*timeout_seconds),
            DecompositionMode::OrdinaryFailure => None,
        }
    }

    /// Returns the current phase hint if in timeout mode, None otherwise.
    ///
    /// This is a convenience method for extracting the optional phase hint
    /// without pattern matching. Returns `None` for `OrdinaryFailure` mode
    /// or when the hint is not set.
    ///
    /// # Examples
    ///
    /// ```
    /// use needle::types::DecompositionMode;
    ///
    /// let mode = DecompositionMode::Timeout {
    ///     timeout_seconds: 120,
    ///     current_phase_hint: Some("Phase 1".to_string()),
    /// };
    /// assert_eq!(mode.current_phase_hint(), Some(&"Phase 1".to_string()));
    ///
    /// let mode = DecompositionMode::Timeout {
    ///     timeout_seconds: 120,
    ///     current_phase_hint: None,
    /// };
    /// assert_eq!(mode.current_phase_hint(), None);
    /// ```
    pub fn current_phase_hint(&self) -> Option<&String> {
        match self {
            DecompositionMode::Timeout {
                current_phase_hint, ..
            } => current_phase_hint.as_ref(),
            DecompositionMode::OrdinaryFailure => None,
        }
    }
}

impl fmt::Display for DecompositionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecompositionMode::Timeout {
                timeout_seconds, ..
            } => write!(f, "timeout ({}s)", timeout_seconds),
            DecompositionMode::OrdinaryFailure => write!(f, "ordinary_failure"),
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

/// A bead dependency as returned from the `br`/`bf` JSON output, or from
/// bead-rs's `bead` (`{"blocker": "<id>", "kind": "blocks"}` — a lean
/// edge-only shape with no enriched title/status/priority). The aliases
/// below accept both without changing how bf/br JSON deserializes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrDependency {
    #[serde(alias = "blocker")]
    pub id: BeadId,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub priority: Priority,
    #[serde(rename = "dependency_type", alias = "kind", default)]
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

/// A comment on a bead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    /// Comment ID (internal database ID).
    pub id: i64,
    /// The bead this comment is on.
    #[serde(rename = "issue_id")]
    pub bead_id: String,
    /// The comment text.
    pub text: String,
    /// Author who created the comment.
    pub author: String,
    /// When the comment was created.
    pub created_at: DateTime<Utc>,
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
    /// Comments on this bead, returned by `bf show --json`.
    /// Empty array if no comments or backend doesn't support comments.
    #[serde(default)]
    pub comments: Vec<Comment>,
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
        let json = serde_json::to_string(&BeadStatus::Deferred).unwrap();
        assert_eq!(json, r#""deferred""#);
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
        assert!(!BeadStatus::Deferred.is_done());
    }

    #[test]
    fn bead_status_display() {
        assert_eq!(BeadStatus::Open.to_string(), "open");
        assert_eq!(BeadStatus::InProgress.to_string(), "in_progress");
        assert_eq!(BeadStatus::Done.to_string(), "done");
        assert_eq!(BeadStatus::Closed.to_string(), "done"); // Closed displays as done
        assert_eq!(BeadStatus::Blocked.to_string(), "blocked");
        assert_eq!(BeadStatus::Deferred.to_string(), "deferred");
    }

    #[test]
    fn bead_status_deferred_deserialization() {
        // bf (bead-forge) emits "deferred" for beads deliberately postponed —
        // GitHub issue jedarden/NEEDLE#10. Previously this failed deserialization
        // and silently dropped the bead from every strand/supervise view.
        let status: BeadStatus = serde_json::from_str(r#""deferred""#).unwrap();
        assert_eq!(status, BeadStatus::Deferred);
        assert!(!status.is_done());
        // Round-trip: serializing back must produce the same wire format bf expects.
        assert_eq!(serde_json::to_string(&status).unwrap(), r#""deferred""#);
    }

    #[test]
    fn bead_status_deferred_distinct_from_blocked() {
        // Deferred (deliberately postponed) and Blocked (unmet dependency) are
        // different states, not aliases of one another.
        assert_ne!(BeadStatus::Deferred, BeadStatus::Blocked);
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

    // ──────────────────────────────────────────────────────────────────────────────
    // ErrorType tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn error_type_display_compile() {
        assert_eq!(ErrorType::Compile.to_string(), "compile");
    }

    #[test]
    fn error_type_display_test() {
        assert_eq!(ErrorType::Test.to_string(), "test");
    }

    #[test]
    fn error_type_display_unknown() {
        assert_eq!(ErrorType::Unknown.to_string(), "unknown");
    }

    #[test]
    fn error_type_all_variants_display() {
        let types = vec![ErrorType::Compile, ErrorType::Test, ErrorType::Unknown];
        for error_type in types {
            let display = format!("{}", error_type);
            assert!(!display.is_empty());
        }
    }

    #[test]
    fn error_type_serde_compile() {
        let json = serde_json::to_string(&ErrorType::Compile).unwrap();
        assert_eq!(json, r#""compile""#);
        let parsed: ErrorType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ErrorType::Compile);
    }

    #[test]
    fn error_type_serde_test() {
        let json = serde_json::to_string(&ErrorType::Test).unwrap();
        assert_eq!(json, r#""test""#);
        let parsed: ErrorType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ErrorType::Test);
    }

    #[test]
    fn error_type_serde_unknown() {
        let json = serde_json::to_string(&ErrorType::Unknown).unwrap();
        assert_eq!(json, r#""unknown""#);
        let parsed: ErrorType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ErrorType::Unknown);
    }

    #[test]
    fn error_type_equality() {
        assert_eq!(ErrorType::Compile, ErrorType::Compile);
        assert_eq!(ErrorType::Test, ErrorType::Test);
        assert_eq!(ErrorType::Unknown, ErrorType::Unknown);
        assert_ne!(ErrorType::Compile, ErrorType::Test);
        assert_ne!(ErrorType::Compile, ErrorType::Unknown);
        assert_ne!(ErrorType::Test, ErrorType::Unknown);
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // detect_compilation_errors tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn detect_compilation_errors_empty_stderr() {
        let stderr = "";
        let errors = detect_compilation_errors(stderr);
        assert!(errors.is_empty());
    }

    #[test]
    fn detect_compilation_errors_single_error_with_code() {
        let stderr = "error[E0308]: mismatched types";
        let errors = detect_compilation_errors(stderr);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            CompilationError::RustError {
                code,
                message,
                file,
                line,
                column,
            } => {
                assert_eq!(code, "E0308");
                assert_eq!(message, "mismatched types");
                assert!(file.is_none());
                assert!(line.is_none());
                assert!(column.is_none());
            }
            _ => panic!("Expected RustError variant"),
        }
    }

    #[test]
    fn detect_compilation_errors_error_with_location() {
        let stderr = r#"error[E0308]: mismatched types
  --> src/main.rs:10:5"#;
        let errors = detect_compilation_errors(stderr);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            CompilationError::RustError {
                code,
                message,
                file,
                line,
                column,
            } => {
                assert_eq!(code, "E0308");
                assert_eq!(message, "mismatched types");
                assert_eq!(file, &Some("src/main.rs".to_string()));
                assert_eq!(line, &Some(10));
                assert_eq!(column, &Some(5));
            }
            _ => panic!("Expected RustError variant"),
        }
    }

    #[test]
    fn detect_compilation_errors_multiple_errors() {
        let stderr = r#"error[E0308]: mismatched types
  --> src/main.rs:10:5
error[E0382]: use of moved value
  --> src/lib.rs:42:10"#;
        let errors = detect_compilation_errors(stderr);
        assert_eq!(errors.len(), 2);
        match &errors[0] {
            CompilationError::RustError { code, .. } => {
                assert_eq!(code, "E0308");
            }
            _ => panic!("Expected RustError variant"),
        }
        match &errors[1] {
            CompilationError::RustError { code, .. } => {
                assert_eq!(code, "E0382");
            }
            _ => panic!("Expected RustError variant"),
        }
    }

    #[test]
    fn detect_compilation_errors_could_not_compile() {
        let stderr = "error: could not compile `my_crate` (bin \"my_crate\")";
        let errors = detect_compilation_errors(stderr);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            CompilationError::General { message } => {
                assert!(message.contains("my_crate"));
            }
            _ => panic!("Expected General variant"),
        }
    }

    #[test]
    fn detect_compilation_errors_abort_message() {
        let stderr = "error: aborting due to 3 previous errors";
        let errors = detect_compilation_errors(stderr);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            CompilationError::Abort { error_count } => {
                assert_eq!(*error_count, 3);
            }
            _ => panic!("Expected Abort variant"),
        }
    }

    #[test]
    fn detect_compilation_errors_full_compilation_output() {
        let stderr = r#"   Compiling my_crate v0.1.0 (/path/to/crate)
error[E0308]: mismatched types
  --> src/main.rs:10:5
   |
10 |     let x: i32 = "hello";
   |            ---   ^^^^^^^ expected `i32`, found `&str`
   |            expected due to this

error: aborting due to 1 previous error

error: could not compile `my_crate` (bin \"my_crate\)
"#;
        let errors = detect_compilation_errors(stderr);
        assert!(!errors.is_empty(), "Should detect at least one error");

        // Should have RustError, Abort, and General errors
        let has_rust_error = errors
            .iter()
            .any(|e| matches!(e, CompilationError::RustError { .. }));
        let has_abort = errors
            .iter()
            .any(|e| matches!(e, CompilationError::Abort { .. }));
        let has_general = errors
            .iter()
            .any(|e| matches!(e, CompilationError::General { .. }));

        assert!(has_rust_error, "Should have RustError");
        assert!(has_abort, "Should have Abort");
        assert!(has_general, "Should have General");
    }

    #[test]
    fn detect_compilation_errors_test_output_only() {
        // Test output without compilation errors
        let stderr = "running 3 tests\ntest test_foo ... ok\ntest test_bar ... FAILED\n";
        let errors = detect_compilation_errors(stderr);
        assert!(
            errors.is_empty(),
            "Should not detect compilation errors in test output"
        );
    }

    #[test]
    fn detect_compilation_errors_unused_warnings() {
        let stderr = "error: unused variable: `x`\nerror: dead_code";
        let errors = detect_compilation_errors(stderr);
        assert_eq!(errors.len(), 2);
        assert!(errors
            .iter()
            .all(|e| matches!(e, CompilationError::General { .. })));
    }

    #[test]
    fn detect_compilation_errors_mixed_output() {
        let stderr = r#"warning: unused variable
error[E0308]: mismatched types
  --> src/main.rs:10:5
running tests
test foo ... ok"#;
        let errors = detect_compilation_errors(stderr);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            CompilationError::RustError { code, .. } => {
                assert_eq!(code, "E0308");
            }
            _ => panic!("Expected RustError variant"),
        }
    }

    #[test]
    fn detect_compilation_errors_location_no_column() {
        let stderr = r#"error[E0308]: mismatched types
  --> src/main.rs:10"#;
        let errors = detect_compilation_errors(stderr);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            CompilationError::RustError {
                code,
                file,
                line,
                column,
                ..
            } => {
                assert_eq!(code, "E0308");
                assert_eq!(file, &Some("src/main.rs".to_string()));
                assert_eq!(line, &Some(10));
                assert!(column.is_none());
            }
            _ => panic!("Expected RustError variant"),
        }
    }

    #[test]
    fn detect_compilation_errors_location_no_line_or_column() {
        let stderr = r#"error[E0308]: mismatched types
  --> src/main.rs"#;
        let errors = detect_compilation_errors(stderr);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            CompilationError::RustError {
                code,
                file,
                line,
                column,
                ..
            } => {
                assert_eq!(code, "E0308");
                assert_eq!(file, &Some("src/main.rs".to_string()));
                assert!(line.is_none());
                assert!(column.is_none());
            }
            _ => panic!("Expected RustError variant"),
        }
    }

    #[test]
    fn parse_error_line_valid_format() {
        let line = "error[E0308]: mismatched types";
        let result = parse_error_line(line);
        assert!(result.is_some());
        let (code, message) = result.unwrap();
        assert_eq!(code, "E0308");
        assert_eq!(message, "mismatched types");
    }

    #[test]
    fn parse_error_line_no_message() {
        let line = "error[E0308]:";
        let result = parse_error_line(line);
        assert!(result.is_some());
        let (code, message) = result.unwrap();
        assert_eq!(code, "E0308");
        assert!(message.is_empty());
    }

    #[test]
    fn parse_error_line_not_an_error() {
        let line = "warning: unused variable";
        let result = parse_error_line(line);
        assert!(result.is_none());
    }

    #[test]
    fn parse_location_line_full_location() {
        let line = "   --> src/main.rs:10:5";
        let result = parse_location_line(line);
        assert!(result.is_some());
        let (file, line_num, column) = result.unwrap();
        assert_eq!(file, "src/main.rs");
        assert_eq!(line_num, Some(10));
        assert_eq!(column, Some(5));
    }

    #[test]
    fn parse_location_line_no_column() {
        let line = "   --> src/main.rs:10";
        let result = parse_location_line(line);
        assert!(result.is_some());
        let (file, line_num, column) = result.unwrap();
        assert_eq!(file, "src/main.rs");
        assert_eq!(line_num, Some(10));
        assert!(column.is_none());
    }

    #[test]
    fn parse_location_line_no_line_or_column() {
        let line = "   --> src/main.rs";
        let result = parse_location_line(line);
        assert!(result.is_some());
        let (file, line_num, column) = result.unwrap();
        assert_eq!(file, "src/main.rs");
        assert!(line_num.is_none());
        assert!(column.is_none());
    }

    #[test]
    fn parse_location_line_not_a_location() {
        let line = "   some other text";
        let result = parse_location_line(line);
        assert!(result.is_none());
    }

    #[test]
    fn parse_abort_count_valid() {
        let line = "aborting due to 3 previous errors";
        let result = parse_abort_count(line);
        assert_eq!(result, Some(3));
    }

    #[test]
    fn parse_abort_count_single_digit() {
        let line = "aborting due to 5 previous errors";
        let result = parse_abort_count(line);
        assert_eq!(result, Some(5));
    }

    #[test]
    fn parse_abort_count_no_number() {
        let line = "aborting due to previous errors";
        let result = parse_abort_count(line);
        assert!(result.is_none());
    }

    #[test]
    fn parse_abort_count_large_number() {
        let line = "aborting due to 42 previous errors";
        let result = parse_abort_count(line);
        assert_eq!(result, Some(42));
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // ChildBeadProposal tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn child_bead_proposal_new_valid() {
        let proposal = ChildBeadProposal::new(
            "Phase 1: Core".to_string(),
            "Implement core functionality".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        );
        assert!(proposal.is_ok());
        let proposal = proposal.unwrap();
        assert_eq!(proposal.phase_title, "Phase 1: Core");
        assert_eq!(proposal.description, "Implement core functionality");
        assert_eq!(proposal.dependencies.len(), 1);
        assert_eq!(proposal.priority, 2);
        assert!(proposal.labels.is_empty());
    }

    #[test]
    fn child_bead_proposal_new_empty_title() {
        let proposal = ChildBeadProposal::new(
            "".to_string(),
            "Implement core functionality".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        );
        assert!(proposal.is_err());
        assert_eq!(proposal.unwrap_err(), "phase_title cannot be empty");
    }

    #[test]
    fn child_bead_proposal_new_whitespace_title() {
        let proposal = ChildBeadProposal::new(
            "   ".to_string(),
            "Implement core functionality".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        );
        assert!(proposal.is_err());
        assert_eq!(proposal.unwrap_err(), "phase_title cannot be empty");
    }

    #[test]
    fn child_bead_proposal_new_empty_description() {
        let proposal = ChildBeadProposal::new(
            "Phase 1: Core".to_string(),
            "".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        );
        assert!(proposal.is_err());
        assert_eq!(proposal.unwrap_err(), "description cannot be empty");
    }

    #[test]
    fn child_bead_proposal_new_empty_dependencies() {
        let proposal = ChildBeadProposal::new(
            "Phase 1: Core".to_string(),
            "Implement core functionality".to_string(),
            vec![],
            2,
        );
        assert!(proposal.is_err());
        assert_eq!(
            proposal.unwrap_err(),
            "dependencies must contain at least one parent bead ID"
        );
    }

    #[test]
    fn child_bead_proposal_new_invalid_priority() {
        let proposal = ChildBeadProposal::new(
            "Phase 1: Core".to_string(),
            "Implement core functionality".to_string(),
            vec![BeadId::from("needle-parent")],
            5,
        );
        assert!(proposal.is_err());
        assert_eq!(proposal.unwrap_err(), "priority must be between 0 and 4");
    }

    #[test]
    fn child_bead_proposal_with_labels() {
        let proposal = ChildBeadProposal::new(
            "Phase 1: Core".to_string(),
            "Implement core functionality".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        )
        .unwrap()
        .with_labels(vec!["phase-1".to_string(), "core".to_string()]);

        assert_eq!(proposal.labels.len(), 2);
        assert_eq!(proposal.labels[0], "phase-1");
        assert_eq!(proposal.labels[1], "core");
    }

    #[test]
    fn child_bead_proposal_validate_success() {
        let proposal = ChildBeadProposal::new(
            "Phase 1: Core".to_string(),
            "Implement core functionality".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        )
        .unwrap();

        assert!(proposal.validate().is_ok());
    }

    #[test]
    fn child_bead_proposal_validate_empty_title() {
        let mut proposal = ChildBeadProposal::new(
            "Phase 1: Core".to_string(),
            "Implement core functionality".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        )
        .unwrap();
        proposal.phase_title = "".to_string();

        assert!(proposal.validate().is_err());
        assert_eq!(proposal.validate().unwrap_err(), "phase_title cannot be empty");
    }

    #[test]
    fn child_bead_proposal_validate_empty_description() {
        let mut proposal = ChildBeadProposal::new(
            "Phase 1: Core".to_string(),
            "Implement core functionality".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        )
        .unwrap();
        proposal.description = "".to_string();

        assert!(proposal.validate().is_err());
        assert_eq!(proposal.validate().unwrap_err(), "description cannot be empty");
    }

    #[test]
    fn child_bead_proposal_validate_empty_dependencies() {
        let mut proposal = ChildBeadProposal::new(
            "Phase 1: Core".to_string(),
            "Implement core functionality".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        )
        .unwrap();
        proposal.dependencies = vec![];

        assert!(proposal.validate().is_err());
        assert_eq!(
            proposal.validate().unwrap_err(),
            "dependencies must contain at least one parent bead ID"
        );
    }

    #[test]
    fn child_bead_proposal_validate_invalid_priority() {
        let mut proposal = ChildBeadProposal::new(
            "Phase 1: Core".to_string(),
            "Implement core functionality".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        )
        .unwrap();
        proposal.priority = 10;

        assert!(proposal.validate().is_err());
        assert_eq!(proposal.validate().unwrap_err(), "priority must be between 0 and 4");
    }

    #[test]
    fn child_bead_proposal_multiple_dependencies() {
        let proposal = ChildBeadProposal::new(
            "Phase 2: Integration".to_string(),
            "Integrate with external systems".to_string(),
            vec![
                BeadId::from("needle-phase1"),
                BeadId::from("needle-dep1"),
                BeadId::from("needle-dep2"),
            ],
            1,
        )
        .unwrap();

        assert_eq!(proposal.dependencies.len(), 3);
        assert_eq!(proposal.priority, 1);
    }

    #[test]
    fn child_bead_proposal_serde_roundtrip() {
        let proposal = ChildBeadProposal::new(
            "Phase 1: Core".to_string(),
            "Implement core functionality".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        )
        .unwrap()
        .with_labels(vec!["phase-1".to_string()]);

        let json = serde_json::to_string(&proposal).unwrap();
        let deserialized: ChildBeadProposal = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.phase_title, proposal.phase_title);
        assert_eq!(deserialized.description, proposal.description);
        assert_eq!(deserialized.dependencies, proposal.dependencies);
        assert_eq!(deserialized.priority, proposal.priority);
        assert_eq!(deserialized.labels, proposal.labels);
    }

    #[test]
    fn child_bead_proposal_priority_boundary_values() {
        // Test priority 0 (highest priority)
        let p0 = ChildBeadProposal::new(
            "Phase 1".to_string(),
            "Description".to_string(),
            vec![BeadId::from("needle-parent")],
            0,
        );
        assert!(p0.is_ok());

        // Test priority 4 (lowest priority)
        let p4 = ChildBeadProposal::new(
            "Phase 1".to_string(),
            "Description".to_string(),
            vec![BeadId::from("needle-parent")],
            4,
        );
        assert!(p4.is_ok());

        // Test priority 5 (invalid)
        let p5 = ChildBeadProposal::new(
            "Phase 1".to_string(),
            "Description".to_string(),
            vec![BeadId::from("needle-parent")],
            5,
        );
        assert!(p5.is_err());
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // DecompositionMode tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn decomposition_mode_timeout_is_timeout() {
        let mode = DecompositionMode::Timeout {
            timeout_seconds: 120,
            current_phase_hint: Some("Phase 1".to_string()),
        };
        assert!(mode.is_timeout());
        assert!(!mode.is_ordinary_failure());
    }

    #[test]
    fn decomposition_mode_ordinary_failure_is_not_timeout() {
        let mode = DecompositionMode::OrdinaryFailure;
        assert!(!mode.is_timeout());
        assert!(mode.is_ordinary_failure());
    }

    #[test]
    fn decomposition_mode_timeout_with_hint() {
        let mode = DecompositionMode::Timeout {
            timeout_seconds: 300,
            current_phase_hint: Some("Phase 2: Analysis".to_string()),
        };
        assert!(mode.is_timeout());
        assert_eq!(mode.timeout_seconds(), Some(300));
        assert_eq!(
            mode.current_phase_hint(),
            Some(&"Phase 2: Analysis".to_string())
        );
    }

    #[test]
    fn decomposition_mode_timeout_without_hint() {
        let mode = DecompositionMode::Timeout {
            timeout_seconds: 60,
            current_phase_hint: None,
        };
        assert!(mode.is_timeout());
        assert_eq!(mode.timeout_seconds(), Some(60));
        assert_eq!(mode.current_phase_hint(), None);
    }

    #[test]
    fn decomposition_mode_ordinary_failure_no_timeout() {
        let mode = DecompositionMode::OrdinaryFailure;
        assert_eq!(mode.timeout_seconds(), None);
        assert_eq!(mode.current_phase_hint(), None);
    }

    #[test]
    fn decomposition_mode_timeout_seconds_zero() {
        let mode = DecompositionMode::Timeout {
            timeout_seconds: 0,
            current_phase_hint: None,
        };
        assert_eq!(mode.timeout_seconds(), Some(0));
        assert!(mode.is_timeout());
    }

    #[test]
    fn decomposition_mode_timeout_seconds_large() {
        let mode = DecompositionMode::Timeout {
            timeout_seconds: 86400, // 1 day
            current_phase_hint: None,
        };
        assert_eq!(mode.timeout_seconds(), Some(86400));
        assert!(mode.is_timeout());
    }

    #[test]
    fn decomposition_mode_display_timeout() {
        let mode = DecompositionMode::Timeout {
            timeout_seconds: 120,
            current_phase_hint: Some("Phase 1".to_string()),
        };
        assert_eq!(mode.to_string(), "timeout (120s)");
    }

    #[test]
    fn decomposition_mode_display_ordinary_failure() {
        let mode = DecompositionMode::OrdinaryFailure;
        assert_eq!(mode.to_string(), "ordinary_failure");
    }

    #[test]
    fn decomposition_mode_equality_timeout() {
        let mode1 = DecompositionMode::Timeout {
            timeout_seconds: 120,
            current_phase_hint: Some("Phase 1".to_string()),
        };
        let mode2 = DecompositionMode::Timeout {
            timeout_seconds: 120,
            current_phase_hint: Some("Phase 1".to_string()),
        };
        assert_eq!(mode1, mode2);
    }

    #[test]
    fn decomposition_mode_inequality_different_timeout() {
        let mode1 = DecompositionMode::Timeout {
            timeout_seconds: 120,
            current_phase_hint: None,
        };
        let mode2 = DecompositionMode::Timeout {
            timeout_seconds: 300,
            current_phase_hint: None,
        };
        assert_ne!(mode1, mode2);
    }

    #[test]
    fn decomposition_mode_inequality_timeout_vs_ordinary() {
        let timeout_mode = DecompositionMode::Timeout {
            timeout_seconds: 120,
            current_phase_hint: None,
        };
        let ordinary_mode = DecompositionMode::OrdinaryFailure;
        assert_ne!(timeout_mode, ordinary_mode);
    }

    #[test]
    fn decomposition_mode_serde_timeout_roundtrip() {
        let mode = DecompositionMode::Timeout {
            timeout_seconds: 180,
            current_phase_hint: Some("Test Phase".to_string()),
        };

        let json = serde_json::to_string(&mode).unwrap();
        assert!(json.contains("timeout"));
        assert!(json.contains("180"));

        let deserialized: DecompositionMode = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, mode);
        assert_eq!(deserialized.timeout_seconds(), Some(180));
        assert_eq!(
            deserialized.current_phase_hint(),
            Some(&"Test Phase".to_string())
        );
    }

    #[test]
    fn decomposition_mode_serde_ordinary_failure_roundtrip() {
        let mode = DecompositionMode::OrdinaryFailure;

        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, r#""ordinary_failure""#);

        let deserialized: DecompositionMode = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, mode);
        assert!(deserialized.is_ordinary_failure());
    }

    #[test]
    fn decomposition_mode_serde_timeout_without_hint() {
        let mode = DecompositionMode::Timeout {
            timeout_seconds: 60,
            current_phase_hint: None,
        };

        let json = serde_json::to_string(&mode).unwrap();
        let deserialized: DecompositionMode = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized, mode);
        assert_eq!(deserialized.timeout_seconds(), Some(60));
        assert_eq!(deserialized.current_phase_hint(), None);
    }

    #[test]
    fn decomposition_mode_current_phase_hint_empty_string() {
        let mode = DecompositionMode::Timeout {
            timeout_seconds: 120,
            current_phase_hint: Some("".to_string()),
        };
        // Empty string is still Some, just empty
        assert_eq!(mode.current_phase_hint(), Some(&"".to_string()));
    }

    #[test]
    fn decomposition_mode_all_variants_have_display() {
        let timeout = DecompositionMode::Timeout {
            timeout_seconds: 120,
            current_phase_hint: None,
        };
        let ordinary = DecompositionMode::OrdinaryFailure;

        // Should not panic
        let _ = format!("{}", timeout);
        let _ = format!("{}", ordinary);
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
    /// Bead was quarantined (status=blocked, labeled `cycling`) after
    /// exceeding the consecutive-failure threshold.
    Quarantined,
    /// No action taken (e.g., success with bead already closed).
    None,
}

impl fmt::Display for BeadAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BeadAction::Released => write!(f, "released"),
            BeadAction::Deferred => write!(f, "deferred"),
            BeadAction::Alerted => write!(f, "alerted"),
            BeadAction::Quarantined => write!(f, "quarantined"),
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
// ChildBeadProposal
// ──────────────────────────────────────────────────────────────────────────────

/// A proposal for a child bead to be created during timeout decomposition.
///
/// This structure captures all the information needed to propose a new child bead
/// when a parent bead times out and needs to be decomposed into smaller phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildBeadProposal {
    /// Title of the phase (e.g., "Phase 1: Core Implementation").
    pub phase_title: String,
    /// Detailed description of what this phase accomplishes.
    pub description: String,
    /// Parent bead IDs that this child depends on.
    pub dependencies: Vec<BeadId>,
    /// Priority level (lower number = higher priority).
    pub priority: Priority,
    /// Labels to apply to the child bead.
    #[serde(default)]
    pub labels: Vec<String>,
}

impl ChildBeadProposal {
    /// Create a new child bead proposal with validation.
    ///
    /// # Arguments
    ///
    /// * `phase_title` - Title of the phase
    /// * `description` - Detailed description of the phase
    /// * `dependencies` - Parent bead IDs this child depends on
    /// * `priority` - Priority level (0-4)
    ///
    /// # Returns
    ///
    /// * `Ok(ChildBeadProposal)` if validation passes
    /// * `Err(String)` if validation fails
    ///
    /// # Examples
    ///
    /// ```
    /// use needle::types::{ChildBeadProposal, BeadId, Priority};
    ///
    /// let proposal = ChildBeadProposal::new(
    ///     "Phase 1: Core".to_string(),
    ///     "Implement core functionality".to_string(),
    ///     vec![BeadId::from("needle-parent")],
    ///     2,
    /// ).unwrap();
    /// ```
    pub fn new(
        phase_title: String,
        description: String,
        dependencies: Vec<BeadId>,
        priority: Priority,
    ) -> Result<Self, String> {
        // Validate required fields
        if phase_title.trim().is_empty() {
            return Err("phase_title cannot be empty".to_string());
        }
        if description.trim().is_empty() {
            return Err("description cannot be empty".to_string());
        }
        if dependencies.is_empty() {
            return Err("dependencies must contain at least one parent bead ID".to_string());
        }
        if priority > 4 {
            return Err("priority must be between 0 and 4".to_string());
        }

        Ok(ChildBeadProposal {
            phase_title,
            description,
            dependencies,
            priority,
            labels: Vec::new(),
        })
    }

    /// Add labels to the proposal.
    pub fn with_labels(mut self, labels: Vec<String>) -> Self {
        self.labels = labels;
        self
    }

    /// Validate that all required fields are present and well-formed.
    pub fn validate(&self) -> Result<(), String> {
        if self.phase_title.trim().is_empty() {
            return Err("phase_title cannot be empty".to_string());
        }
        if self.description.trim().is_empty() {
            return Err("description cannot be empty".to_string());
        }
        if self.dependencies.is_empty() {
            return Err("dependencies must contain at least one parent bead ID".to_string());
        }
        if self.priority > 4 {
            return Err("priority must be between 0 and 4".to_string());
        }
        Ok(())
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

/// Classification of error types in the build/test pipeline.
///
/// Used to categorize errors by their source and recovery path.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    /// Error from compiling Rust code (cargo build / cargo test --no-run).
    Compile,
    /// Error from running tests (cargo test execution failure).
    Test,
    /// Error type could not be determined.
    Unknown,
}

impl fmt::Display for ErrorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorType::Compile => write!(f, "compile"),
            ErrorType::Test => write!(f, "test"),
            ErrorType::Unknown => write!(f, "unknown"),
        }
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
// Error Code Classification
// ──────────────────────────────────────────────────────────────────────────────

/// Category of a Rust compiler error code.
///
/// Rust error codes (E0XXX) are categorized by their error type to enable
/// better error handling and reporting.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    /// Type mismatch errors (E0308, E0309, etc.)
    TypeMismatch,
    /// Borrow checker and lifetime errors (E0382, E0502, E0507, etc.)
    BorrowChecker,
    /// Missing or incorrect trait implementation (E0038, E0046, etc.)
    TraitImpl,
    /// Pattern matching errors (E0002, E0009, etc.)
    PatternMatching,
    /// Scope and visibility errors (E0403, E0412, E0603, etc.)
    ScopeVisibility,
    /// Syntax errors (E0053, E0063, E0263, etc.)
    Syntax,
    /// Generic and const parameter errors (E0207, E0392, E0408, etc.)
    Generic,
    /// Macro expansion errors (E0276, E0519, E0704, etc.)
    Macro,
    /// Dead code or unused item warnings treated as errors (E0382, E0425, E0526)
    DeadCode,
    /// Error code without a standard mapping.
    Unknown,
}

impl ErrorCategory {
    /// Get a human-readable description of the category.
    pub fn description(&self) -> &'static str {
        match self {
            ErrorCategory::TypeMismatch => "type mismatch or conversion error",
            ErrorCategory::BorrowChecker => "borrow checker or lifetime error",
            ErrorCategory::TraitImpl => "trait implementation or bound error",
            ErrorCategory::PatternMatching => "pattern matching error",
            ErrorCategory::ScopeVisibility => "scope or visibility error",
            ErrorCategory::Syntax => "syntax error",
            ErrorCategory::Generic => "generic or const parameter error",
            ErrorCategory::Macro => "macro expansion error",
            ErrorCategory::DeadCode => "dead code or unused item",
            ErrorCategory::Unknown => "unknown error category",
        }
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorCategory::TypeMismatch => write!(f, "type_mismatch"),
            ErrorCategory::BorrowChecker => write!(f, "borrow_checker"),
            ErrorCategory::TraitImpl => write!(f, "trait_impl"),
            ErrorCategory::PatternMatching => write!(f, "pattern_matching"),
            ErrorCategory::ScopeVisibility => write!(f, "scope_visibility"),
            ErrorCategory::Syntax => write!(f, "syntax"),
            ErrorCategory::Generic => write!(f, "generic"),
            ErrorCategory::Macro => write!(f, "macro"),
            ErrorCategory::DeadCode => write!(f, "dead_code"),
            ErrorCategory::Unknown => write!(f, "unknown"),
        }
    }
}

/// Parse and validate a Rust error code from a string.
///
/// This function extracts and validates error codes in the format E0XXX
/// (where XXX are digits). Returns `None` if the format is invalid.
///
/// # Arguments
///
/// * `input` - String that may contain an error code
///
/// # Returns
///
/// * `Some(code)` - The extracted error code if valid (e.g., "E0308")
/// * `None` - If no valid error code is found
///
/// # Examples
///
/// ```
/// use needle::types::parse_error_code;
///
/// assert_eq!(parse_error_code("E0308"), Some("E0308".to_string()));
/// assert_eq!(parse_error_code("error[E0308]:"), Some("E0308".to_string()));
/// assert_eq!(parse_error_code("E12345"), None); // Wrong format
/// assert_eq!(parse_error_code("E0ABC"), None); // Non-digit characters
/// ```
pub fn parse_error_code(input: &str) -> Option<String> {
    // Find the error code pattern: E followed by exactly 4 digits
    let re = regex::Regex::new(r"E(\d{4})").ok()?;
    if let Some(captures) = re.captures(input) {
        if let Some(code) = captures.get(0) {
            let code_str = code.as_str();
            // Verify it's the E0XXX format (first digit should be 0)
            if code_str.len() == 5 && code_str.starts_with("E0") {
                return Some(code_str.to_string());
            }
        }
    }
    None
}

/// Classify a Rust error code into an error category.
///
/// Maps known error codes to their semantic categories. Returns
/// `ErrorCategory::Unknown` for codes without standard mappings.
///
/// # Arguments
///
/// * `code` - The error code (e.g., "E0308")
///
/// # Returns
///
/// The corresponding `ErrorCategory` for this error code.
///
/// # Examples
///
/// ```
/// use needle::types::{classify_error_code, ErrorCategory};
///
/// assert_eq!(classify_error_code("E0308"), ErrorCategory::TypeMismatch);
/// assert_eq!(classify_error_code("E0382"), ErrorCategory::BorrowChecker);
/// assert_eq!(classify_error_code("E9999"), ErrorCategory::Unknown);
/// ```
pub fn classify_error_code(code: &str) -> ErrorCategory {
    match code {
        // Type mismatch errors
        "E0308" | "E0309" | "E0310" | "E0311" | "E0312" | "E0313" | "E0314" | "E0315" | "E0316"
        | "E0317" | "E0369" | "E0370" => ErrorCategory::TypeMismatch,

        // Borrow checker and lifetime errors
        "E0382" | "E0502" | "E0503" | "E0505" | "E0506" | "E0507" | "E0508" | "E0509" | "E0510"
        | "E0511" | "E0512" | "E0515" | "E0516" | "E0517" | "E0597" | "E0623" | "E0624"
        | "E0625" | "E0626" | "E0716" | "E0782" | "E0783" | "E0937" | "E0980" => {
            ErrorCategory::BorrowChecker
        }

        // Trait implementation errors
        "E0038" | "E0046" | "E0117" | "E0118" | "E0119" | "E0120" | "E0183" | "E0207" | "E0210"
        | "E0220" | "E0227" | "E0229" | "E0230" | "E0277" | "E0365" | "E0366" | "E0367"
        | "E0368" | "E0381" | "E0390" | "E0391" | "E0412" | "E0423" | "E0437" | "E0558"
        | "E0574" | "E0647" | "E0699" | "E0708" | "E0719" | "E0781" => ErrorCategory::TraitImpl,

        // Pattern matching errors
        "E0002" | "E0009" | "E0007" | "E0010" | "E0011" | "E0012" | "E0013" | "E0014" | "E0015"
        | "E0016" | "E0017" | "E0018" | "E0019" | "E0022" | "E0023" | "E0024" | "E0025"
        | "E0026" | "E0031" | "E0033" | "E0034" | "E0035" | "E0039" | "E0040" | "E0044"
        | "E0052" | "E0054" | "E0055" | "E0162" | "E0163" | "E0164" | "E0165" | "E0302"
        | "E0409" | "E0422" | "E0424" | "E0513" | "E0529" | "E0616" | "E0617" | "E0618"
        | "E0639" | "E0640" | "E0641" | "E0642" | "E0643" | "E0644" => {
            ErrorCategory::PatternMatching
        }

        // Scope and visibility errors
        "E0403" | "E0404" | "E0405" | "E0406" | "E0407" | "E0408" | "E0411" | "E0413" | "E0414"
        | "E0415" | "E0501" | "E0583" | "E0603" | "E0604" | "E0605" | "E0606" | "E0607"
        | "E0608" | "E0609" | "E0610" | "E0611" | "E0612" | "E0613" | "E0614" | "E0615"
        | "E0621" | "E0622" | "E0631" | "E0633" | "E0634" | "E0636" | "E0742" | "E0743"
        | "E0744" | "E0745" | "E0750" | "E0758" | "E0759" | "E0760" | "E0761" | "E0762"
        | "E0763" | "E0764" | "E0765" | "E0766" | "E0767" | "E0768" | "E0769" | "E0770"
        | "E0771" | "E0772" | "E0773" | "E0774" | "E0775" | "E0776" | "E0777" | "E0778"
        | "E0779" | "E0780" | "E0790" | "E0791" | "E0792" | "E0793" | "E0794" | "E0795"
        | "E0796" | "E0797" | "E0798" | "E0799" => ErrorCategory::ScopeVisibility,

        // Syntax errors
        "E0053" | "E0060" | "E0061" | "E0062" | "E0063" | "E0066" | "E0070" | "E0071" | "E0072"
        | "E0073" | "E0075" | "E0076" | "E0077" | "E0078" | "E0079" | "E0080" | "E0081"
        | "E0082" | "E0085" | "E0087" | "E0106" | "E0116" | "E0124" | "E0131" | "E0133"
        | "E0161" | "E0175" | "E0201" | "E0204" | "E0205" | "E0206" | "E0211" | "E0214"
        | "E0225" | "E0226" | "E0231" | "E0254" | "E0255" | "E0256" | "E0257" | "E0258"
        | "E0259" | "E0260" | "E0261" | "E0262" | "E0263" | "E0264" | "E0267" | "E0268"
        | "E0275" | "E0281" | "E0282" | "E0301" | "E0306" | "E0324" | "E0328" | "E0378"
        | "E0379" | "E0401" | "E0402" | "E0428" | "E0430" | "E0433" | "E0434" | "E0435"
        | "E0436" | "E0438" | "E0439" | "E0440" | "E0441" | "E0442" | "E0443" | "E0444"
        | "E0445" | "E0446" | "E0447" | "E0448" | "E0449" | "E0450" | "E0451" | "E0452"
        | "E0453" | "E0454" | "E0455" | "E0456" | "E0457" | "E0458" | "E0459" | "E0460"
        | "E0461" | "E0462" | "E0463" | "E0464" | "E0465" | "E0466" | "E0467" | "E0468"
        | "E0469" | "E0470" | "E0471" | "E0472" | "E0473" | "E0474" | "E0475" | "E0476"
        | "E0477" | "E0478" | "E0479" | "E0480" | "E0481" | "E0482" | "E0483" | "E0484"
        | "E0485" | "E0486" | "E0487" | "E0488" | "E0489" | "E0490" | "E0491" | "E0492"
        | "E0493" | "E0494" | "E0495" | "E0496" | "E0497" | "E0498" | "E0499" | "E0518"
        | "E0524" | "E0525" | "E0527" | "E0528" | "E0531" | "E0534" | "E0536" | "E0537"
        | "E0539" | "E0545" | "E0546" | "E0547" | "E0548" | "E0550" | "E0551" | "E0552"
        | "E0553" | "E0554" | "E0556" | "E0557" | "E0559" | "E0560" | "E0561" | "E0562"
        | "E0565" | "E0566" | "E0567" | "E0568" | "E0569" | "E0570" | "E0571" | "E0572"
        | "E0573" | "E0575" | "E0576" | "E0577" | "E0578" | "E0579" | "E0580" | "E0581"
        | "E0582" | "E0584" | "E0585" | "E0586" | "E0587" | "E0588" | "E0589" | "E0590"
        | "E0591" | "E0592" | "E0593" | "E0594" | "E0595" | "E0596" | "E0598" | "E0599"
        | "E0601" | "E0619" | "E0620" | "E0628" | "E0629" | "E0630" | "E0632" | "E0635"
        | "E0637" | "E0638" | "E0645" | "E0646" | "E0648" | "E0649" | "E0650" | "E0651"
        | "E0652" | "E0653" | "E0654" | "E0655" | "E0656" | "E0657" | "E0658" | "E0659"
        | "E0660" | "E0661" | "E0662" | "E0663" | "E0664" | "E0665" | "E0666" | "E0667"
        | "E0668" | "E0669" | "E0670" | "E0671" | "E0672" | "E0673" | "E0674" | "E0675"
        | "E0676" | "E0677" | "E0678" | "E0679" | "E0680" | "E0681" | "E0682" | "E0683"
        | "E0684" | "E0685" | "E0686" | "E0687" | "E0688" | "E0689" | "E0690" | "E0691"
        | "E0692" | "E0693" | "E0694" | "E0695" | "E0696" | "E0697" | "E0698" | "E0701"
        | "E0702" | "E0703" | "E0705" | "E0706" | "E0707" | "E0709" | "E0710" | "E0712"
        | "E0713" | "E0714" | "E0715" | "E0717" | "E0718" | "E0720" | "E0721" | "E0722"
        | "E0723" | "E0724" | "E0725" | "E0726" | "E0727" | "E0728" | "E0729" | "E0730"
        | "E0731" | "E0732" | "E0733" | "E0734" | "E0735" | "E0736" | "E0737" | "E0738"
        | "E0739" | "E0740" | "E0741" | "E0746" | "E0747" | "E0748" | "E0749" | "E0751"
        | "E0752" | "E0753" | "E0754" | "E0755" | "E0756" | "E0757" | "E0800" | "E0801"
        | "E0802" | "E0803" | "E0804" | "E0805" | "E0806" | "E0807" | "E0808" | "E0809"
        | "E0810" | "E0811" | "E0812" | "E0813" | "E0814" | "E0815" | "E0816" | "E0817"
        | "E0818" | "E0819" | "E0820" | "E0821" | "E0822" | "E0823" | "E0824" | "E0825"
        | "E0826" | "E0827" | "E0828" | "E0829" | "E0830" | "E0831" | "E0832" | "E0833"
        | "E0834" | "E0835" | "E0836" | "E0837" | "E0838" | "E0839" | "E0840" | "E0841"
        | "E0842" | "E0843" | "E0844" | "E0845" | "E0846" | "E0847" | "E0848" | "E0849"
        | "E0850" | "E0851" | "E0852" | "E0853" | "E0854" | "E0855" | "E0856" | "E0857"
        | "E0858" | "E0859" | "E0860" | "E0861" | "E0862" | "E0863" | "E0864" | "E0865"
        | "E0866" | "E0867" | "E0868" | "E0869" | "E0870" | "E0871" | "E0872" | "E0873"
        | "E0874" | "E0875" | "E0876" | "E0877" | "E0878" | "E0879" | "E0880" | "E0881"
        | "E0882" | "E0883" | "E0884" | "E0885" | "E0886" | "E0887" | "E0888" | "E0889"
        | "E0890" | "E0891" | "E0892" | "E0893" | "E0894" | "E0895" | "E0896" | "E0897"
        | "E0898" | "E0899" | "E0900" | "E0901" | "E0902" | "E0903" | "E0904" | "E0905"
        | "E0906" | "E0907" | "E0908" | "E0909" | "E0910" | "E0911" | "E0912" | "E0913"
        | "E0914" | "E0915" | "E0916" | "E0917" | "E0918" | "E0919" | "E0920" | "E0921"
        | "E0922" | "E0923" | "E0924" | "E0925" | "E0926" | "E0927" | "E0928" | "E0929"
        | "E0930" | "E0931" | "E0932" | "E0933" | "E0934" | "E0935" | "E0936" | "E0938"
        | "E0939" | "E0940" | "E0941" | "E0942" | "E0943" | "E0944" | "E0945" | "E0946"
        | "E0947" | "E0948" | "E0949" | "E0950" | "E0951" | "E0952" | "E0953" | "E0954"
        | "E0955" | "E0956" | "E0957" | "E0958" | "E0959" | "E0960" | "E0961" | "E0962"
        | "E0963" | "E0964" | "E0965" | "E0966" | "E0967" | "E0968" | "E0969" | "E0970"
        | "E0971" | "E0972" | "E0973" | "E0974" | "E0975" | "E0976" | "E0977" | "E0978"
        | "E0979" | "E0981" | "E0982" | "E0983" | "E0984" | "E0985" | "E0986" | "E0987"
        | "E0988" | "E0989" | "E0990" | "E0991" | "E0992" | "E0993" | "E0994" | "E0995"
        | "E0996" | "E0997" | "E0998" | "E0999" => ErrorCategory::Syntax,

        // Generic and const parameter errors
        "E0392" | "E0393" | "E0394" | "E0395" | "E0396" | "E0397" | "E0398" | "E0399" | "E0400"
        | "E0563" | "E0564" => ErrorCategory::Generic,

        // Macro expansion errors
        "E0276" | "E0519" | "E0520" | "E0521" | "E0522" | "E0523" | "E0704" | "E0784" | "E0785"
        | "E0786" | "E0787" | "E0788" | "E0789" => ErrorCategory::Macro,

        // Dead code or unused item errors
        "E0425" | "E0526" => ErrorCategory::DeadCode,

        // Unknown or unmapped error codes
        _ => ErrorCategory::Unknown,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Compilation Error Detection
// ──────────────────────────────────────────────────────────────────────────────

/// Detect compilation errors from cargo test stderr.
///
/// Parses cargo test stderr output and extracts compilation errors including
/// error codes, messages, and file locations. Handles multi-line error output
/// with location information on subsequent lines.
///
/// # Arguments
///
/// * `stderr` - The stderr output from cargo test
///
/// # Returns
///
/// A vector of `CompilationError` values representing all detected errors.
/// Returns an empty vector if no compilation errors are detected.
///
/// # Examples
///
/// ```
/// use needle::types::detect_compilation_errors;
///
/// let stderr = r#"error[E0308]: mismatched types
///  --> src/main.rs:10:5
///   |
/// 10|     let x: i32 = "hello";
///   |     ---   ^^^^^^^ expected `i32`, found `&str`
///   |     expected due to this
///
/// error: aborting due to 1 previous error"#;
///
/// let errors = detect_compilation_errors(stderr);
/// assert_eq!(errors.len(), 1);
/// assert!(matches!(errors[0], CompilationError::RustError { code, .. } if code == "E0308"));
/// ```
pub fn detect_compilation_errors(stderr: &str) -> Vec<CompilationError> {
    let mut errors = Vec::new();
    let mut current_error: Option<CompilationError> = None;
    let mut error_count: Option<usize> = None;

    for line in stderr.lines() {
        let line = line.trim();

        // Pattern 1: Rust compiler error with code: "error[E0308]: mismatched types"
        if let Some((code, message)) = parse_error_line(line) {
            // Save any current error before starting a new one
            if let Some(err) = current_error.take() {
                errors.push(err);
            }
            current_error = Some(CompilationError::RustError {
                code,
                message,
                file: None,
                line: None,
                column: None,
            });
            continue;
        }

        // Pattern 2: File location line: "   --> src/main.rs:10:5"
        if current_error.is_some() && line.contains("-->") {
            if let Some((file, line_num, col)) = parse_location_line(line) {
                if let Some(CompilationError::RustError {
                    code,
                    message,
                    file: _,
                    line: _,
                    column: _,
                }) = current_error.take()
                {
                    current_error = Some(CompilationError::RustError {
                        code,
                        message,
                        file: Some(file),
                        line: line_num,
                        column: col,
                    });
                }
            }
            continue;
        }

        // Pattern 3: "could not compile" message
        if line.contains("could not compile") {
            // Save any current error before adding the general error
            if let Some(err) = current_error.take() {
                errors.push(err);
            }
            errors.push(CompilationError::General {
                message: line.to_string(),
            });
            continue;
        }

        // Pattern 4: "aborting due to N previous errors"
        if line.contains("aborting due to") {
            // Save any current error before processing abort
            if let Some(err) = current_error.take() {
                errors.push(err);
            }
            if let Some(count) = parse_abort_count(line) {
                error_count = Some(count);
            }
            continue;
        }

        // Pattern 5: Check for warning/error messages without error codes (unused, dead_code, etc.)
        if line.starts_with("error:") && !line.starts_with("error[E") {
            // Check for unused warnings
            if line.contains("unused")
                || line.contains("dead_code")
                || line.contains("unused_variables")
            {
                // Save any current error before adding the general error
                if let Some(err) = current_error.take() {
                    errors.push(err);
                }
                errors.push(CompilationError::General {
                    message: line.to_string(),
                });
            }
        }

        // If we hit a blank line, save the current error
        if line.is_empty() {
            if let Some(err) = current_error.take() {
                errors.push(err);
            }
        }
    }

    // Don't forget the last error if there was one
    if let Some(err) = current_error {
        errors.push(err);
    }

    // Add abort error if we detected one
    if let Some(count) = error_count {
        errors.push(CompilationError::Abort { error_count: count });
    }

    errors
}

/// Parse an error line to extract error code and message.
///
/// Input: "error[E0308]: mismatched types"
/// Output: Some(("E0308", "mismatched types"))
fn parse_error_line(line: &str) -> Option<(String, String)> {
    if line.starts_with("error[E") {
        if let Some(end_bracket) = line.find(']') {
            let code = line[6..end_bracket].to_string(); // Skip "error[" but keep the "E"
            let rest = line[end_bracket + 1..].trim();
            let message = rest.strip_prefix(':').map(|s| s.trim()).unwrap_or(rest);
            return Some((code, message.to_string()));
        }
    }
    None
}

/// Parse a location line to extract file, line, and column.
///
/// Input: "   --> src/main.rs:10:5"
/// Output: Some(("src/main.rs", Some(10), Some(5)))
fn parse_location_line(line: &str) -> Option<(String, Option<usize>, Option<usize>)> {
    // Find the part after "-->"
    if let Some(arrow_pos) = line.find("-->") {
        let location_part = line[arrow_pos + 3..].trim();

        // Split by ':' to get file, line, and column
        let parts: Vec<&str> = location_part.split(':').collect();

        if parts.is_empty() {
            return None;
        }

        let file = parts[0].trim().to_string();
        let line_num = if parts.len() >= 2 {
            parts[1].trim().parse().ok()
        } else {
            None
        };
        let column = if parts.len() >= 3 {
            parts[2].trim().parse().ok()
        } else {
            None
        };

        return Some((file, line_num, column));
    }
    None
}

/// Parse the error count from an abort message.
///
/// Input: "aborting due to 3 previous errors"
/// Output: Some(3)
fn parse_abort_count(line: &str) -> Option<usize> {
    for word in line.split_whitespace() {
        if let Ok(n) = word.parse::<usize>() {
            return Some(n);
        }
    }
    None
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

/// Extract only `stitch:`-prefixed labels for propagation to a derived bead
/// (HOOP Hook 4 — Stitch Label Inheritance, docs/needle-hooks.md in the HOOP repo).
///
/// Unlike `extract_stitch_labels` (a blocklist that passes through nearly
/// everything), this is a strict allowlist — it exists specifically so a
/// follow-up bead inherits its parent's Stitch lineage without also picking
/// up unrelated project/domain labels the parent happened to carry.
pub fn extract_stitch_prefixed_labels(labels: &[String]) -> Vec<String> {
    labels
        .iter()
        .filter(|l| l.starts_with("stitch:"))
        .cloned()
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Error Code Parsing and Classification Tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn parse_error_code_valid_format() {
    // Valid E0XXX codes
    assert_eq!(parse_error_code("E0308"), Some("E0308".to_string()));
    assert_eq!(parse_error_code("E0001"), Some("E0001".to_string()));
    assert_eq!(parse_error_code("E0999"), Some("E0999".to_string()));
    assert_eq!(parse_error_code("E0382"), Some("E0382".to_string()));
}

#[test]
fn parse_error_code_from_error_line() {
    // Extract from error lines
    assert_eq!(
        parse_error_code("error[E0308]: mismatched types"),
        Some("E0308".to_string())
    );
    assert_eq!(
        parse_error_code("  error[E0382]: use of moved value"),
        Some("E0382".to_string())
    );
}

#[test]
fn parse_error_code_invalid_format() {
    // Invalid formats
    assert_eq!(parse_error_code("E12345"), None); // Too long
    assert_eq!(parse_error_code("E0ABC"), None); // Non-digit characters
    assert_eq!(parse_error_code("E012"), None); // Too short
    assert_eq!(parse_error_code("E001"), None); // Too short
    assert_eq!(parse_error_code("E000"), None); // No digit after 0
    assert_eq!(parse_error_code("E1000"), None); // First digit not 0
    assert_eq!(parse_error_code("E2000"), None); // First digit not 0
}

#[test]
fn parse_error_code_case_sensitive() {
    // Should be case-sensitive - only uppercase E
    assert_eq!(parse_error_code("e0308"), None); // lowercase e
    assert_eq!(parse_error_code("E0308"), Some("E0308".to_string())); // uppercase E
}

#[test]
fn parse_error_code_with_surrounding_text() {
    // Extract from within larger strings
    assert_eq!(
        parse_error_code("error: aborting due to previous error (E0308)"),
        Some("E0308".to_string())
    );
    assert_eq!(
        parse_error_code("see https://doc.rust-lang.org/error-index.html#E0382"),
        Some("E0382".to_string())
    );
}

#[test]
fn parse_error_code_empty_and_invalid() {
    // Edge cases
    assert_eq!(parse_error_code(""), None);
    assert_eq!(parse_error_code("E"), None);
    assert_eq!(parse_error_code("0308"), None);
    assert_eq!(parse_error_code("E0"), None);
    assert_eq!(parse_error_code("E0XXX"), None);
}

#[test]
fn classify_error_code_type_mismatch() {
    // Type mismatch errors
    assert_eq!(classify_error_code("E0308"), ErrorCategory::TypeMismatch);
    assert_eq!(classify_error_code("E0309"), ErrorCategory::TypeMismatch);
    assert_eq!(classify_error_code("E0310"), ErrorCategory::TypeMismatch);
    assert_eq!(classify_error_code("E0311"), ErrorCategory::TypeMismatch);
    assert_eq!(classify_error_code("E0312"), ErrorCategory::TypeMismatch);
    assert_eq!(classify_error_code("E0313"), ErrorCategory::TypeMismatch);
    assert_eq!(classify_error_code("E0314"), ErrorCategory::TypeMismatch);
    assert_eq!(classify_error_code("E0315"), ErrorCategory::TypeMismatch);
    assert_eq!(classify_error_code("E0316"), ErrorCategory::TypeMismatch);
    assert_eq!(classify_error_code("E0317"), ErrorCategory::TypeMismatch);
    assert_eq!(classify_error_code("E0369"), ErrorCategory::TypeMismatch);
    assert_eq!(classify_error_code("E0370"), ErrorCategory::TypeMismatch);
}

#[test]
fn classify_error_code_borrow_checker() {
    // Borrow checker errors
    assert_eq!(classify_error_code("E0382"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0502"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0503"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0505"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0506"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0507"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0508"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0509"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0510"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0511"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0512"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0515"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0516"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0517"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0597"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0623"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0624"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0625"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0626"), ErrorCategory::BorrowChecker);
    assert_eq!(classify_error_code("E0716"), ErrorCategory::BorrowChecker);
}

#[test]
fn classify_error_code_trait_impl() {
    // Trait implementation errors
    assert_eq!(classify_error_code("E0038"), ErrorCategory::TraitImpl);
    assert_eq!(classify_error_code("E0046"), ErrorCategory::TraitImpl);
    assert_eq!(classify_error_code("E0117"), ErrorCategory::TraitImpl);
    assert_eq!(classify_error_code("E0118"), ErrorCategory::TraitImpl);
    assert_eq!(classify_error_code("E0119"), ErrorCategory::TraitImpl);
    assert_eq!(classify_error_code("E0120"), ErrorCategory::TraitImpl);
    assert_eq!(classify_error_code("E0183"), ErrorCategory::TraitImpl);
    assert_eq!(classify_error_code("E0207"), ErrorCategory::TraitImpl);
    assert_eq!(classify_error_code("E0210"), ErrorCategory::TraitImpl);
    assert_eq!(classify_error_code("E0277"), ErrorCategory::TraitImpl);
    assert_eq!(classify_error_code("E0381"), ErrorCategory::TraitImpl);
}

#[test]
fn classify_error_code_pattern_matching() {
    // Pattern matching errors
    assert_eq!(classify_error_code("E0002"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0009"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0007"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0010"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0011"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0012"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0013"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0014"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0015"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0016"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0017"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0018"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0302"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0409"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0422"), ErrorCategory::PatternMatching);
    assert_eq!(classify_error_code("E0424"), ErrorCategory::PatternMatching);
}

#[test]
fn classify_error_code_scope_visibility() {
    // Scope and visibility errors
    assert_eq!(classify_error_code("E0403"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0404"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0405"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0406"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0407"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0408"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0603"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0604"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0605"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0606"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0742"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0743"), ErrorCategory::ScopeVisibility);
    assert_eq!(classify_error_code("E0750"), ErrorCategory::ScopeVisibility);
}

#[test]
fn classify_error_code_syntax() {
    // Syntax errors
    assert_eq!(classify_error_code("E0053"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0060"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0061"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0062"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0063"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0066"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0263"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0264"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0267"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0268"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0301"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0306"), ErrorCategory::Syntax);
    assert_eq!(classify_error_code("E0324"), ErrorCategory::Syntax);
}

#[test]
fn classify_error_code_generic() {
    // Generic and const parameter errors
    assert_eq!(classify_error_code("E0392"), ErrorCategory::Generic);
    assert_eq!(classify_error_code("E0393"), ErrorCategory::Generic);
    assert_eq!(classify_error_code("E0394"), ErrorCategory::Generic);
    assert_eq!(classify_error_code("E0395"), ErrorCategory::Generic);
    assert_eq!(classify_error_code("E0396"), ErrorCategory::Generic);
    assert_eq!(classify_error_code("E0397"), ErrorCategory::Generic);
    assert_eq!(classify_error_code("E0398"), ErrorCategory::Generic);
    assert_eq!(classify_error_code("E0399"), ErrorCategory::Generic);
    assert_eq!(classify_error_code("E0400"), ErrorCategory::Generic);
    assert_eq!(classify_error_code("E0563"), ErrorCategory::Generic);
    assert_eq!(classify_error_code("E0564"), ErrorCategory::Generic);
}

#[test]
fn classify_error_code_macro() {
    // Macro expansion errors
    assert_eq!(classify_error_code("E0276"), ErrorCategory::Macro);
    assert_eq!(classify_error_code("E0519"), ErrorCategory::Macro);
    assert_eq!(classify_error_code("E0520"), ErrorCategory::Macro);
    assert_eq!(classify_error_code("E0521"), ErrorCategory::Macro);
    assert_eq!(classify_error_code("E0522"), ErrorCategory::Macro);
    assert_eq!(classify_error_code("E0523"), ErrorCategory::Macro);
    assert_eq!(classify_error_code("E0704"), ErrorCategory::Macro);
}

#[test]
fn classify_error_code_dead_code() {
    // Dead code or unused item errors (E0382 is primarily borrow checker, E0601/E0602/E0611 are primarily scope visibility)
    assert_eq!(classify_error_code("E0425"), ErrorCategory::DeadCode);
    assert_eq!(classify_error_code("E0526"), ErrorCategory::DeadCode);
}

#[test]
fn classify_error_code_unknown() {
    // Unknown or unmapped error codes
    assert_eq!(classify_error_code("E9999"), ErrorCategory::Unknown);
    assert_eq!(classify_error_code("E8888"), ErrorCategory::Unknown);
    assert_eq!(classify_error_code("E1234"), ErrorCategory::Unknown);
    assert_eq!(classify_error_code("E0050"), ErrorCategory::Unknown);
    assert_eq!(classify_error_code("E0099"), ErrorCategory::Unknown);
}

#[test]
fn error_category_description() {
    // Test category descriptions
    assert_eq!(
        ErrorCategory::TypeMismatch.description(),
        "type mismatch or conversion error"
    );
    assert_eq!(
        ErrorCategory::BorrowChecker.description(),
        "borrow checker or lifetime error"
    );
    assert_eq!(
        ErrorCategory::TraitImpl.description(),
        "trait implementation or bound error"
    );
    assert_eq!(
        ErrorCategory::PatternMatching.description(),
        "pattern matching error"
    );
    assert_eq!(
        ErrorCategory::ScopeVisibility.description(),
        "scope or visibility error"
    );
    assert_eq!(ErrorCategory::Syntax.description(), "syntax error");
    assert_eq!(
        ErrorCategory::Generic.description(),
        "generic or const parameter error"
    );
    assert_eq!(ErrorCategory::Macro.description(), "macro expansion error");
    assert_eq!(
        ErrorCategory::DeadCode.description(),
        "dead code or unused item"
    );
    assert_eq!(
        ErrorCategory::Unknown.description(),
        "unknown error category"
    );
}

#[test]
fn error_category_display() {
    // Test Display implementation
    assert_eq!(ErrorCategory::TypeMismatch.to_string(), "type_mismatch");
    assert_eq!(ErrorCategory::BorrowChecker.to_string(), "borrow_checker");
    assert_eq!(ErrorCategory::TraitImpl.to_string(), "trait_impl");
    assert_eq!(
        ErrorCategory::PatternMatching.to_string(),
        "pattern_matching"
    );
    assert_eq!(
        ErrorCategory::ScopeVisibility.to_string(),
        "scope_visibility"
    );
    assert_eq!(ErrorCategory::Syntax.to_string(), "syntax");
    assert_eq!(ErrorCategory::Generic.to_string(), "generic");
    assert_eq!(ErrorCategory::Macro.to_string(), "macro");
    assert_eq!(ErrorCategory::DeadCode.to_string(), "dead_code");
    assert_eq!(ErrorCategory::Unknown.to_string(), "unknown");
}

#[test]
fn error_category_all_variants_have_display_impl() {
    // Verify all variants can be displayed
    let categories = vec![
        ErrorCategory::TypeMismatch,
        ErrorCategory::BorrowChecker,
        ErrorCategory::TraitImpl,
        ErrorCategory::PatternMatching,
        ErrorCategory::ScopeVisibility,
        ErrorCategory::Syntax,
        ErrorCategory::Generic,
        ErrorCategory::Macro,
        ErrorCategory::DeadCode,
        ErrorCategory::Unknown,
    ];

    for category in categories {
        let display = format!("{}", category);
        let desc = category.description();
        assert!(!display.is_empty());
        assert!(!desc.is_empty());
    }
}

#[test]
fn error_category_serde_roundtrip() {
    // Test serialization/deserialization
    let categories = vec![
        ErrorCategory::TypeMismatch,
        ErrorCategory::BorrowChecker,
        ErrorCategory::TraitImpl,
        ErrorCategory::PatternMatching,
        ErrorCategory::ScopeVisibility,
        ErrorCategory::Syntax,
        ErrorCategory::Generic,
        ErrorCategory::Macro,
        ErrorCategory::DeadCode,
        ErrorCategory::Unknown,
    ];

    for category in categories {
        let json = serde_json::to_string(&category).unwrap();
        let parsed: ErrorCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, category, "roundtrip failed for {:?}", category);
    }
}

#[test]
fn parse_and_classify_integration() {
    // Integration test: parse and classify common error codes
    let test_cases = vec![
        ("E0308", ErrorCategory::TypeMismatch),
        ("E0382", ErrorCategory::BorrowChecker),
        ("E0038", ErrorCategory::TraitImpl),
        ("E0002", ErrorCategory::PatternMatching),
        ("E0403", ErrorCategory::ScopeVisibility),
        ("E0053", ErrorCategory::Syntax),
        ("E0392", ErrorCategory::Generic),
        ("E0276", ErrorCategory::Macro),
        ("E0425", ErrorCategory::DeadCode),
        // Must be a well-formed but unmapped code. `parse_error_code` only
        // accepts the E0xxx form that rustc actually emits — see
        // `parse_error_code_invalid_format`, which asserts E1000/E2000 are
        // rejected for exactly that reason. "E9999" therefore cannot parse,
        // and asserting it does contradicted that test outright.
        ("E0000", ErrorCategory::Unknown),
    ];

    for (code, expected_category) in test_cases {
        let parsed = parse_error_code(code);
        assert_eq!(parsed, Some(code.to_string()), "Failed to parse {}", code);

        let category = classify_error_code(code);
        assert_eq!(
            category, expected_category,
            "Wrong category for {}: got {:?}, expected {:?}",
            code, category, expected_category
        );
    }
}

#[test]
fn error_category_equality() {
    // Test equality
    assert_eq!(ErrorCategory::TypeMismatch, ErrorCategory::TypeMismatch);
    assert_eq!(ErrorCategory::BorrowChecker, ErrorCategory::BorrowChecker);
    assert_eq!(ErrorCategory::Unknown, ErrorCategory::Unknown);

    assert_ne!(ErrorCategory::TypeMismatch, ErrorCategory::BorrowChecker);
    assert_ne!(ErrorCategory::Syntax, ErrorCategory::Macro);
    assert_ne!(ErrorCategory::DeadCode, ErrorCategory::Unknown);
}

// ──────────────────────────────────────────────────────────────────────────────
// Timeout Mitosis Decomposition Tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn mitosis_mode_display() {
    assert_eq!(MitosisMode::Timeout.to_string(), "timeout");
    assert_eq!(MitosisMode::Ordinary.to_string(), "ordinary");
}

#[test]
fn mitosis_mode_serde_roundtrip() {
    let modes = vec![MitosisMode::Timeout, MitosisMode::Ordinary];
    for mode in modes {
        let json = serde_json::to_string(&mode).unwrap();
        let parsed: MitosisMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, mode);
    }
}

#[test]
fn decomposition_proposal_timeout_splittable() {
    let proposal = DecompositionProposal::Timeout {
        phases: vec![TimeoutPhaseProposal {
            title: "Phase 1".to_string(),
            description: "Complete work".to_string(),
            is_completed: false,
            depends_on_phases: vec![],
            estimated_duration_secs: Some(3600),
            completion_criteria: vec!["All tests pass".to_string()],
        }],
        refusal_reason: None,
    };

    assert!(proposal.is_splittable());
    assert_eq!(proposal.mode(), MitosisMode::Timeout);
    assert_eq!(proposal.child_count(), 1);
    assert!(proposal.refusal_reason().is_none());
}

#[test]
fn decomposition_proposal_timeout_refused() {
    let proposal = DecompositionProposal::Timeout {
        phases: vec![],
        refusal_reason: Some("Task is atomic".to_string()),
    };

    assert!(!proposal.is_splittable());
    assert_eq!(proposal.mode(), MitosisMode::Timeout);
    assert_eq!(proposal.child_count(), 0);
    assert_eq!(proposal.refusal_reason(), Some("Task is atomic"));
}

#[test]
fn decomposition_proposal_ordinary_splittable() {
    let proposal = DecompositionProposal::Ordinary {
        tasks: vec![OrdinaryTaskProposal {
            title: "Sub-task A".to_string(),
            description: "Do A".to_string(),
            is_completed: false,
        }],
        refusal_reason: None,
    };

    assert!(proposal.is_splittable());
    assert_eq!(proposal.mode(), MitosisMode::Ordinary);
    assert_eq!(proposal.child_count(), 1);
    assert!(proposal.refusal_reason().is_none());
}

#[test]
fn timeout_refusal_reason_atomic() {
    let reason = TimeoutRefusalReason::Atomic {
        explanation: "Full test suite cannot be decomposed".to_string(),
    };

    assert_eq!(reason.as_str(), "atomic");
    assert_eq!(
        reason.explanation(),
        "Full test suite cannot be decomposed"
    );
    assert_eq!(
        reason.to_string(),
        "atomic: Full test suite cannot be decomposed"
    );
}

#[test]
fn timeout_refusal_reason_unsafe_overlap() {
    let reason = TimeoutRefusalReason::UnsafeOverlap {
        explanation: "Splitting would redo incomplete work".to_string(),
    };

    assert_eq!(reason.as_str(), "unsafe_overlap");
    assert!(matches!(reason, TimeoutRefusalReason::UnsafeOverlap { .. }));
}

#[test]
fn timeout_refusal_reason_infrastructure_failure() {
    let reason = TimeoutRefusalReason::InfrastructureFailure {
        explanation: "Network hang caused timeout".to_string(),
    };

    assert_eq!(reason.as_str(), "infrastructure_failure");
}

#[test]
fn timeout_refusal_reason_insufficient_context() {
    let reason = TimeoutRefusalReason::InsufficientContext {
        explanation: "Git state shows no commits".to_string(),
    };

    assert_eq!(reason.as_str(), "insufficient_context");
}

#[test]
fn timeout_refusal_reason_depth_limit() {
    let reason = TimeoutRefusalReason::DepthLimit {
        max_depth: 2,
        current_depth: 3,
    };

    assert_eq!(reason.as_str(), "depth_limit");
    assert_eq!(
        reason.explanation(),
        "bead has reached maximum generation depth for mitosis"
    );
}

#[test]
fn timeout_refusal_reason_out_of_scope() {
    let reason = TimeoutRefusalReason::OutOfScope {
        explanation: "Bead references Pluck configuration".to_string(),
    };

    assert_eq!(reason.as_str(), "out_of_scope");
}

#[test]
fn timeout_refusal_reason_serde_roundtrip() {
    let reasons = vec![
        TimeoutRefusalReason::Atomic {
            explanation: "atomic task".to_string(),
        },
        TimeoutRefusalReason::UnsafeOverlap {
            explanation: "unsafe overlap".to_string(),
        },
        TimeoutRefusalReason::InfrastructureFailure {
            explanation: "infrastructure".to_string(),
        },
        TimeoutRefusalReason::InsufficientContext {
            explanation: "no context".to_string(),
        },
        TimeoutRefusalReason::DepthLimit {
            max_depth: 5,
            current_depth: 6,
        },
        TimeoutRefusalReason::OutOfScope {
            explanation: "out of scope".to_string(),
        },
    ];

    for reason in reasons {
        let json = serde_json::to_string(&reason).unwrap();
        let parsed: TimeoutRefusalReason = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, reason);
    }
}

#[test]
fn decomposition_safety_safe() {
    let safety = DecompositionSafety::Safe {
        confidence: 0.85,
        evidence: vec!["Git commits show clear progress".to_string()],
    };

    assert!(safety.is_safe());
    assert_eq!(safety.confidence(), 0.85);
}

#[test]
fn decomposition_safety_unsafe() {
    let safety = DecompositionSafety::Unsafe {
        reason: TimeoutRefusalReason::Atomic {
            explanation: "atomic task".to_string(),
        },
    };

    assert!(!safety.is_safe());
    assert_eq!(safety.confidence(), 0.0);
}

#[test]
fn timeout_phase_proposal_serialization() {
    let phase = TimeoutPhaseProposal {
        title: "Phase 1: Complete OAuth".to_string(),
        description: "Implement token endpoint".to_string(),
        is_completed: false,
        depends_on_phases: vec![],
        estimated_duration_secs: Some(1800),
        completion_criteria: vec!["Tests pass".to_string()],
    };

    let json = serde_json::to_string(&phase).unwrap();
    let parsed: TimeoutPhaseProposal = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.title, phase.title);
    assert_eq!(parsed.description, phase.description);
    assert_eq!(parsed.is_completed, phase.is_completed);
    assert_eq!(parsed.estimated_duration_secs, phase.estimated_duration_secs);
}

#[test]
fn ordinary_task_proposal_serialization() {
    let task = OrdinaryTaskProposal {
        title: "Add endpoint".to_string(),
        description: "Create REST endpoint".to_string(),
        is_completed: false,
    };

    let json = serde_json::to_string(&task).unwrap();
    let parsed: OrdinaryTaskProposal = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.title, task.title);
    assert_eq!(parsed.description, task.description);
}

#[test]
fn decomposition_proposal_mode_detection() {
    let timeout_proposal = DecompositionProposal::Timeout {
        phases: vec![],
        refusal_reason: None,
    };

    let ordinary_proposal = DecompositionProposal::Ordinary {
        tasks: vec![],
        refusal_reason: None,
    };

    assert_eq!(timeout_proposal.mode(), MitosisMode::Timeout);
    assert_eq!(ordinary_proposal.mode(), MitosisMode::Ordinary);
}

// ──────────────────────────────────────────────────────────────────────────────
// Timeout Mitosis Decomposition Types
// ──────────────────────────────────────────────────────────────────────────────

/// Mitosis mode: distinguishes timeout-triggered from ordinary failure decomposition.
///
/// Timeout mode uses different decomposition criteria and produces phase-based
/// children that can close independently when their portion of work completes.
/// Ordinary failure mode splits multi-task beads without phase awareness.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MitosisMode {
    /// Timeout-triggered mitosis: splits bead based on completed vs remaining work.
    /// Uses timeout context (elapsed time, activity evidence, git state) to infer
    /// decomposition boundaries and produce independently closable phases.
    Timeout,
    /// Ordinary failure mitosis: splits multi-task beads based on task analysis.
    /// Triggered by failure count thresholds without timeout context.
    Ordinary,
}

impl fmt::Display for MitosisMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MitosisMode::Timeout => write!(f, "timeout"),
            MitosisMode::Ordinary => write!(f, "ordinary"),
        }
    }
}

/// A child bead proposal in timeout-triggered mitosis.
///
/// Unlike ordinary mitosis children, timeout-phase children are designed to
/// close independently when their specific portion of work completes. They
/// carry phase metadata and completion criteria.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TimeoutPhaseProposal {
    /// Human-readable title for this phase (e.g., "Phase 1: Complete OAuth implementation").
    pub title: String,
    /// Detailed description of what this phase accomplishes and its acceptance criteria.
    pub description: String,
    /// Whether this phase represents work already completed before the timeout.
    /// Completed phases are created as closed beads to track progress without blocking.
    pub is_completed: bool,
    /// Dependencies on other phase beads by title (linearized by mitosis evaluator).
    #[serde(default)]
    pub depends_on_phases: Vec<String>,
    /// Estimated or observed duration for this phase (if known).
    #[serde(default)]
    pub estimated_duration_secs: Option<u64>,
    /// Completion criteria: what conditions must be met to close this phase bead.
    #[serde(default)]
    pub completion_criteria: Vec<String>,
}

/// A child bead proposal in ordinary failure mitosis.
///
/// Ordinary mitosis splits multi-task beads without phase awareness. Children
/// are independent sub-tasks that must all complete before the parent closes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OrdinaryTaskProposal {
    /// Title for this sub-task (e.g., "Add endpoint" not "Phase 1: ...").
    pub title: String,
    /// Description of the sub-task deliverables and acceptance criteria.
    pub description: String,
    /// Whether this sub-task was already completed (observed via git state).
    pub is_completed: bool,
}

/// Unified decomposition proposal that can represent either timeout or ordinary mode.
///
/// This enum provides type-safe discrimination between the two mitosis modes
/// while maintaining a common API for the evaluator and caller.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DecompositionProposal {
    /// Timeout-triggered decomposition with phase-based children.
    #[serde(rename = "timeout")]
    Timeout {
        /// Phases that split the bead's work into independently closable units.
        phases: Vec<TimeoutPhaseProposal>,
        /// Reason if decomposition was refused (empty if proposing children).
        #[serde(default)]
        refusal_reason: Option<String>,
    },
    /// Ordinary failure decomposition with task-based children.
    #[serde(rename = "ordinary")]
    Ordinary {
        /// Sub-tasks that split the bead's work into independent units.
        tasks: Vec<OrdinaryTaskProposal>,
        /// Reason if decomposition was refused (empty if proposing children).
        #[serde(default)]
        refusal_reason: Option<String>,
    },
}

impl DecompositionProposal {
    /// Returns true if the proposal indicates the bead should be split.
    pub fn is_splittable(&self) -> bool {
        match self {
            DecompositionProposal::Timeout { phases, refusal_reason, .. } => {
                refusal_reason.is_none() && !phases.is_empty()
            }
            DecompositionProposal::Ordinary { tasks, refusal_reason, .. } => {
                refusal_reason.is_none() && !tasks.is_empty()
            }
        }
    }

    /// Returns the mitosis mode for this proposal.
    pub fn mode(&self) -> MitosisMode {
        match self {
            DecompositionProposal::Timeout { .. } => MitosisMode::Timeout,
            DecompositionProposal::Ordinary { .. } => MitosisMode::Ordinary,
        }
    }

    /// Returns the refusal reason if decomposition was refused.
    pub fn refusal_reason(&self) -> Option<&str> {
        match self {
            DecompositionProposal::Timeout { refusal_reason, .. } => refusal_reason.as_deref(),
            DecompositionProposal::Ordinary { refusal_reason, .. } => refusal_reason.as_deref(),
        }
    }

    /// Returns the number of proposed children (0 if refused).
    pub fn child_count(&self) -> usize {
        match self {
            DecompositionProposal::Timeout { phases, .. } => phases.len(),
            DecompositionProposal::Ordinary { tasks, .. } => tasks.len(),
        }
    }
}

/// Timeout decomposition refusal categories.
///
/// These are canonical reasons why timeout-triggered mitosis might refuse to
/// split a bead, used for telemetry and consistent agent prompting.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutRefusalReason {
    /// The task is atomic and cannot be meaningfully decomposed.
    Atomic { explanation: String },

    /// Splitting would require unsafe overlap with incomplete work.
    UnsafeOverlap { explanation: String },

    /// The timeout was caused by infrastructure failure, not productive work.
    InfrastructureFailure { explanation: String },

    /// Insufficient context to determine a safe decomposition boundary.
    InsufficientContext { explanation: String },

    /// The bead has exceeded maximum mitosis depth.
    DepthLimit { max_depth: u32, current_depth: u32 },

    /// The bead references NEEDLE-internal configuration (out of scope).
    OutOfScope { explanation: String },
}

impl TimeoutRefusalReason {
    /// Returns a human-readable explanation.
    pub fn explanation(&self) -> &str {
        match self {
            TimeoutRefusalReason::Atomic { explanation } => explanation,
            TimeoutRefusalReason::UnsafeOverlap { explanation } => explanation,
            TimeoutRefusalReason::InfrastructureFailure { explanation } => explanation,
            TimeoutRefusalReason::InsufficientContext { explanation } => explanation,
            TimeoutRefusalReason::DepthLimit { .. } => {
                "bead has reached maximum generation depth for mitosis"
            }
            TimeoutRefusalReason::OutOfScope { explanation } => explanation,
        }
    }

    /// Returns a refusal reason suitable for logging/telemetry.
    pub fn as_str(&self) -> &'static str {
        match self {
            TimeoutRefusalReason::Atomic { .. } => "atomic",
            TimeoutRefusalReason::UnsafeOverlap { .. } => "unsafe_overlap",
            TimeoutRefusalReason::InfrastructureFailure { .. } => "infrastructure_failure",
            TimeoutRefusalReason::InsufficientContext { .. } => "insufficient_context",
            TimeoutRefusalReason::DepthLimit { .. } => "depth_limit",
            TimeoutRefusalReason::OutOfScope { .. } => "out_of_scope",
        }
    }
}

impl fmt::Display for TimeoutRefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.as_str(), self.explanation())
    }
}

/// Decomposition safety assessment for timeout-triggered mitosis.
///
/// This type captures the analysis of whether splitting a timeout bead is
/// safe, combining eligibility, context analysis, and agent judgment.
#[derive(Debug, Clone, PartialEq)]
pub enum DecompositionSafety {
    /// Safe to split: the timeout represents productive long-running work
    /// that can be decomposed into independently completable phases.
    Safe {
        /// Confidence level in this assessment (0.0 to 1.0).
        confidence: f32,
        /// Evidence supporting the safety decision.
        evidence: Vec<String>,
    },

    /// Unsafe to split: decomposition would risk corruption, duplication, or loss.
    Unsafe {
        /// Why splitting is unsafe.
        reason: TimeoutRefusalReason,
    },
}

impl DecompositionSafety {
    /// Returns true if the assessment indicates safe decomposition.
    pub fn is_safe(&self) -> bool {
        matches!(self, DecompositionSafety::Safe { .. })
    }

    /// Returns the confidence level (0.0 if unsafe).
    pub fn confidence(&self) -> f32 {
        match self {
            DecompositionSafety::Safe { confidence, .. } => *confidence,
            DecompositionSafety::Unsafe { .. } => 0.0,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Timeout Analysis Result Types
// ──────────────────────────────────────────────────────────────────────────────

/// Result of analyzing a timeout event for Mitosis decomposition.
///
/// Captures timeout-specific failure data including duration, retry history,
/// and contextual evidence used to determine whether the timeout represents
/// productive long-running work or an infrastructure failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TimeoutAnalysisResult {
    /// Duration of the timeout in seconds.
    pub timeout_duration_secs: u64,

    /// Number of retries this bead has undergone.
    pub retry_count: u32,

    /// Contextual information about what was being done when the timeout occurred.
    pub timeout_context: TimeoutContext,

    /// Whether there was evidence of productive work before the timeout.
    pub has_activity_evidence: bool,

    /// Git state analysis results (if available).
    #[serde(default)]
    pub git_state: Option<GitStateAnalysis>,
}

/// Contextual information about what was being done when the timeout occurred.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutContext {
    /// Agent was actively working on code compilation.
    Compilation {
        /// Which compilation stage (e.g., "debug", "release", "test").
        stage: String,
    },

    /// Agent was running tests.
    TestExecution {
        /// Number of tests being run.
        test_count: Option<usize>,
        /// Whether any tests were passing before timeout.
        tests_passing: bool,
    },

    /// Agent was analyzing code or reading documentation.
    Analysis {
        /// Files being examined.
        files_examined: Vec<String>,
        /// Whether the agent was making progress through files.
        making_progress: bool,
    },

    /// Agent was performing build or deployment operations.
    BuildDeployment {
        /// Description of the operation (e.g., "docker build", "kubectl apply").
        operation: String,
    },

    /// Agent was executing a long-running command or process.
    LongRunningProcess {
        /// The command being executed.
        command: String,
        /// Expected duration if known.
        expected_duration_secs: Option<u64>,
    },

    /// Unknown context - insufficient evidence to determine activity.
    Unknown,
}

/// Analysis of git state at timeout time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GitStateAnalysis {
    /// Whether there were uncommitted changes.
    pub has_uncommitted_changes: bool,

    /// Number of files modified.
    pub modified_file_count: usize,

    /// Whether changes suggest productive work (vs. speculative or test edits).
    pub suggests_productive_work: bool,

    /// Branch name (if available).
    #[serde(default)]
    pub branch: Option<String>,
}

impl TimeoutAnalysisResult {
    /// Create a new timeout analysis result.
    pub fn new(
        timeout_duration_secs: u64,
        retry_count: u32,
        timeout_context: TimeoutContext,
    ) -> Self {
        Self {
            timeout_duration_secs,
            retry_count,
            timeout_context,
            has_activity_evidence: false,
            git_state: None,
        }
    }

    /// Add activity evidence to the analysis.
    pub fn with_activity_evidence(mut self, has_evidence: bool) -> Self {
        self.has_activity_evidence = has_evidence;
        self
    }

    /// Add git state analysis to the result.
    pub fn with_git_state(mut self, git_state: GitStateAnalysis) -> Self {
        self.git_state = Some(git_state);
        self
    }

    /// Returns true if the analysis suggests productive work was in progress.
    pub fn suggests_productive_work(&self) -> bool {
        self.has_activity_evidence
            || match &self.timeout_context {
                TimeoutContext::Compilation { .. } => true,
                TimeoutContext::TestExecution {
                    tests_passing: true, ..
                } => true,
                TimeoutContext::Analysis {
                    making_progress: true, ..
                } => true,
                TimeoutContext::BuildDeployment { .. } => true,
                TimeoutContext::LongRunningProcess { .. } => true,
                _ => false,
            }
    }

    /// Returns a human-readable summary of the timeout context.
    pub fn context_summary(&self) -> String {
        match &self.timeout_context {
            TimeoutContext::Compilation { stage } => {
                format!("compilation ({})", stage)
            }
            TimeoutContext::TestExecution {
                test_count,
                tests_passing,
            } => {
                format!(
                    "test execution ({} tests, {})",
                    test_count.map_or("unknown".to_string(), |n| n.to_string()),
                    if *tests_passing { "passing" } else { "failing" }
                )
            }
            TimeoutContext::Analysis {
                files_examined,
                making_progress,
            } => {
                format!(
                    "analysis ({} files, {})",
                    files_examined.len(),
                    if *making_progress { "progressing" } else { "stalled" }
                )
            }
            TimeoutContext::BuildDeployment { operation } => {
                format!("build/deployment ({})", operation)
            }
            TimeoutContext::LongRunningProcess {
                command,
                expected_duration_secs,
            } => {
                format!(
                    "long-running process ({}, expected: {}s)",
                    command,
                    expected_duration_secs.map_or("unknown".to_string(), |d| d.to_string())
                )
            }
            TimeoutContext::Unknown => "unknown context".to_string(),
        }
    }
}
