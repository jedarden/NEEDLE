//! Resolve decision contract: post-Pluck outcome analysis.
//!
//! This module implements a structured decision system that runs after a Pluck
//! operation completes. The resolver analyzes the agent's work and determines
//! the appropriate next action, with strict validation and fail-safe defaults.
//!
//! ## Decision Flow
//!
//! 1. Agent completes Pluck (exit code 0 or non-zero)
//! 2. Resolver receives agent output and context
//! 3. Structured prompt is built from the resolve template
//! 4. Agent must respond with valid JSON matching the schema
//! 5. Parser validates the response exhaustively
//! 6. Invalid/incomplete/unknown decisions fall back to safe defaults
//!
//! ## Decision Types
//!
//! - **Complete**: Task succeeded, bead should be closed
//! - **Retry**: Temporary failure, release for immediate retry
//! - **Blocked**: External dependency, cannot proceed without human input
//! - **Split**: Task decomposes into multiple independent subtasks
//!
//! ## Safety Guarantees
//!
//! - Resolver NEVER implements changes or mutates beads
//! - Invalid JSON → fallback to Retry (safe default)
//! - Unknown decision type → fallback to Retry
//! - Missing required fields → fallback to Retry
//! - Timeout → fallback to Retry
//!
//! Depends on: `types`, `prompt`, `config`, `telemetry`.

use std::fmt;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::prompt::PromptBuilder;
use crate::telemetry::Telemetry;
use crate::types::Bead;

// ──────────────────────────────────────────────────────────────────────────────
// ResolveDecision enum
// ──────────────────────────────────────────────────────────────────────────────

/// The four exhaustive resolve decision types.
///
/// Every possible post-Pluck outcome maps to exactly one of these decisions.
/// No wildcards in matching — all variants must be explicitly handled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveDecision {
    /// Task completed successfully — close the bead.
    Complete {
        /// Evidence that the task succeeded (git commits, tests passed, etc.)
        evidence: String,
        /// Human-readable summary of what was accomplished.
        summary: String,
    },

    /// Temporary failure — release bead for immediate retry.
    Retry {
        /// Why the attempt failed (error message, diagnostic info).
        reason: String,
        /// Suggested retry strategy (same approach, different timeout, etc.).
        strategy: RetryStrategy,
    },

    /// Blocked by external dependency — cannot proceed without human input.
    Blocked {
        /// What external resource is blocking progress (API, human decision, etc).
        blocker: String,
        /// What action is needed from a human (approve plan, provide credentials, etc).
        required_action: String,
        /// How critical this block is to project progress.
        severity: BlockSeverity,
    },

    /// Task decomposes into multiple independent subtasks.
    Split {
        /// Proposed child beads (independent, sequentially completable).
        children: Vec<ProposedChild>,
        /// Rationale for why this decomposition is appropriate.
        rationale: String,
    },
}

impl ResolveDecision {
    /// Returns the decision type as a string for telemetry.
    pub fn as_str(&self) -> &'static str {
        match self {
            ResolveDecision::Complete { .. } => "complete",
            ResolveDecision::Retry { .. } => "retry",
            ResolveDecision::Blocked { .. } => "blocked",
            ResolveDecision::Split { .. } => "split",
        }
    }

    /// Validate the decision has all required fields populated.
    pub fn validate(&self) -> Result<()> {
        match self {
            ResolveDecision::Complete { evidence, summary } => {
                if evidence.trim().is_empty() {
                    bail!("Complete decision missing evidence");
                }
                if summary.trim().is_empty() {
                    bail!("Complete decision missing summary");
                }
                Ok(())
            }
            ResolveDecision::Retry { reason, strategy } => {
                if reason.trim().is_empty() {
                    bail!("Retry decision missing reason");
                }
                strategy.validate()?;
                Ok(())
            }
            ResolveDecision::Blocked {
                blocker,
                required_action,
                severity,
            } => {
                if blocker.trim().is_empty() {
                    bail!("Blocked decision missing blocker");
                }
                if required_action.trim().is_empty() {
                    bail!("Blocked decision missing required_action");
                }
                severity.validate()?;
                Ok(())
            }
            ResolveDecision::Split {
                children,
                rationale,
            } => {
                if children.is_empty() {
                    bail!("Split decision must have at least one child");
                }
                if children.len() > 10 {
                    bail!("Split decision cannot have more than 10 children");
                }
                for (idx, child) in children.iter().enumerate() {
                    child.validate(idx)?;
                }
                if rationale.trim().is_empty() {
                    bail!("Split decision missing rationale");
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for ResolveDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveDecision::Complete { summary, .. } => {
                write!(f, "Complete: {}", summary)
            }
            ResolveDecision::Retry { reason, .. } => {
                write!(f, "Retry: {}", reason)
            }
            ResolveDecision::Blocked {
                blocker,
                required_action,
                ..
            } => {
                write!(f, "Blocked on {}: {}", blocker, required_action)
            }
            ResolveDecision::Split { children, .. } => {
                write!(f, "Split into {} child tasks", children.len())
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// RetryStrategy
// ──────────────────────────────────────────────────────────────────────────────

/// Strategy for retrying a failed attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryStrategy {
    /// Same approach should work (transient failure, rate limit, etc).
    Same,
    /// Increase timeout for next attempt.
    IncreaseTimeout,
    /// Different approach needed (implementation incorrect, API changed, etc).
    DifferentApproach,
    /// Resource unavailable (service down, dependency missing, etc).
    Backoff,
}

impl RetryStrategy {
    fn validate(&self) -> Result<()> {
        // All variants are valid by construction
        Ok(())
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            RetryStrategy::Same => "same",
            RetryStrategy::IncreaseTimeout => "increase_timeout",
            RetryStrategy::DifferentApproach => "different_approach",
            RetryStrategy::Backoff => "backoff",
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// BlockSeverity
// ──────────────────────────────────────────────────────────────────────────────

/// How critical a blocker is to project progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockSeverity {
    /// Cosmetic issue, nice-to-have enhancement, or low-priority cleanup.
    Low,
    /// Feature complete but blocked on polish, tests, or documentation.
    Medium,
    /// Critical blocker preventing core functionality or release.
    High,
}

impl BlockSeverity {
    fn validate(&self) -> Result<()> {
        // All variants are valid by construction
        Ok(())
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            BlockSeverity::Low => "low",
            BlockSeverity::Medium => "medium",
            BlockSeverity::High => "high",
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ProposedChild
// ──────────────────────────────────────────────────────────────────────────────

/// A proposed child bead from a Split decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedChild {
    /// Short title for the child task.
    pub title: String,
    /// Detailed description and acceptance criteria.
    pub body: String,
    /// Recommended priority (1-4, lower is higher priority).
    pub priority: u8,
}

impl ProposedChild {
    fn validate(&self, idx: usize) -> Result<()> {
        if self.title.trim().is_empty() {
            bail!("Child {} has empty title", idx);
        }
        if self.body.trim().is_empty() {
            bail!("Child {} ({}) has empty body", idx, self.title);
        }
        if !(1..=4).contains(&self.priority) {
            bail!(
                "Child {} ({}) has invalid priority {}, must be 1-4",
                idx,
                self.title,
                self.priority
            );
        }
        if self.title.len() > 200 {
            bail!(
                "Child {} title too long ({} chars, max 200)",
                idx,
                self.title.len()
            );
        }
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ResolveResponse
// ──────────────────────────────────────────────────────────────────────────────

/// The structured response schema for the resolve prompt.
///
/// Agents must respond with exactly this structure. All fields are required
/// for the decision to be valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveResponse {
    /// The decision type (one of: complete, retry, blocked, split).
    pub decision: ResolveDecision,
}

impl ResolveResponse {
    /// Parse and validate a JSON response string.
    ///
    /// Returns an error if:
    /// - JSON is malformed
    /// - Structure doesn't match ResolveResponse
    /// - Decision is invalid per validate()
    pub fn parse_and_validate(json: &str) -> Result<Self> {
        // First, try to parse as JSON
        let response: Self = serde_json::from_str(json)
            .with_context(|| "failed to parse resolve response as JSON")?;

        // Then validate the decision structure
        response
            .decision
            .validate()
            .with_context(|| "parsed decision failed validation")?;

        Ok(response)
    }

    /// Check if the response JSON is valid without fully parsing.
    ///
    /// This is a cheap pre-check before attempting full parsing.
    pub fn looks_valid(json: &str) -> bool {
        // Quick heuristic checks
        let json = json.trim();

        // Must be non-empty and start with {
        if json.is_empty() || !json.starts_with('{') {
            return false;
        }

        // Must contain a decision field
        if !json.contains("\"decision\"") {
            return false;
        }

        // Must contain one of the decision type markers
        json.contains("\"complete\"")
            || json.contains("\"retry\"")
            || json.contains("\"blocked\"")
            || json.contains("\"split\"")
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ResolveContext
// ──────────────────────────────────────────────────────────────────────────────

/// Context passed to the resolver for decision-making.
#[derive(Debug, Clone)]
pub struct ResolveContext<'a> {
    /// The bead being resolved.
    pub bead: &'a Bead,
    /// Agent exit code from Pluck.
    pub exit_code: i32,
    /// Agent stdout output.
    pub stdout: String,
    /// Agent stderr output.
    pub stderr: String,
    /// Duration of the Pluck operation.
    pub duration: Duration,
    /// Timestamp when Pluck started.
    pub started_at: DateTime<Utc>,
    /// Whether the operation was interrupted (SIGINT/SIGTERM).
    pub was_interrupted: bool,
}

impl<'a> ResolveContext<'a> {
    /// Create a new resolve context.
    pub fn new(
        bead: &'a Bead,
        exit_code: i32,
        stdout: String,
        stderr: String,
        duration: Duration,
        started_at: DateTime<Utc>,
        was_interrupted: bool,
    ) -> Self {
        Self {
            bead,
            exit_code,
            stdout,
            stderr,
            duration,
            started_at,
            was_interrupted,
        }
    }

    /// Format the duration for display in the prompt.
    pub fn formatted_duration(&self) -> String {
        let secs = self.duration.as_secs();
        if secs >= 3600 {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        } else if secs >= 60 {
            format!("{}m {}s", secs / 60, secs % 60)
        } else {
            format!("{}s", secs)
        }
    }

    /// Get a truncated version of stdout for the prompt (max 2000 chars).
    pub fn truncated_stdout(&self) -> String {
        self.stdout.chars().take(2000).collect()
    }

    /// Get a truncated version of stderr for the prompt (max 2000 chars).
    pub fn truncated_stderr(&self) -> String {
        self.stderr.chars().take(2000).collect()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// VerificationError
// ──────────────────────────────────────────────────────────────────────────────

/// Error type for binary identity verification failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationError {
    /// Binary identity verification failed.
    VerificationFailed(String),
    /// Verification not supported for this binary type.
    NotSupported(String),
}

impl fmt::Display for VerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerificationError::VerificationFailed(msg) => {
                write!(f, "Verification failed: {}", msg)
            }
            VerificationError::NotSupported(msg) => {
                write!(f, "Verification not supported: {}", msg)
            }
        }
    }
}

impl std::error::Error for VerificationError {}

// ──────────────────────────────────────────────────────────────────────────────
// Resolver
// ──────────────────────────────────────────────────────────────────────────────

/// The resolver analyzes Pluck outcomes and determines next actions.
pub struct Resolver {
    /// Timeout for resolve agent calls (default 120s).
    timeout: Duration,
    /// Prompt builder for constructing the resolve prompt.
    prompt_builder: PromptBuilder,
    /// Telemetry client for emitting events.
    telemetry: Option<Telemetry>,
}

impl Resolver {
    /// Create a new resolver with default timeout.
    pub fn new(prompt_builder: PromptBuilder) -> Self {
        Self {
            timeout: Duration::from_secs(120),
            prompt_builder,
            telemetry: None,
        }
    }

    /// Set the timeout for resolve agent calls.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the telemetry client for event emission.
    pub fn with_telemetry(mut self, telemetry: Telemetry) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    /// Resolve a Pluck outcome into a structured decision.
    ///
    /// This method:
    /// 1. Builds the resolve prompt with context
    /// 2. Invokes an agent to analyze and decide
    /// 3. Parses and validates the response
    /// 4. Verifies binary identity after successful resolution
    /// 5. Returns the decision or a safe fallback
    ///
    /// # Safety
    ///
    /// Any failure (timeout, invalid JSON, parse error, validation error, verification failure)
    /// returns a safe fallback decision (Retry with generic reason) rather
    /// than propagating the error. This ensures the worker can always continue.
    pub async fn resolve(&self, context: &ResolveContext<'_>) -> ResolveDecision {
        tracing::info!(
            bead_id = %context.bead.id,
            exit_code = context.exit_code,
            "resolver: starting analysis"
        );

        // Build the resolve prompt
        let prompt = match self.build_prompt(context) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    bead_id = %context.bead.id,
                    error = %e,
                    "resolver: failed to build prompt, using fallback"
                );
                return self.fallback_decision("prompt_build_failed");
            }
        };

        // Invoke the resolve agent with timeout
        let response_text = match self.invoke_resolve_agent(&prompt).await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(
                    bead_id = %context.bead.id,
                    error = %e,
                    "resolver: agent invocation failed, using fallback"
                );
                return self.fallback_decision("agent_invocation_failed");
            }
        };

        // Parse and validate the response
        let decision = match self.parse_and_validate_response(&response_text) {
            Ok(decision) => decision,
            Err(e) => {
                tracing::warn!(
                    bead_id = %context.bead.id,
                    error = %e,
                    response = %response_text,
                    "resolver: response parsing/validation failed, using fallback"
                );
                return self.fallback_decision("response_validation_failed");
            }
        };

        // Verify binary identity after successful resolution
        if let Err(e) = self.verify_binary_identity(&decision) {
            tracing::warn!(
                bead_id = %context.bead.id,
                error = %e,
                "resolver: binary identity verification failed, using fallback"
            );
            return ResolveDecision::Retry {
                reason: format!("Binary identity verification failed: {}", e),
                strategy: RetryStrategy::DifferentApproach,
            };
        }

        tracing::info!(
            bead_id = %context.bead.id,
            decision = %decision.as_str(),
            "resolver: decision successfully determined"
        );

        decision
    }

    /// Invoke the resolve agent with the given prompt.
    ///
    /// Returns the agent's raw text response or an error.
    async fn invoke_resolve_agent(&self, prompt: &str) -> Result<String> {
        use std::process::Stdio;
        use tokio::process::Command as AsyncCommand;

        tracing::debug!(
            prompt_length = prompt.len(),
            "resolver: invoking resolve agent"
        );

        // Use claude CLI for resolve analysis
        let output = tokio::time::timeout(self.timeout, async {
            let child = AsyncCommand::new("claude")
                .arg("--message")
                .arg(prompt)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("failed to spawn resolve agent")?;

            child
                .wait_with_output()
                .await
                .context("failed to wait for resolve agent")
        })
        .await
        .context("resolve agent invocation timed out")??;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            anyhow::bail!(
                "resolve agent exited with {}: {}",
                output.status,
                stderr.trim()
            );
        }

        tracing::debug!(
            response_length = stdout.len(),
            "resolver: agent response received"
        );

        Ok(stdout)
    }

    /// Parse and validate the agent's response text.
    ///
    /// Returns the validated decision or an error.
    fn parse_and_validate_response(&self, response_text: &str) -> Result<ResolveDecision> {
        // Strip markdown code fencing if present
        let cleaned = response_text
            .trim()
            .trim_start_matches("```json")
            .trim_end_matches("```")
            .trim();

        // Try to parse as ResolveResponse
        let response = ResolveResponse::parse_and_validate(cleaned)?;

        // Return the decision (already validated by parse_and_validate)
        Ok(response.decision)
    }

    /// Build the resolve prompt from context.
    fn build_prompt(&self, context: &ResolveContext<'_>) -> Result<String> {
        let worker_id = "resolver"; // Resolver doesn't have a worker ID

        let exit_status = if context.exit_code == 0 {
            "success"
        } else {
            "failure"
        };
        let was_interrupted = if context.was_interrupted {
            "true"
        } else {
            "false"
        };

        // Build the prompt with resolve-specific variables using the dedicated method
        let built = self.prompt_builder.build_resolve(
            context.bead,
            &context.bead.workspace,
            worker_id,
            context.exit_code,
            &context.formatted_duration(),
            &context.truncated_stdout(),
            &context.truncated_stderr(),
            exit_status,
            was_interrupted,
        )?;

        Ok(built.content)
    }

    /// Return a safe fallback decision.
    ///
    /// This is used when:
    /// - Prompt building fails
    /// - Agent invocation times out
    /// - JSON parsing fails
    /// - Decision validation fails
    fn fallback_decision(&self, reason: &str) -> ResolveDecision {
        ResolveDecision::Retry {
            reason: format!(
                "Resolver error: {}. Safe fallback: retry with same approach.",
                reason
            ),
            strategy: RetryStrategy::Same,
        }
    }

    /// Verify binary identity after successful resolution.
    ///
    /// This is a placeholder hook for binary identity verification.
    /// Currently returns Ok(()) as a stub implementation.
    ///
    /// # Future Implementation
    ///
    /// This method should:
    /// - Verify the binary matches expected checksums
    /// - Validate binary signatures
    /// - Check binary integrity
    /// - Ensure the binary is from a trusted source
    ///
    /// # Returns
    ///
    /// - `Ok(())` - Verification passed
    /// - `Err(VerificationError)` - Verification failed
    fn verify_binary_identity(&self, _decision: &ResolveDecision) -> Result<(), VerificationError> {
        // Stub implementation - always passes for now
        // TODO: Implement actual verification logic
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BeadId, BeadStatus};
    use chrono::Utc;
    use std::path::PathBuf;
    use std::time::Duration;

    fn test_bead() -> Bead {
        Bead {
            id: BeadId::from("needle-test"),
            title: "Test bead".to_string(),
            body: Some("Test body".to_string()),
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some("worker-01".to_string()),
            labels: vec![],
            workspace: PathBuf::from("/tmp/test-workspace"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn resolve_decision_display_formats_correctly() {
        let complete = ResolveDecision::Complete {
            evidence: "Commit abc123".to_string(),
            summary: "Implemented feature".to_string(),
        };
        assert_eq!(format!("{}", complete), "Complete: Implemented feature");

        let retry = ResolveDecision::Retry {
            reason: "Network timeout".to_string(),
            strategy: RetryStrategy::IncreaseTimeout,
        };
        assert_eq!(format!("{}", retry), "Retry: Network timeout");

        let blocked = ResolveDecision::Blocked {
            blocker: "API key".to_string(),
            required_action: "Provide credentials".to_string(),
            severity: BlockSeverity::High,
        };
        assert!(format!("{}", blocked).contains("Blocked on API key"));

        let split = ResolveDecision::Split {
            children: vec![ProposedChild {
                title: "Child 1".to_string(),
                body: "Description".to_string(),
                priority: 1,
            }],
            rationale: "Two independent tasks".to_string(),
        };
        assert_eq!(format!("{}", split), "Split into 1 child tasks");
    }

    #[test]
    fn resolve_decision_as_str_returns_correct_strings() {
        assert_eq!(
            ResolveDecision::Complete {
                evidence: "x".to_string(),
                summary: "y".to_string(),
            }
            .as_str(),
            "complete"
        );

        assert_eq!(
            ResolveDecision::Retry {
                reason: "x".to_string(),
                strategy: RetryStrategy::Same,
            }
            .as_str(),
            "retry"
        );

        assert_eq!(
            ResolveDecision::Blocked {
                blocker: "x".to_string(),
                required_action: "y".to_string(),
                severity: BlockSeverity::Medium,
            }
            .as_str(),
            "blocked"
        );

        assert_eq!(
            ResolveDecision::Split {
                children: vec![],
                rationale: "x".to_string(),
            }
            .as_str(),
            "split"
        );
    }

    #[test]
    fn complete_decision_requires_all_fields() {
        let decision = ResolveDecision::Complete {
            evidence: "Commit abc123".to_string(),
            summary: "Implemented feature".to_string(),
        };
        assert!(decision.validate().is_ok());

        let invalid = ResolveDecision::Complete {
            evidence: "".to_string(),
            summary: "Implemented feature".to_string(),
        };
        assert!(invalid.validate().is_err());

        let invalid = ResolveDecision::Complete {
            evidence: "Commit abc123".to_string(),
            summary: "".to_string(),
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn retry_decision_requires_all_fields() {
        let decision = ResolveDecision::Retry {
            reason: "Network timeout".to_string(),
            strategy: RetryStrategy::IncreaseTimeout,
        };
        assert!(decision.validate().is_ok());

        let invalid = ResolveDecision::Retry {
            reason: "".to_string(),
            strategy: RetryStrategy::Same,
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn blocked_decision_requires_all_fields() {
        let decision = ResolveDecision::Blocked {
            blocker: "API key".to_string(),
            required_action: "Provide credentials".to_string(),
            severity: BlockSeverity::High,
        };
        assert!(decision.validate().is_ok());

        let invalid = ResolveDecision::Blocked {
            blocker: "".to_string(),
            required_action: "Provide credentials".to_string(),
            severity: BlockSeverity::Medium,
        };
        assert!(invalid.validate().is_err());

        let invalid = ResolveDecision::Blocked {
            blocker: "API key".to_string(),
            required_action: "".to_string(),
            severity: BlockSeverity::Low,
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn split_decision_requires_children_and_rationale() {
        let decision = ResolveDecision::Split {
            children: vec![ProposedChild {
                title: "Child 1".to_string(),
                body: "Description".to_string(),
                priority: 1,
            }],
            rationale: "Two independent tasks".to_string(),
        };
        assert!(decision.validate().is_ok());

        let invalid = ResolveDecision::Split {
            children: vec![],
            rationale: "No children".to_string(),
        };
        assert!(invalid.validate().is_err());

        let invalid = ResolveDecision::Split {
            children: vec![ProposedChild {
                title: "Child 1".to_string(),
                body: "Description".to_string(),
                priority: 1,
            }],
            rationale: "".to_string(),
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn split_decision_limits_child_count() {
        let children: Vec<_> = (0..11)
            .map(|i| ProposedChild {
                title: format!("Child {}", i),
                body: format!("Description {}", i),
                priority: 1,
            })
            .collect();

        let invalid = ResolveDecision::Split {
            children,
            rationale: "Too many children".to_string(),
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn proposed_child_validation() {
        let valid = ProposedChild {
            title: "Valid child".to_string(),
            body: "Valid description".to_string(),
            priority: 1,
        };
        assert!(valid.validate(0).is_ok());

        let invalid_title = ProposedChild {
            title: "".to_string(),
            body: "Description".to_string(),
            priority: 1,
        };
        assert!(invalid_title.validate(0).is_err());

        let invalid_body = ProposedChild {
            title: "Title".to_string(),
            body: "".to_string(),
            priority: 1,
        };
        assert!(invalid_body.validate(0).is_err());

        let invalid_priority = ProposedChild {
            title: "Title".to_string(),
            body: "Description".to_string(),
            priority: 0,
        };
        assert!(invalid_priority.validate(0).is_err());

        let invalid_priority = ProposedChild {
            title: "Title".to_string(),
            body: "Description".to_string(),
            priority: 5,
        };
        assert!(invalid_priority.validate(0).is_err());

        let too_long_title = ProposedChild {
            title: "a".repeat(201),
            body: "Description".to_string(),
            priority: 1,
        };
        assert!(too_long_title.validate(0).is_err());
    }

    #[test]
    fn resolve_response_parse_and_validate() {
        let json = r#"{"decision":{"complete":{"evidence":"Commit abc123","summary":"Done"}}}"#;
        let response = ResolveResponse::parse_and_validate(json).unwrap();
        assert!(matches!(
            response.decision,
            ResolveDecision::Complete { .. }
        ));

        let invalid_json = "not json";
        assert!(ResolveResponse::parse_and_validate(invalid_json).is_err());

        let missing_fields = r#"{"decision":{"complete":{}}}"#;
        assert!(ResolveResponse::parse_and_validate(missing_fields).is_err());
    }

    #[test]
    fn resolve_response_looks_valid_heuristic() {
        assert!(ResolveResponse::looks_valid(
            r#"{"decision":{"complete":{"evidence":"x","summary":"y"}}}"#
        ));

        assert!(!ResolveResponse::looks_valid("not json"));
        assert!(!ResolveResponse::looks_valid("{}"));
        assert!(!ResolveResponse::looks_valid(r#"{"other":"field"}"#));
    }

    #[test]
    fn retry_strategy_as_str() {
        assert_eq!(RetryStrategy::Same.as_str(), "same");
        assert_eq!(RetryStrategy::IncreaseTimeout.as_str(), "increase_timeout");
        assert_eq!(
            RetryStrategy::DifferentApproach.as_str(),
            "different_approach"
        );
        assert_eq!(RetryStrategy::Backoff.as_str(), "backoff");
    }

    #[test]
    fn block_severity_as_str() {
        assert_eq!(BlockSeverity::Low.as_str(), "low");
        assert_eq!(BlockSeverity::Medium.as_str(), "medium");
        assert_eq!(BlockSeverity::High.as_str(), "high");
    }

    #[test]
    fn resolve_context_helpers() {
        let bead = test_bead();
        let bead_ref = Box::leak(Box::new(bead));

        let context = ResolveContext::new(
            bead_ref,
            0,
            "stdout".to_string(),
            "stderr".to_string(),
            Duration::from_secs(3665), // 1h 1m 5s
            Utc::now(),
            false,
        );

        assert_eq!(context.formatted_duration(), "1h 1m");
        assert_eq!(context.truncated_stdout(), "stdout");
        assert_eq!(context.truncated_stderr(), "stderr");
    }

    #[test]
    fn resolve_context_truncates_long_output() {
        let bead = test_bead();
        let bead_ref = Box::leak(Box::new(bead));

        let long_output = "x".repeat(3000);
        let context = ResolveContext::new(
            bead_ref,
            0,
            long_output.clone(),
            long_output.clone(),
            Duration::from_secs(60),
            Utc::now(),
            false,
        );

        assert!(context.truncated_stdout().len() <= 2000);
        assert!(context.truncated_stderr().len() <= 2000);
    }

    #[tokio::test]
    async fn resolver_returns_fallback_on_prompt_build_failure() {
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder);

        // Create a context that will fail to build a prompt (invalid bead)
        let invalid_bead = Bead {
            id: BeadId::from("needle-test"),
            title: "Test".to_string(),
            body: Some("Test".to_string()),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec![],
            workspace: PathBuf::from("/nonexistent"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let bead_ref = Box::leak(Box::new(invalid_bead));
        let context = ResolveContext::new(
            bead_ref,
            0,
            "stdout".to_string(),
            "stderr".to_string(),
            Duration::from_secs(60),
            Utc::now(),
            false,
        );

        // This should return a fallback decision, not panic
        let decision = resolver.resolve(&context).await;
        assert!(matches!(decision, ResolveDecision::Retry { .. }));
    }

    #[test]
    fn resolver_timeout_is_configurable() {
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder).with_timeout(Duration::from_secs(300));

        assert_eq!(resolver.timeout.as_secs(), 300);
    }

    #[test]
    fn fallback_decision_has_safe_retry_strategy() {
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder);

        let fallback = resolver.fallback_decision("test_reason");

        assert!(matches!(
            fallback,
            ResolveDecision::Retry {
                strategy: RetryStrategy::Same,
                ..
            }
        ));
        if let ResolveDecision::Retry { reason, strategy } = fallback {
            assert!(reason.contains("Resolver error: test_reason"));
            assert_eq!(strategy, RetryStrategy::Same);
        }
    }

    #[test]
    fn verify_binary_identity_stub_always_succeeds() {
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder);

        let decision = ResolveDecision::Complete {
            evidence: "Test evidence".to_string(),
            summary: "Test summary".to_string(),
        };

        let result = resolver.verify_binary_identity(&decision);
        assert!(result.is_ok());
    }

    #[test]
    fn verification_error_can_be_created_and_displayed() {
        let error = VerificationError::VerificationFailed("checksum mismatch".to_string());
        assert!(error.to_string().contains("checksum mismatch"));

        let error = VerificationError::NotSupported("binary type".to_string());
        assert!(error.to_string().contains("not supported"));
    }

    #[tokio::test]
    async fn resolver_calls_verification_after_resolution() {
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder);

        let bead = test_bead();
        let bead_ref = Box::leak(Box::new(bead));

        let context = ResolveContext::new(
            bead_ref,
            0,
            "stdout".to_string(),
            "stderr".to_string(),
            Duration::from_secs(60),
            Utc::now(),
            false,
        );

        // The resolver should call verification (even though it's a stub that passes)
        let decision = resolver.resolve(&context).await;
        assert!(matches!(decision, ResolveDecision::Retry { .. }));
    }

    #[test]
    fn parse_and_validate_complete_decision() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json = r#"{"decision":{"complete":{"evidence":"Commit abc123","summary":"Done"}}}"#;
        let decision = resolver.parse_and_validate_response(json).unwrap();

        assert!(matches!(decision, ResolveDecision::Complete { .. }));
        if let ResolveDecision::Complete { evidence, summary } = decision {
            assert_eq!(evidence, "Commit abc123");
            assert_eq!(summary, "Done");
        }
    }

    #[test]
    fn parse_and_validate_retry_decision() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json =
            r#"{"decision":{"retry":{"reason":"Network timeout","strategy":"increase_timeout"}}}"#;
        let decision = resolver.parse_and_validate_response(json).unwrap();

        assert!(matches!(decision, ResolveDecision::Retry { .. }));
        if let ResolveDecision::Retry { reason, strategy } = decision {
            assert_eq!(reason, "Network timeout");
            assert_eq!(strategy, RetryStrategy::IncreaseTimeout);
        }
    }

    #[test]
    fn parse_and_validate_blocked_decision() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json = r#"{"decision":{"blocked":{"blocker":"API key","required_action":"Provide credentials","severity":"high"}}}"#;
        let decision = resolver.parse_and_validate_response(json).unwrap();

        assert!(matches!(decision, ResolveDecision::Blocked { .. }));
        if let ResolveDecision::Blocked {
            blocker,
            required_action,
            severity,
        } = decision
        {
            assert_eq!(blocker, "API key");
            assert_eq!(required_action, "Provide credentials");
            assert_eq!(severity, BlockSeverity::High);
        }
    }

    #[test]
    fn parse_and_validate_split_decision() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json = r#"{"decision":{"split":{"children":[{"title":"Child 1","body":"Description","priority":1}],"rationale":"Two tasks"}}}"#;
        let decision = resolver.parse_and_validate_response(json).unwrap();

        assert!(matches!(decision, ResolveDecision::Split { .. }));
        if let ResolveDecision::Split {
            children,
            rationale,
        } = decision
        {
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].title, "Child 1");
            assert_eq!(rationale, "Two tasks");
        }
    }

    #[test]
    fn parse_and_validate_strips_markdown_fencing() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json = r#"```json
{"decision":{"complete":{"evidence":"Commit abc123","summary":"Done"}}}
```"#;
        let decision = resolver.parse_and_validate_response(json).unwrap();

        assert!(matches!(decision, ResolveDecision::Complete { .. }));
    }

    #[test]
    fn parse_and_validate_rejects_invalid_json() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json = "not valid json";
        let result = resolver.parse_and_validate_response(json);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("parse"));
    }

    #[test]
    fn parse_and_validate_rejects_missing_fields() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Missing evidence field
        let json = r#"{"decision":{"complete":{"summary":"Done"}}}"#;
        let result = resolver.parse_and_validate_response(json);

        assert!(result.is_err());
        // Check the error chain - missing fields cause JSON parse failure
        let err = result.unwrap_err();
        let err_msg = format!("{:?}", err); // Use debug format to see full error chain
        assert!(
            err_msg.contains("evidence")
                || err_msg.contains("missing field")
                || err_msg.contains("key")
        );
    }

    #[test]
    fn parse_and_validate_rejects_invalid_decision_type() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json = r#"{"decision":{"invalid_type":{"field":"value"}}}"#;
        let result = resolver.parse_and_validate_response(json);

        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_rejects_empty_strings() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Empty evidence
        let json = r#"{"decision":{"complete":{"evidence":"","summary":"Done"}}}"#;
        let result = resolver.parse_and_validate_response(json);

        assert!(result.is_err());
        // Check the error chain - empty strings cause validation failure
        let err = result.unwrap_err();
        let err_msg = format!("{:?}", err); // Use debug format to see full error chain
        assert!(err_msg.contains("evidence") || err_msg.contains("Complete decision missing"));
    }

    #[tokio::test]
    async fn invoke_resolve_agent_times_out() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ))
        .with_timeout(Duration::from_millis(100));

        // Create a very long prompt to ensure processing takes time
        let long_prompt = "x".repeat(100000);
        let result = resolver.invoke_resolve_agent(&long_prompt).await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();

        // invoke_resolve_agent() spawns the `claude` CLI. Where that binary exists the
        // 100ms budget is what fails, and asserting on "timed out" is meaningful. CI
        // images do not ship it, so the call fails at spawn() instead and never reaches
        // the timeout -- this test asserted the timeout unconditionally and so passed
        // only on machines that happen to have `claude` installed. Assert whichever
        // failure the environment can actually produce. See needle-ab52a15a.
        if which::which("claude").is_ok() {
            assert!(
                err_msg.contains("timed out"),
                "with the claude CLI present the 100ms budget should be what fails; got: {err_msg}"
            );
        } else {
            assert!(
                err_msg.contains("failed to spawn resolve agent"),
                "without the claude CLI the call should fail at spawn; got: {err_msg}"
            );
        }
    }
}
