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
// HardDeadline type alias
// ──────────────────────────────────────────────────────────────────────────────

/// Hard deadline - absolute wall-clock timeout from process start (in seconds).
///
/// A hard deadline is a **non-resettable, absolute timeout** that starts counting
/// from the moment the agent process is spawned. Unlike idle timeout (which is
/// reset on any stdout/stderr activity), the hard deadline is a strict upper
/// bound on total execution time regardless of process activity.
///
/// **Key characteristics:**
/// - **Absolute**: Measured from process spawn time, not last activity
/// - **Non-resettable**: Cannot be extended or reset by any process behavior
/// - **Strict**: Process termination occurs immediately when deadline is reached
/// - **Independent**: Operates separately from idle timeout detection
///
/// Represented as `u64` seconds. A value of `0` means no deadline is enforced.
pub type HardDeadline = u64;

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
// DecompositionDecision
// ──────────────────────────────────────────────────────────────────────────────

/// Decision whether to decompose a bead into child beads or refuse decomposition.
///
/// This enum represents the outcome of evaluating whether a bead should be split
/// into smaller child beads via timeout-driven decomposition. The decision is
/// based on clear thresholds and bead characteristics.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecompositionDecision {
    /// Split the bead into the proposed child beads.
    ///
    /// This variant contains the proposed child beads that should be created
    /// to complete the original work in smaller, manageable phases.
    Split {
        /// Proposed child beads to create.
        with_proposals: Vec<ChildBeadProposal>,
    },
    /// Refuse to decompose the bead.
    ///
    /// This variant is returned when the bead does not meet the criteria for
    /// decomposition. The refusal reason explains why splitting was not appropriate.
    Refuse {
        /// Human-readable reason for refusal.
        reason: String,
    },
}

impl DecompositionDecision {
    /// Returns true if this decision is to split.
    pub fn is_split(&self) -> bool {
        matches!(self, DecompositionDecision::Split { .. })
    }

    /// Returns true if this decision is to refuse decomposition.
    pub fn is_refuse(&self) -> bool {
        matches!(self, DecompositionDecision::Refuse { .. })
    }

    /// Returns the proposals if this is a Split decision, None otherwise.
    pub fn proposals(&self) -> Option<&Vec<ChildBeadProposal>> {
        match self {
            DecompositionDecision::Split { with_proposals } => Some(with_proposals),
            DecompositionDecision::Refuse { .. } => None,
        }
    }

    /// Returns the refusal reason if this is a Refuse decision, None otherwise.
    pub fn refusal_reason(&self) -> Option<&String> {
        match self {
            DecompositionDecision::Refuse { reason } => Some(reason),
            DecompositionDecision::Split { .. } => None,
        }
    }
}

impl fmt::Display for DecompositionDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecompositionDecision::Split { with_proposals } => {
                write!(f, "split into {} child beads", with_proposals.len())
            }
            DecompositionDecision::Refuse { reason } => {
                write!(f, "refuse: {}", reason)
            }
        }
    }
}

/// Configuration for decomposition decision thresholds.
#[derive(Debug, Clone)]
pub struct DecompositionThresholds {
    /// Minimum timeout (in seconds) required to consider splitting.
    /// Beads that timed out quickly are likely genuine errors, not size issues.
    pub min_timeout_seconds: u64,
    /// Minimum number of retries required to consider splitting.
    /// Beads should be given multiple chances before decomposition.
    pub min_retry_count: u32,
    /// Minimum bead body length (in characters) to consider splitting.
    /// Beads shorter than this are considered "too small" to split meaningfully.
    pub min_bead_size: usize,
    /// Labels that indicate a bead is already a child bead.
    /// Such beads should not be split further to avoid deep nesting.
    pub child_labels: Vec<String>,
}

impl Default for DecompositionThresholds {
    fn default() -> Self {
        DecompositionThresholds {
            min_timeout_seconds: 300, // 5 minutes
            min_retry_count: 2,       // Must retry at least twice
            min_bead_size: 200,       // At least 200 characters
            child_labels: vec![
                "mitosis-child".to_string(),
                "decomposition-child".to_string(),
            ],
        }
    }
}

/// Evaluate whether a bead should be decomposed into child beads.
///
/// This function implements the decision tree for timeout-driven decomposition.
/// It evaluates bead characteristics against configured thresholds to determine
/// whether splitting is appropriate.
///
/// # Arguments
///
/// * `bead` - The bead to evaluate
/// * `timeout_seconds` - The timeout duration that was exceeded
/// * `retry_count` - Number of consecutive retries for this bead
/// * `thresholds` - Configuration thresholds (uses defaults if None)
///
/// # Returns
///
/// * `DecompositionDecision::Split` - If criteria are met (proposals must be added by caller)
/// * `DecompositionDecision::Refuse` - If criteria are not met
///
/// # Decision Logic
///
/// **Split if ALL of these conditions are met:**
/// - timeout_seconds >= thresholds.min_timeout_seconds
/// - retry_count >= thresholds.min_retry_count
/// - bead body length >= thresholds.min_bead_size
/// - bead has no assignee (not actively being worked)
/// - bead has no child labels (not already a decomposed child)
///
/// **Refuse if ANY of these conditions are met:**
/// - Bead body is too small (< min_bead_size)
/// - Bead has an active assignee
/// - Bead is already a child (has child labels)
/// - Timeout is too short (< min_timeout_seconds)
/// - Not enough retries (< min_retry_count)
///
/// # Examples
///
/// ```no_run
/// use needle::types::{Bead, BeadId, BeadStatus, decompose_bead_decision};
/// use std::path::PathBuf;
/// use chrono::{DateTime, Utc};
///
/// let bead = Bead {
///     id: BeadId::from("needle-test"),
///     title: "Test bead".to_string(),
///     body: Some("This is a substantial bead body that exceeds 200 characters...".to_string()),
///     priority: 2,
///     status: BeadStatus::Open,
///     assignee: None,
///     labels: vec![],
///     workspace: PathBuf::from("/tmp/test"),
///     dependencies: vec![],
///     dependents: vec![],
///     comments: vec![],
///     created_at: DateTime::from_timestamp(0, 0).unwrap(),
///     updated_at: DateTime::from_timestamp(0, 0).unwrap(),
/// };
///
/// let decision = decompose_bead_decision(&bead, 600, 3, None);
/// assert!(decision.is_split());
/// ```
pub fn decompose_bead_decision(
    bead: &Bead,
    timeout_seconds: u64,
    retry_count: u32,
    thresholds: Option<DecompositionThresholds>,
) -> DecompositionDecision {
    let thresholds = thresholds.unwrap_or_default();

    // Check if bead is already a child (has child labels)
    let is_child_bead = bead
        .labels
        .iter()
        .any(|label| thresholds.child_labels.contains(label));

    if is_child_bead {
        return DecompositionDecision::Refuse {
            reason: format!(
                "bead is already a child bead (has labels: {:?})",
                bead.labels
                    .iter()
                    .filter(|l| thresholds.child_labels.contains(l))
                    .collect::<Vec<_>>()
            ),
        };
    }

    // Check if bead has an active assignee
    if let Some(assignee) = &bead.assignee {
        return DecompositionDecision::Refuse {
            reason: format!("bead has active assignee: {}", assignee),
        };
    }

    // Check bead body size
    let body_size = bead.body.as_ref().map(|b| b.len()).unwrap_or(0);
    if body_size < thresholds.min_bead_size {
        return DecompositionDecision::Refuse {
            reason: format!(
                "bead body too small ({} chars < {} minimum)",
                body_size, thresholds.min_bead_size
            ),
        };
    }

    // Check timeout threshold
    if timeout_seconds < thresholds.min_timeout_seconds {
        return DecompositionDecision::Refuse {
            reason: format!(
                "timeout too short ({}s < {}s minimum)",
                timeout_seconds, thresholds.min_timeout_seconds
            ),
        };
    }

    // Check retry count threshold
    if retry_count < thresholds.min_retry_count {
        return DecompositionDecision::Refuse {
            reason: format!(
                "insufficient retries ({} < {} minimum)",
                retry_count, thresholds.min_retry_count
            ),
        };
    }

    // All criteria met - approve for splitting
    // The caller must add actual proposals via Split::with_proposals
    DecompositionDecision::Split {
        with_proposals: Vec::new(), // Empty - caller will populate
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
// ClaimStatus
// ──────────────────────────────────────────────────────────────────────────────

/// Current claim state of a bead from the live store.
///
/// Returned by `BeadStore::claim_status` for atomic verification operations.
/// The revision field is used for optimistic concurrency control (compare-and-set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimStatus {
    /// Current lifecycle status of the bead.
    pub status: BeadStatus,
    /// Current assignee, if any.
    pub assignee: Option<String>,
    /// Monotonic revision number for optimistic concurrency control.
    ///
    /// This value increments on every state change. Use it with `--if-revision`
    /// for atomic compare-and-set operations (bead-rs only).
    pub revision: Option<u64>,
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
        assert_eq!(BeadAction::Closed.to_string(), "closed");
        assert_eq!(BeadAction::Released.to_string(), "released");
        assert_eq!(BeadAction::Deferred.to_string(), "deferred");
        assert_eq!(BeadAction::Alerted.to_string(), "alerted");
        assert_eq!(BeadAction::Quarantined.to_string(), "quarantined");
        assert_eq!(BeadAction::Interrupted.to_string(), "interrupted");
        assert_eq!(BeadAction::Errored.to_string(), "errored");
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
        assert_eq!(
            proposal.validate().unwrap_err(),
            "phase_title cannot be empty"
        );
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
        assert_eq!(
            proposal.validate().unwrap_err(),
            "description cannot be empty"
        );
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
        assert_eq!(
            proposal.validate().unwrap_err(),
            "priority must be between 0 and 4"
        );
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

    // ──────────────────────────────────────────────────────────────────────────────
    // DecompositionDecision tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn decomposition_decision_split_is_split() {
        let decision = DecompositionDecision::Split {
            with_proposals: vec![],
        };
        assert!(decision.is_split());
        assert!(!decision.is_refuse());
    }

    #[test]
    fn decomposition_decision_refuse_is_refuse() {
        let decision = DecompositionDecision::Refuse {
            reason: "test refusal".to_string(),
        };
        assert!(!decision.is_split());
        assert!(decision.is_refuse());
    }

    #[test]
    fn decomposition_decision_proposals_split() {
        let proposals = vec![ChildBeadProposal::new(
            "Phase 1".to_string(),
            "Description".to_string(),
            vec![BeadId::from("needle-parent")],
            2,
        )
        .unwrap()];
        let decision = DecompositionDecision::Split {
            with_proposals: proposals.clone(),
        };
        assert_eq!(decision.proposals(), Some(&proposals));
    }

    #[test]
    fn decomposition_decision_proposals_refuse() {
        let decision = DecompositionDecision::Refuse {
            reason: "test".to_string(),
        };
        assert_eq!(decision.proposals(), None);
    }

    #[test]
    fn decomposition_decision_refusal_reason_refuse() {
        let reason = "bead is too small".to_string();
        let decision = DecompositionDecision::Refuse {
            reason: reason.clone(),
        };
        assert_eq!(decision.refusal_reason(), Some(&reason));
    }

    #[test]
    fn decomposition_decision_refusal_reason_split() {
        let decision = DecompositionDecision::Split {
            with_proposals: vec![],
        };
        assert_eq!(decision.refusal_reason(), None);
    }

    #[test]
    fn decomposition_decision_display_split() {
        let decision = DecompositionDecision::Split {
            with_proposals: vec![
                ChildBeadProposal::new(
                    "Phase 1".to_string(),
                    "Description 1".to_string(),
                    vec![BeadId::from("needle-parent")],
                    2,
                )
                .unwrap(),
                ChildBeadProposal::new(
                    "Phase 2".to_string(),
                    "Description 2".to_string(),
                    vec![BeadId::from("needle-parent")],
                    2,
                )
                .unwrap(),
            ],
        };
        assert_eq!(decision.to_string(), "split into 2 child beads");
    }

    #[test]
    fn decomposition_decision_display_refuse() {
        let decision = DecompositionDecision::Refuse {
            reason: "bead is too small".to_string(),
        };
        assert_eq!(decision.to_string(), "refuse: bead is too small");
    }

    #[test]
    fn decomposition_thresholds_default() {
        let thresholds = DecompositionThresholds::default();
        assert_eq!(thresholds.min_timeout_seconds, 300);
        assert_eq!(thresholds.min_retry_count, 2);
        assert_eq!(thresholds.min_bead_size, 200);
        assert_eq!(thresholds.child_labels.len(), 2);
        assert!(thresholds
            .child_labels
            .contains(&"mitosis-child".to_string()));
        assert!(thresholds
            .child_labels
            .contains(&"decomposition-child".to_string()));
    }

    fn make_test_bead(body: &str, assignee: Option<&str>, labels: Vec<&str>) -> Bead {
        Bead {
            id: BeadId::from("needle-test"),
            title: "Test bead".to_string(),
            body: Some(body.to_string()),
            priority: 2,
            status: BeadStatus::Open,
            assignee: assignee.map(|s| s.to_string()),
            labels: labels.into_iter().map(|s| s.to_string()).collect(),
            workspace: std::path::PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: DateTime::from_timestamp(0, 0).unwrap(),
            updated_at: DateTime::from_timestamp(0, 0).unwrap(),
        }
    }

    #[test]
    fn decompose_bead_decision_all_criteria_met() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );
        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(decision.is_split());
    }

    #[test]
    fn decompose_bead_decision_refuse_too_small() {
        let bead = make_test_bead("Tiny bead body", None, vec![]);
        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(decision.is_refuse());
        assert!(decision.refusal_reason().unwrap().contains("too small"));
    }

    #[test]
    fn decompose_bead_decision_refuse_has_assignee() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            Some("worker-01"),
            vec![],
        );
        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("active assignee"));
    }

    #[test]
    fn decompose_bead_decision_refuse_child_bead() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec!["mitosis-child"],
        );
        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("already a child"));
    }

    #[test]
    fn decompose_bead_decision_refuse_timeout_too_short() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );
        let decision = decompose_bead_decision(&bead, 60, 3, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("timeout too short"));
    }

    #[test]
    fn decompose_bead_decision_refuse_insufficient_retries() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );
        let decision = decompose_bead_decision(&bead, 600, 1, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("insufficient retries"));
    }

    #[test]
    fn decompose_bead_decision_custom_thresholds() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );

        // Custom thresholds: higher timeout requirement
        let custom_thresholds = DecompositionThresholds {
            min_timeout_seconds: 900,
            min_retry_count: 2,
            min_bead_size: 200,
            child_labels: vec!["mitosis-child".to_string()],
        };

        let decision = decompose_bead_decision(&bead, 600, 3, Some(custom_thresholds));
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("timeout too short"));
    }

    #[test]
    fn decompose_bead_decision_boundary_values() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );

        // Exactly at thresholds should split
        let decision = decompose_bead_decision(&bead, 300, 2, None);
        assert!(decision.is_split());

        // Just below timeout threshold should refuse
        let decision = decompose_bead_decision(&bead, 299, 2, None);
        assert!(decision.is_refuse());

        // Just below retry threshold should refuse
        let decision = decompose_bead_decision(&bead, 300, 1, None);
        assert!(decision.is_refuse());
    }

    #[test]
    fn decompose_bead_decision_empty_body() {
        let bead = make_test_bead("", None, vec![]);
        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(decision.is_refuse());
        assert!(decision.refusal_reason().unwrap().contains("too small"));
    }

    #[test]
    fn decompose_bead_decision_none_body() {
        let mut bead = make_test_bead("test", None, vec![]);
        bead.body = None;
        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(decision.is_refuse());
        assert!(decision.refusal_reason().unwrap().contains("too small"));
    }

    #[test]
    fn decompose_bead_decision_multiple_child_labels() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec!["other-label", "decomposition-child", "another-label"],
        );
        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("already a child"));
    }

    #[test]
    fn decompose_bead_decision_custom_child_labels() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec!["custom-child-label"],
        );

        let custom_thresholds = DecompositionThresholds {
            min_timeout_seconds: 300,
            min_retry_count: 2,
            min_bead_size: 200,
            child_labels: vec!["custom-child-label".to_string()],
        };

        let decision = decompose_bead_decision(&bead, 600, 3, Some(custom_thresholds));
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("already a child"));
    }

    #[test]
    fn decompose_bead_decision_timeout_exactly_at_threshold() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );
        // Timeout exactly at threshold (300s) with sufficient retries
        let decision = decompose_bead_decision(&bead, 300, 3, None);
        assert!(
            decision.is_split(),
            "Should split when timeout equals threshold"
        );
    }

    #[test]
    fn decompose_bead_decision_timeout_one_second_above_threshold() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );
        // Timeout one second above threshold (301s)
        let decision = decompose_bead_decision(&bead, 301, 3, None);
        assert!(
            decision.is_split(),
            "Should split when timeout exceeds threshold by 1s"
        );
    }

    #[test]
    fn decompose_bead_decision_timeout_one_second_below_threshold() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );
        // Timeout one second below threshold (299s)
        let decision = decompose_bead_decision(&bead, 299, 3, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("timeout too short"));
    }

    #[test]
    fn decompose_bead_decision_retry_exactly_at_threshold() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );
        // Retry count exactly at threshold (2)
        let decision = decompose_bead_decision(&bead, 600, 2, None);
        assert!(
            decision.is_split(),
            "Should split when retry_count equals threshold"
        );
    }

    #[test]
    fn decompose_bead_decision_retry_one_above_threshold() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );
        // Retry count one above threshold (3)
        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(
            decision.is_split(),
            "Should split when retry_count exceeds threshold"
        );
    }

    #[test]
    fn decompose_bead_decision_retry_one_below_threshold() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );
        // Retry count one below threshold (1)
        let decision = decompose_bead_decision(&bead, 600, 1, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("insufficient retries"));
    }

    #[test]
    fn decompose_bead_decision_body_size_exactly_at_threshold() {
        // Create a body that is exactly 200 characters
        let body = "a".repeat(200);
        let bead = make_test_bead(&body, None, vec![]);

        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(
            decision.is_split(),
            "Should split when body size equals threshold"
        );
    }

    #[test]
    fn decompose_bead_decision_body_size_one_above_threshold() {
        // Create a body that is 201 characters
        let body = "a".repeat(201);
        let bead = make_test_bead(&body, None, vec![]);

        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(
            decision.is_split(),
            "Should split when body size exceeds threshold by 1"
        );
    }

    #[test]
    fn decompose_bead_decision_body_size_one_below_threshold() {
        // Create a body that is 199 characters
        let body = "a".repeat(199);
        let bead = make_test_bead(&body, None, vec![]);

        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(decision.is_refuse());
        assert!(decision.refusal_reason().unwrap().contains("too small"));
    }

    #[test]
    fn decompose_bead_decision_decomposition_child_label() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec!["decomposition-child"],
        );
        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("already a child"));
    }

    #[test]
    fn decompose_bead_decision_both_child_labels_present() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec!["mitosis-child", "decomposition-child"],
        );
        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("already a child"));
    }

    #[test]
    fn decompose_bead_decision_all_criteria_at_exact_boundaries() {
        // All criteria exactly at their minimum thresholds
        let body = "a".repeat(200);
        let bead = make_test_bead(&body, None, vec![]);

        // timeout=300 (exact), retry_count=2 (exact), body_size=200 (exact)
        let decision = decompose_bead_decision(&bead, 300, 2, None);
        assert!(
            decision.is_split(),
            "Should split when all criteria are exactly at thresholds"
        );
    }

    #[test]
    fn decompose_bead_decision_all_criteria_one_above_thresholds() {
        // All criteria one unit above minimum thresholds
        let body = "a".repeat(201);
        let bead = make_test_bead(&body, None, vec![]);

        // timeout=301 (>300), retry_count=3 (>2), body_size=201 (>200)
        let decision = decompose_bead_decision(&bead, 301, 3, None);
        assert!(decision.is_split(), "Should split when all criteria exceed thresholds");
    }

    #[test]
    fn decompose_bead_decision_all_criteria_one_below_thresholds() {
        // All criteria one unit below minimum thresholds
        let body = "a".repeat(199);
        let bead = make_test_bead(&body, None, vec![]);

        // timeout=299 (<300) should fail first
        let decision = decompose_bead_decision(&bead, 299, 1, None);
        assert!(decision.is_refuse());
        // Should fail on timeout check (first threshold check)
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("timeout too short"));
    }

    #[test]
    fn decompose_bead_decision_zero_retries() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );

        // Zero retries should definitely refuse
        let decision = decompose_bead_decision(&bead, 600, 0, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("insufficient retries"));
    }

    #[test]
    fn decompose_bead_decision_zero_timeout() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );

        // Zero timeout should definitely refuse
        let decision = decompose_bead_decision(&bead, 0, 3, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("timeout too short"));
    }

    #[test]
    fn decompose_bead_decision_large_timeout_insufficient_retries() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );

        // Even with very large timeout, insufficient retries should refuse
        let decision = decompose_bead_decision(&bead, 3600, 1, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("insufficient retries"));
    }

    #[test]
    fn decompose_bead_decision_many_retries_short_timeout() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );

        // Even with many retries, short timeout should refuse
        let decision = decompose_bead_decision(&bead, 10, 100, None);
        assert!(decision.is_refuse());
        assert!(decision
            .refusal_reason()
            .unwrap()
            .contains("timeout too short"));
    }

    #[test]
    fn decompose_bead_decision_split_returns_empty_proposals() {
        let bead = make_test_bead(
            "This is a substantial bead body that exceeds 200 characters. "
                .repeat(4)
                .as_str(),
            None,
            vec![],
        );

        let decision = decompose_bead_decision(&bead, 600, 3, None);
        assert!(decision.is_split());

        // Split decision should have empty proposals (caller must populate)
        let proposals = decision.proposals().unwrap();
        assert!(proposals.is_empty(), "Initial split decision should have empty proposals");
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

/// Terminal action produced by the outcome handler for a claimed bead.
///
/// There is deliberately no `None` variant: once dispatch reaches outcome
/// handling, the bead must be closed, released, quarantined, interrupted, or
/// routed through explicit error recovery.  The worker consumes this value at
/// the state-machine boundary and verifies that the bead is no longer held.
#[must_use = "a BeadAction must be applied before the dispatch cycle can advance"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeadAction {
    /// The agent closed the bead and the handler confirmed the closed state.
    Closed,
    /// Bead was released back to open status.
    Released,
    /// Bead was deferred (e.g., timeout with deferred label).
    Deferred,
    /// An alert bead was created.
    Alerted,
    /// Bead was quarantined (status=blocked, labeled `cycling`) after
    /// exceeding the consecutive-failure threshold.
    Quarantined,
    /// Bead was released because the worker was interrupted and must stop.
    Interrupted,
    /// Normal outcome handling failed; the worker must run release recovery.
    Errored,
}

impl fmt::Display for BeadAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BeadAction::Closed => write!(f, "closed"),
            BeadAction::Released => write!(f, "released"),
            BeadAction::Deferred => write!(f, "deferred"),
            BeadAction::Alerted => write!(f, "alerted"),
            BeadAction::Quarantined => write!(f, "quarantined"),
            BeadAction::Interrupted => write!(f, "interrupted"),
            BeadAction::Errored => write!(f, "errored"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// HandlerResult
// ──────────────────────────────────────────────────────────────────────────────

/// Result of handling an agent outcome.
#[must_use = "the handler result contains a BeadAction that must be applied"]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    ERROR_CODE_CATEGORIES
        .iter()
        .find_map(|(known_code, category)| (*known_code == code).then_some(*category))
        .unwrap_or(ErrorCategory::Unknown)
}

const ERROR_CODE_CATEGORIES: &[(&str, ErrorCategory)] = &[
    // Keep the mapping explicit. In particular, do not classify unassigned
    // E0807+ values by range; rustc does not emit those error codes.
    // Type mismatch errors
    ("E0308", ErrorCategory::TypeMismatch),
    ("E0309", ErrorCategory::TypeMismatch),
    ("E0310", ErrorCategory::TypeMismatch),
    ("E0311", ErrorCategory::TypeMismatch),
    ("E0312", ErrorCategory::TypeMismatch),
    ("E0313", ErrorCategory::TypeMismatch),
    ("E0314", ErrorCategory::TypeMismatch),
    ("E0315", ErrorCategory::TypeMismatch),
    ("E0316", ErrorCategory::TypeMismatch),
    ("E0317", ErrorCategory::TypeMismatch),
    ("E0369", ErrorCategory::TypeMismatch),
    ("E0370", ErrorCategory::TypeMismatch),
    // Borrow checker and lifetime errors
    ("E0382", ErrorCategory::BorrowChecker),
    ("E0502", ErrorCategory::BorrowChecker),
    ("E0503", ErrorCategory::BorrowChecker),
    ("E0505", ErrorCategory::BorrowChecker),
    ("E0506", ErrorCategory::BorrowChecker),
    ("E0507", ErrorCategory::BorrowChecker),
    ("E0508", ErrorCategory::BorrowChecker),
    ("E0509", ErrorCategory::BorrowChecker),
    ("E0510", ErrorCategory::BorrowChecker),
    ("E0511", ErrorCategory::BorrowChecker),
    ("E0512", ErrorCategory::BorrowChecker),
    ("E0515", ErrorCategory::BorrowChecker),
    ("E0516", ErrorCategory::BorrowChecker),
    ("E0517", ErrorCategory::BorrowChecker),
    ("E0597", ErrorCategory::BorrowChecker),
    ("E0623", ErrorCategory::BorrowChecker),
    ("E0624", ErrorCategory::BorrowChecker),
    ("E0625", ErrorCategory::BorrowChecker),
    ("E0626", ErrorCategory::BorrowChecker),
    ("E0716", ErrorCategory::BorrowChecker),
    ("E0782", ErrorCategory::BorrowChecker),
    ("E0783", ErrorCategory::BorrowChecker),
    // Trait implementation errors
    ("E0038", ErrorCategory::TraitImpl),
    ("E0046", ErrorCategory::TraitImpl),
    ("E0117", ErrorCategory::TraitImpl),
    ("E0118", ErrorCategory::TraitImpl),
    ("E0119", ErrorCategory::TraitImpl),
    ("E0120", ErrorCategory::TraitImpl),
    ("E0183", ErrorCategory::TraitImpl),
    ("E0207", ErrorCategory::TraitImpl),
    ("E0210", ErrorCategory::TraitImpl),
    ("E0220", ErrorCategory::TraitImpl),
    ("E0227", ErrorCategory::TraitImpl),
    ("E0229", ErrorCategory::TraitImpl),
    ("E0230", ErrorCategory::TraitImpl),
    ("E0277", ErrorCategory::TraitImpl),
    ("E0365", ErrorCategory::TraitImpl),
    ("E0366", ErrorCategory::TraitImpl),
    ("E0367", ErrorCategory::TraitImpl),
    ("E0368", ErrorCategory::TraitImpl),
    ("E0381", ErrorCategory::TraitImpl),
    ("E0390", ErrorCategory::TraitImpl),
    ("E0391", ErrorCategory::TraitImpl),
    ("E0412", ErrorCategory::TraitImpl),
    ("E0423", ErrorCategory::TraitImpl),
    ("E0437", ErrorCategory::TraitImpl),
    ("E0558", ErrorCategory::TraitImpl),
    ("E0574", ErrorCategory::TraitImpl),
    ("E0647", ErrorCategory::TraitImpl),
    ("E0699", ErrorCategory::TraitImpl),
    ("E0708", ErrorCategory::TraitImpl),
    ("E0719", ErrorCategory::TraitImpl),
    ("E0781", ErrorCategory::TraitImpl),
    // Pattern matching errors
    ("E0002", ErrorCategory::PatternMatching),
    ("E0009", ErrorCategory::PatternMatching),
    ("E0007", ErrorCategory::PatternMatching),
    ("E0010", ErrorCategory::PatternMatching),
    ("E0011", ErrorCategory::PatternMatching),
    ("E0012", ErrorCategory::PatternMatching),
    ("E0013", ErrorCategory::PatternMatching),
    ("E0014", ErrorCategory::PatternMatching),
    ("E0015", ErrorCategory::PatternMatching),
    ("E0016", ErrorCategory::PatternMatching),
    ("E0017", ErrorCategory::PatternMatching),
    ("E0018", ErrorCategory::PatternMatching),
    ("E0019", ErrorCategory::PatternMatching),
    ("E0022", ErrorCategory::PatternMatching),
    ("E0023", ErrorCategory::PatternMatching),
    ("E0024", ErrorCategory::PatternMatching),
    ("E0025", ErrorCategory::PatternMatching),
    ("E0026", ErrorCategory::PatternMatching),
    ("E0031", ErrorCategory::PatternMatching),
    ("E0033", ErrorCategory::PatternMatching),
    ("E0034", ErrorCategory::PatternMatching),
    ("E0035", ErrorCategory::PatternMatching),
    ("E0039", ErrorCategory::PatternMatching),
    ("E0040", ErrorCategory::PatternMatching),
    ("E0044", ErrorCategory::PatternMatching),
    ("E0052", ErrorCategory::PatternMatching),
    ("E0054", ErrorCategory::PatternMatching),
    ("E0055", ErrorCategory::PatternMatching),
    ("E0162", ErrorCategory::PatternMatching),
    ("E0163", ErrorCategory::PatternMatching),
    ("E0164", ErrorCategory::PatternMatching),
    ("E0165", ErrorCategory::PatternMatching),
    ("E0302", ErrorCategory::PatternMatching),
    ("E0409", ErrorCategory::PatternMatching),
    ("E0422", ErrorCategory::PatternMatching),
    ("E0424", ErrorCategory::PatternMatching),
    ("E0513", ErrorCategory::PatternMatching),
    ("E0529", ErrorCategory::PatternMatching),
    ("E0616", ErrorCategory::PatternMatching),
    ("E0617", ErrorCategory::PatternMatching),
    ("E0618", ErrorCategory::PatternMatching),
    ("E0639", ErrorCategory::PatternMatching),
    ("E0640", ErrorCategory::PatternMatching),
    ("E0641", ErrorCategory::PatternMatching),
    ("E0642", ErrorCategory::PatternMatching),
    ("E0643", ErrorCategory::PatternMatching),
    ("E0644", ErrorCategory::PatternMatching),
    // Scope and visibility errors
    ("E0403", ErrorCategory::ScopeVisibility),
    ("E0404", ErrorCategory::ScopeVisibility),
    ("E0405", ErrorCategory::ScopeVisibility),
    ("E0406", ErrorCategory::ScopeVisibility),
    ("E0407", ErrorCategory::ScopeVisibility),
    ("E0408", ErrorCategory::ScopeVisibility),
    ("E0411", ErrorCategory::ScopeVisibility),
    ("E0413", ErrorCategory::ScopeVisibility),
    ("E0414", ErrorCategory::ScopeVisibility),
    ("E0415", ErrorCategory::ScopeVisibility),
    ("E0501", ErrorCategory::ScopeVisibility),
    ("E0583", ErrorCategory::ScopeVisibility),
    ("E0603", ErrorCategory::ScopeVisibility),
    ("E0604", ErrorCategory::ScopeVisibility),
    ("E0605", ErrorCategory::ScopeVisibility),
    ("E0606", ErrorCategory::ScopeVisibility),
    ("E0607", ErrorCategory::ScopeVisibility),
    ("E0608", ErrorCategory::ScopeVisibility),
    ("E0609", ErrorCategory::ScopeVisibility),
    ("E0610", ErrorCategory::ScopeVisibility),
    ("E0611", ErrorCategory::ScopeVisibility),
    ("E0612", ErrorCategory::ScopeVisibility),
    ("E0613", ErrorCategory::ScopeVisibility),
    ("E0614", ErrorCategory::ScopeVisibility),
    ("E0615", ErrorCategory::ScopeVisibility),
    ("E0621", ErrorCategory::ScopeVisibility),
    ("E0622", ErrorCategory::ScopeVisibility),
    ("E0631", ErrorCategory::ScopeVisibility),
    ("E0633", ErrorCategory::ScopeVisibility),
    ("E0634", ErrorCategory::ScopeVisibility),
    ("E0636", ErrorCategory::ScopeVisibility),
    ("E0742", ErrorCategory::ScopeVisibility),
    ("E0743", ErrorCategory::ScopeVisibility),
    ("E0744", ErrorCategory::ScopeVisibility),
    ("E0745", ErrorCategory::ScopeVisibility),
    ("E0750", ErrorCategory::ScopeVisibility),
    ("E0758", ErrorCategory::ScopeVisibility),
    ("E0759", ErrorCategory::ScopeVisibility),
    ("E0760", ErrorCategory::ScopeVisibility),
    ("E0761", ErrorCategory::ScopeVisibility),
    ("E0762", ErrorCategory::ScopeVisibility),
    ("E0763", ErrorCategory::ScopeVisibility),
    ("E0764", ErrorCategory::ScopeVisibility),
    ("E0765", ErrorCategory::ScopeVisibility),
    ("E0766", ErrorCategory::ScopeVisibility),
    ("E0767", ErrorCategory::ScopeVisibility),
    ("E0768", ErrorCategory::ScopeVisibility),
    ("E0769", ErrorCategory::ScopeVisibility),
    ("E0770", ErrorCategory::ScopeVisibility),
    ("E0771", ErrorCategory::ScopeVisibility),
    ("E0772", ErrorCategory::ScopeVisibility),
    ("E0773", ErrorCategory::ScopeVisibility),
    ("E0774", ErrorCategory::ScopeVisibility),
    ("E0775", ErrorCategory::ScopeVisibility),
    ("E0776", ErrorCategory::ScopeVisibility),
    ("E0777", ErrorCategory::ScopeVisibility),
    ("E0778", ErrorCategory::ScopeVisibility),
    ("E0779", ErrorCategory::ScopeVisibility),
    ("E0780", ErrorCategory::ScopeVisibility),
    ("E0790", ErrorCategory::ScopeVisibility),
    ("E0791", ErrorCategory::ScopeVisibility),
    ("E0792", ErrorCategory::ScopeVisibility),
    ("E0793", ErrorCategory::ScopeVisibility),
    ("E0794", ErrorCategory::ScopeVisibility),
    ("E0795", ErrorCategory::ScopeVisibility),
    ("E0796", ErrorCategory::ScopeVisibility),
    ("E0797", ErrorCategory::ScopeVisibility),
    ("E0798", ErrorCategory::ScopeVisibility),
    ("E0799", ErrorCategory::ScopeVisibility),
    // Syntax errors
    ("E0053", ErrorCategory::Syntax),
    ("E0060", ErrorCategory::Syntax),
    ("E0061", ErrorCategory::Syntax),
    ("E0062", ErrorCategory::Syntax),
    ("E0063", ErrorCategory::Syntax),
    ("E0066", ErrorCategory::Syntax),
    ("E0070", ErrorCategory::Syntax),
    ("E0071", ErrorCategory::Syntax),
    ("E0072", ErrorCategory::Syntax),
    ("E0073", ErrorCategory::Syntax),
    ("E0075", ErrorCategory::Syntax),
    ("E0076", ErrorCategory::Syntax),
    ("E0077", ErrorCategory::Syntax),
    ("E0078", ErrorCategory::Syntax),
    ("E0079", ErrorCategory::Syntax),
    ("E0080", ErrorCategory::Syntax),
    ("E0081", ErrorCategory::Syntax),
    ("E0082", ErrorCategory::Syntax),
    ("E0085", ErrorCategory::Syntax),
    ("E0087", ErrorCategory::Syntax),
    ("E0106", ErrorCategory::Syntax),
    ("E0116", ErrorCategory::Syntax),
    ("E0124", ErrorCategory::Syntax),
    ("E0131", ErrorCategory::Syntax),
    ("E0133", ErrorCategory::Syntax),
    ("E0161", ErrorCategory::Syntax),
    ("E0175", ErrorCategory::Syntax),
    ("E0201", ErrorCategory::Syntax),
    ("E0204", ErrorCategory::Syntax),
    ("E0205", ErrorCategory::Syntax),
    ("E0206", ErrorCategory::Syntax),
    ("E0211", ErrorCategory::Syntax),
    ("E0214", ErrorCategory::Syntax),
    ("E0225", ErrorCategory::Syntax),
    ("E0226", ErrorCategory::Syntax),
    ("E0231", ErrorCategory::Syntax),
    ("E0254", ErrorCategory::Syntax),
    ("E0255", ErrorCategory::Syntax),
    ("E0256", ErrorCategory::Syntax),
    ("E0257", ErrorCategory::Syntax),
    ("E0258", ErrorCategory::Syntax),
    ("E0259", ErrorCategory::Syntax),
    ("E0260", ErrorCategory::Syntax),
    ("E0261", ErrorCategory::Syntax),
    ("E0262", ErrorCategory::Syntax),
    ("E0263", ErrorCategory::Syntax),
    ("E0264", ErrorCategory::Syntax),
    ("E0267", ErrorCategory::Syntax),
    ("E0268", ErrorCategory::Syntax),
    ("E0275", ErrorCategory::Syntax),
    ("E0281", ErrorCategory::Syntax),
    ("E0282", ErrorCategory::Syntax),
    ("E0301", ErrorCategory::Syntax),
    ("E0306", ErrorCategory::Syntax),
    ("E0324", ErrorCategory::Syntax),
    ("E0328", ErrorCategory::Syntax),
    ("E0378", ErrorCategory::Syntax),
    ("E0379", ErrorCategory::Syntax),
    ("E0401", ErrorCategory::Syntax),
    ("E0402", ErrorCategory::Syntax),
    ("E0428", ErrorCategory::Syntax),
    ("E0430", ErrorCategory::Syntax),
    ("E0433", ErrorCategory::Syntax),
    ("E0434", ErrorCategory::Syntax),
    ("E0435", ErrorCategory::Syntax),
    ("E0436", ErrorCategory::Syntax),
    ("E0438", ErrorCategory::Syntax),
    ("E0439", ErrorCategory::Syntax),
    ("E0440", ErrorCategory::Syntax),
    ("E0441", ErrorCategory::Syntax),
    ("E0442", ErrorCategory::Syntax),
    ("E0443", ErrorCategory::Syntax),
    ("E0444", ErrorCategory::Syntax),
    ("E0445", ErrorCategory::Syntax),
    ("E0446", ErrorCategory::Syntax),
    ("E0447", ErrorCategory::Syntax),
    ("E0448", ErrorCategory::Syntax),
    ("E0449", ErrorCategory::Syntax),
    ("E0450", ErrorCategory::Syntax),
    ("E0451", ErrorCategory::Syntax),
    ("E0452", ErrorCategory::Syntax),
    ("E0453", ErrorCategory::Syntax),
    ("E0454", ErrorCategory::Syntax),
    ("E0455", ErrorCategory::Syntax),
    ("E0456", ErrorCategory::Syntax),
    ("E0457", ErrorCategory::Syntax),
    ("E0458", ErrorCategory::Syntax),
    ("E0459", ErrorCategory::Syntax),
    ("E0460", ErrorCategory::Syntax),
    ("E0461", ErrorCategory::Syntax),
    ("E0462", ErrorCategory::Syntax),
    ("E0463", ErrorCategory::Syntax),
    ("E0464", ErrorCategory::Syntax),
    ("E0465", ErrorCategory::Syntax),
    ("E0466", ErrorCategory::Syntax),
    ("E0467", ErrorCategory::Syntax),
    ("E0468", ErrorCategory::Syntax),
    ("E0469", ErrorCategory::Syntax),
    ("E0470", ErrorCategory::Syntax),
    ("E0471", ErrorCategory::Syntax),
    ("E0472", ErrorCategory::Syntax),
    ("E0473", ErrorCategory::Syntax),
    ("E0474", ErrorCategory::Syntax),
    ("E0475", ErrorCategory::Syntax),
    ("E0476", ErrorCategory::Syntax),
    ("E0477", ErrorCategory::Syntax),
    ("E0478", ErrorCategory::Syntax),
    ("E0479", ErrorCategory::Syntax),
    ("E0480", ErrorCategory::Syntax),
    ("E0481", ErrorCategory::Syntax),
    ("E0482", ErrorCategory::Syntax),
    ("E0483", ErrorCategory::Syntax),
    ("E0484", ErrorCategory::Syntax),
    ("E0485", ErrorCategory::Syntax),
    ("E0486", ErrorCategory::Syntax),
    ("E0487", ErrorCategory::Syntax),
    ("E0488", ErrorCategory::Syntax),
    ("E0489", ErrorCategory::Syntax),
    ("E0490", ErrorCategory::Syntax),
    ("E0491", ErrorCategory::Syntax),
    ("E0492", ErrorCategory::Syntax),
    ("E0493", ErrorCategory::Syntax),
    ("E0494", ErrorCategory::Syntax),
    ("E0495", ErrorCategory::Syntax),
    ("E0496", ErrorCategory::Syntax),
    ("E0497", ErrorCategory::Syntax),
    ("E0498", ErrorCategory::Syntax),
    ("E0499", ErrorCategory::Syntax),
    ("E0518", ErrorCategory::Syntax),
    ("E0524", ErrorCategory::Syntax),
    ("E0525", ErrorCategory::Syntax),
    ("E0527", ErrorCategory::Syntax),
    ("E0528", ErrorCategory::Syntax),
    ("E0531", ErrorCategory::Syntax),
    ("E0534", ErrorCategory::Syntax),
    ("E0536", ErrorCategory::Syntax),
    ("E0537", ErrorCategory::Syntax),
    ("E0539", ErrorCategory::Syntax),
    ("E0545", ErrorCategory::Syntax),
    ("E0546", ErrorCategory::Syntax),
    ("E0547", ErrorCategory::Syntax),
    ("E0548", ErrorCategory::Syntax),
    ("E0550", ErrorCategory::Syntax),
    ("E0551", ErrorCategory::Syntax),
    ("E0552", ErrorCategory::Syntax),
    ("E0553", ErrorCategory::Syntax),
    ("E0554", ErrorCategory::Syntax),
    ("E0556", ErrorCategory::Syntax),
    ("E0557", ErrorCategory::Syntax),
    ("E0559", ErrorCategory::Syntax),
    ("E0560", ErrorCategory::Syntax),
    ("E0561", ErrorCategory::Syntax),
    ("E0562", ErrorCategory::Syntax),
    ("E0565", ErrorCategory::Syntax),
    ("E0566", ErrorCategory::Syntax),
    ("E0567", ErrorCategory::Syntax),
    ("E0568", ErrorCategory::Syntax),
    ("E0569", ErrorCategory::Syntax),
    ("E0570", ErrorCategory::Syntax),
    ("E0571", ErrorCategory::Syntax),
    ("E0572", ErrorCategory::Syntax),
    ("E0573", ErrorCategory::Syntax),
    ("E0575", ErrorCategory::Syntax),
    ("E0576", ErrorCategory::Syntax),
    ("E0577", ErrorCategory::Syntax),
    ("E0578", ErrorCategory::Syntax),
    ("E0579", ErrorCategory::Syntax),
    ("E0580", ErrorCategory::Syntax),
    ("E0581", ErrorCategory::Syntax),
    ("E0582", ErrorCategory::Syntax),
    ("E0584", ErrorCategory::Syntax),
    ("E0585", ErrorCategory::Syntax),
    ("E0586", ErrorCategory::Syntax),
    ("E0587", ErrorCategory::Syntax),
    ("E0588", ErrorCategory::Syntax),
    ("E0589", ErrorCategory::Syntax),
    ("E0590", ErrorCategory::Syntax),
    ("E0591", ErrorCategory::Syntax),
    ("E0592", ErrorCategory::Syntax),
    ("E0593", ErrorCategory::Syntax),
    ("E0594", ErrorCategory::Syntax),
    ("E0595", ErrorCategory::Syntax),
    ("E0596", ErrorCategory::Syntax),
    ("E0598", ErrorCategory::Syntax),
    ("E0599", ErrorCategory::Syntax),
    ("E0601", ErrorCategory::Syntax),
    ("E0619", ErrorCategory::Syntax),
    ("E0620", ErrorCategory::Syntax),
    ("E0628", ErrorCategory::Syntax),
    ("E0629", ErrorCategory::Syntax),
    ("E0630", ErrorCategory::Syntax),
    ("E0632", ErrorCategory::Syntax),
    ("E0635", ErrorCategory::Syntax),
    ("E0637", ErrorCategory::Syntax),
    ("E0638", ErrorCategory::Syntax),
    ("E0645", ErrorCategory::Syntax),
    ("E0646", ErrorCategory::Syntax),
    ("E0648", ErrorCategory::Syntax),
    ("E0649", ErrorCategory::Syntax),
    ("E0650", ErrorCategory::Syntax),
    ("E0651", ErrorCategory::Syntax),
    ("E0652", ErrorCategory::Syntax),
    ("E0653", ErrorCategory::Syntax),
    ("E0654", ErrorCategory::Syntax),
    ("E0655", ErrorCategory::Syntax),
    ("E0656", ErrorCategory::Syntax),
    ("E0657", ErrorCategory::Syntax),
    ("E0658", ErrorCategory::Syntax),
    ("E0659", ErrorCategory::Syntax),
    ("E0660", ErrorCategory::Syntax),
    ("E0661", ErrorCategory::Syntax),
    ("E0662", ErrorCategory::Syntax),
    ("E0663", ErrorCategory::Syntax),
    ("E0664", ErrorCategory::Syntax),
    ("E0665", ErrorCategory::Syntax),
    ("E0666", ErrorCategory::Syntax),
    ("E0667", ErrorCategory::Syntax),
    ("E0668", ErrorCategory::Syntax),
    ("E0669", ErrorCategory::Syntax),
    ("E0670", ErrorCategory::Syntax),
    ("E0671", ErrorCategory::Syntax),
    ("E0672", ErrorCategory::Syntax),
    ("E0673", ErrorCategory::Syntax),
    ("E0674", ErrorCategory::Syntax),
    ("E0675", ErrorCategory::Syntax),
    ("E0676", ErrorCategory::Syntax),
    ("E0677", ErrorCategory::Syntax),
    ("E0678", ErrorCategory::Syntax),
    ("E0679", ErrorCategory::Syntax),
    ("E0680", ErrorCategory::Syntax),
    ("E0681", ErrorCategory::Syntax),
    ("E0682", ErrorCategory::Syntax),
    ("E0683", ErrorCategory::Syntax),
    ("E0684", ErrorCategory::Syntax),
    ("E0685", ErrorCategory::Syntax),
    ("E0686", ErrorCategory::Syntax),
    ("E0687", ErrorCategory::Syntax),
    ("E0688", ErrorCategory::Syntax),
    ("E0689", ErrorCategory::Syntax),
    ("E0690", ErrorCategory::Syntax),
    ("E0691", ErrorCategory::Syntax),
    ("E0692", ErrorCategory::Syntax),
    ("E0693", ErrorCategory::Syntax),
    ("E0694", ErrorCategory::Syntax),
    ("E0695", ErrorCategory::Syntax),
    ("E0696", ErrorCategory::Syntax),
    ("E0697", ErrorCategory::Syntax),
    ("E0698", ErrorCategory::Syntax),
    ("E0701", ErrorCategory::Syntax),
    ("E0702", ErrorCategory::Syntax),
    ("E0703", ErrorCategory::Syntax),
    ("E0705", ErrorCategory::Syntax),
    ("E0706", ErrorCategory::Syntax),
    ("E0707", ErrorCategory::Syntax),
    ("E0709", ErrorCategory::Syntax),
    ("E0710", ErrorCategory::Syntax),
    ("E0712", ErrorCategory::Syntax),
    ("E0713", ErrorCategory::Syntax),
    ("E0714", ErrorCategory::Syntax),
    ("E0715", ErrorCategory::Syntax),
    ("E0717", ErrorCategory::Syntax),
    ("E0718", ErrorCategory::Syntax),
    ("E0720", ErrorCategory::Syntax),
    ("E0721", ErrorCategory::Syntax),
    ("E0722", ErrorCategory::Syntax),
    ("E0723", ErrorCategory::Syntax),
    ("E0724", ErrorCategory::Syntax),
    ("E0725", ErrorCategory::Syntax),
    ("E0726", ErrorCategory::Syntax),
    ("E0727", ErrorCategory::Syntax),
    ("E0728", ErrorCategory::Syntax),
    ("E0729", ErrorCategory::Syntax),
    ("E0730", ErrorCategory::Syntax),
    ("E0731", ErrorCategory::Syntax),
    ("E0732", ErrorCategory::Syntax),
    ("E0733", ErrorCategory::Syntax),
    ("E0734", ErrorCategory::Syntax),
    ("E0735", ErrorCategory::Syntax),
    ("E0736", ErrorCategory::Syntax),
    ("E0737", ErrorCategory::Syntax),
    ("E0738", ErrorCategory::Syntax),
    ("E0739", ErrorCategory::Syntax),
    ("E0740", ErrorCategory::Syntax),
    ("E0741", ErrorCategory::Syntax),
    ("E0746", ErrorCategory::Syntax),
    ("E0747", ErrorCategory::Syntax),
    ("E0748", ErrorCategory::Syntax),
    ("E0749", ErrorCategory::Syntax),
    ("E0751", ErrorCategory::Syntax),
    ("E0752", ErrorCategory::Syntax),
    ("E0753", ErrorCategory::Syntax),
    ("E0754", ErrorCategory::Syntax),
    ("E0755", ErrorCategory::Syntax),
    ("E0756", ErrorCategory::Syntax),
    ("E0757", ErrorCategory::Syntax),
    ("E0800", ErrorCategory::Syntax),
    ("E0801", ErrorCategory::Syntax),
    ("E0802", ErrorCategory::Syntax),
    ("E0803", ErrorCategory::Syntax),
    ("E0804", ErrorCategory::Syntax),
    ("E0805", ErrorCategory::Syntax),
    ("E0806", ErrorCategory::Syntax),
    // Generic and const parameter errors
    ("E0392", ErrorCategory::Generic),
    ("E0393", ErrorCategory::Generic),
    ("E0394", ErrorCategory::Generic),
    ("E0395", ErrorCategory::Generic),
    ("E0396", ErrorCategory::Generic),
    ("E0397", ErrorCategory::Generic),
    ("E0398", ErrorCategory::Generic),
    ("E0399", ErrorCategory::Generic),
    ("E0400", ErrorCategory::Generic),
    ("E0563", ErrorCategory::Generic),
    ("E0564", ErrorCategory::Generic),
    // Macro expansion errors
    ("E0276", ErrorCategory::Macro),
    ("E0519", ErrorCategory::Macro),
    ("E0520", ErrorCategory::Macro),
    ("E0521", ErrorCategory::Macro),
    ("E0522", ErrorCategory::Macro),
    ("E0523", ErrorCategory::Macro),
    ("E0704", ErrorCategory::Macro),
    ("E0784", ErrorCategory::Macro),
    ("E0785", ErrorCategory::Macro),
    ("E0786", ErrorCategory::Macro),
    ("E0787", ErrorCategory::Macro),
    ("E0788", ErrorCategory::Macro),
    ("E0789", ErrorCategory::Macro),
    // Dead code or unused item errors
    ("E0425", ErrorCategory::DeadCode),
    ("E0526", ErrorCategory::DeadCode),
];

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
fn classify_error_code_table_has_unique_codes() {
    let mut seen = std::collections::HashSet::new();
    for &(code, _) in ERROR_CODE_CATEGORIES {
        assert!(seen.insert(code), "duplicate error code in table: {code}");
    }

    assert_eq!(seen.len(), ERROR_CODE_CATEGORIES.len());
    assert_eq!(classify_error_code("E0807"), ErrorCategory::Unknown);
    assert_eq!(classify_error_code("E0999"), ErrorCategory::Unknown);
}

#[test]
fn classify_error_code_overlap_categories_are_explicit() {
    let cases = [
        ("E0748", ErrorCategory::Syntax),
        ("E0749", ErrorCategory::Syntax),
        ("E0750", ErrorCategory::ScopeVisibility),
        ("E0751", ErrorCategory::Syntax),
        ("E0758", ErrorCategory::ScopeVisibility),
        ("E0761", ErrorCategory::ScopeVisibility),
        ("E0781", ErrorCategory::TraitImpl),
        ("E0782", ErrorCategory::BorrowChecker),
        ("E0783", ErrorCategory::BorrowChecker),
        ("E0784", ErrorCategory::Macro),
    ];

    for (code, expected) in cases {
        assert_eq!(
            classify_error_code(code),
            expected,
            "wrong category for {code}"
        );
    }
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
    assert_eq!(reason.explanation(), "Full test suite cannot be decomposed");
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
    assert_eq!(
        parsed.estimated_duration_secs,
        phase.estimated_duration_secs
    );
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
            DecompositionProposal::Timeout {
                phases,
                refusal_reason,
                ..
            } => refusal_reason.is_none() && !phases.is_empty(),
            DecompositionProposal::Ordinary {
                tasks,
                refusal_reason,
                ..
            } => refusal_reason.is_none() && !tasks.is_empty(),
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
    #[allow(clippy::match_like_matches_macro)]
    pub fn suggests_productive_work(&self) -> bool {
        self.has_activity_evidence
            || match &self.timeout_context {
                TimeoutContext::Compilation { .. } => true,
                TimeoutContext::TestExecution {
                    tests_passing: true,
                    ..
                } => true,
                TimeoutContext::Analysis {
                    making_progress: true,
                    ..
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
                    if *making_progress {
                        "progressing"
                    } else {
                        "stalled"
                    }
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

// ──────────────────────────────────────────────────────────────────────────────
// Resolve Decision Types
// ──────────────────────────────────────────────────────────────────────────────

/// Decision from the Resolve strand about bead disposition.
///
/// The Resolve strand analyzes beads after Pluck selection to determine
/// whether they should proceed immediately, be deferred, or be decomposed.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveDecision {
    /// Bead is ready for immediate dispatch and implementation.
    Complete,
    /// Bead cannot proceed right now but may succeed later (transient condition).
    Retry,
    /// Bead has an unmet dependency that blocks all progress.
    Blocked,
    /// Bead is too large/complex and should be split into smaller child beads.
    Split,
}

impl fmt::Display for ResolveDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveDecision::Complete => write!(f, "complete"),
            ResolveDecision::Retry => write!(f, "retry"),
            ResolveDecision::Blocked => write!(f, "blocked"),
            ResolveDecision::Split => write!(f, "split"),
        }
    }
}

/// Structured outcome from Resolve decision analysis.
///
/// Each variant carries evidence supporting the decision and any
/// decision-specific fields required for downstream handling.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveOutcome {
    /// Bead cleared for immediate dispatch.
    Complete {
        /// Evidence supporting the complete decision.
        evidence: String,
    },
    /// Bead should be retried after a delay (transient condition).
    Retry {
        /// Evidence supporting the retry decision.
        evidence: String,
        /// Seconds to wait before retry (default: 600).
        retry_after_seconds: u64,
    },
    /// Bead is blocked by an unmet dependency.
    Blocked {
        /// Evidence supporting the blocked decision.
        evidence: String,
        /// ID of the blocking bead.
        blocker_id: BeadId,
    },
    /// Bead should be split into smaller child beads.
    Split {
        /// Evidence supporting the split decision.
        evidence: String,
        /// Explanation of why splitting is needed.
        split_reason: Option<String>,
    },
}

impl ResolveOutcome {
    /// Returns the evidence string for this outcome.
    pub fn evidence(&self) -> &str {
        match self {
            ResolveOutcome::Complete { evidence } => evidence,
            ResolveOutcome::Retry { evidence, .. } => evidence,
            ResolveOutcome::Blocked { evidence, .. } => evidence,
            ResolveOutcome::Split { evidence, .. } => evidence,
        }
    }

    /// Returns the decision variant corresponding to this outcome.
    pub fn decision(&self) -> ResolveDecision {
        match self {
            ResolveOutcome::Complete { .. } => ResolveDecision::Complete,
            ResolveOutcome::Retry { .. } => ResolveDecision::Retry,
            ResolveOutcome::Blocked { .. } => ResolveDecision::Blocked,
            ResolveOutcome::Split { .. } => ResolveDecision::Split,
        }
    }
}

impl fmt::Display for ResolveOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveOutcome::Complete { evidence } => {
                write!(f, "complete: {}", evidence)
            }
            ResolveOutcome::Retry {
                evidence,
                retry_after_seconds,
            } => {
                write!(f, "retry after {}s: {}", retry_after_seconds, evidence)
            }
            ResolveOutcome::Blocked {
                evidence,
                blocker_id,
            } => {
                write!(f, "blocked by {}: {}", blocker_id, evidence)
            }
            ResolveOutcome::Split {
                evidence,
                split_reason,
            } => {
                if let Some(reason) = split_reason {
                    write!(f, "split ({}): {}", reason, evidence)
                } else {
                    write!(f, "split: {}", evidence)
                }
            }
        }
    }
}
