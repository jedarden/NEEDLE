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

use crate::bead_store::backend::BeadBackend;
use crate::prompt::PromptBuilder;
use crate::telemetry::Telemetry;
use crate::types::Bead;
use std::path::PathBuf;

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
        /// Commit message for the changes made.
        commit_message: String,
    },

    /// Temporary failure — release bead for immediate retry.
    Retry {
        /// Evidence about what failed (error message, diagnostic info).
        evidence: String,
        /// Suggested retry strategy (same approach, different timeout, etc.).
        strategy: String,
    },

    /// Blocked by external dependency — cannot proceed without human input.
    Blocked {
        /// Evidence about the blocker (logs, error messages, etc).
        evidence: String,
        /// Type of blocker (API, human decision, resource, etc).
        blocker_type: String,
        /// Description of what is blocking progress.
        description: String,
    },

    /// Task decomposes into multiple independent subtasks.
    Split {
        /// Evidence supporting the split decision (analysis, complexity assessment, etc).
        evidence: String,
        /// Parent bead ID that this split originates from.
        parent_bead_id: String,
        /// Titles of the proposed child beads.
        child_titles: Vec<String>,
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
            ResolveDecision::Complete {
                evidence,
                commit_message,
            } => {
                if evidence.trim().is_empty() {
                    bail!("Complete decision missing evidence");
                }
                if commit_message.trim().is_empty() {
                    bail!("Complete decision missing commit_message");
                }
                Ok(())
            }
            ResolveDecision::Retry { evidence, strategy } => {
                if evidence.trim().is_empty() {
                    bail!("Retry decision missing evidence");
                }
                if strategy.trim().is_empty() {
                    bail!("Retry decision missing strategy");
                }
                Ok(())
            }
            ResolveDecision::Blocked {
                evidence,
                blocker_type,
                description,
            } => {
                if evidence.trim().is_empty() {
                    bail!("Blocked decision missing evidence");
                }
                if blocker_type.trim().is_empty() {
                    bail!("Blocked decision missing blocker_type");
                }
                if description.trim().is_empty() {
                    bail!("Blocked decision missing description");
                }
                Ok(())
            }
            ResolveDecision::Split {
                evidence,
                parent_bead_id,
                child_titles,
            } => {
                if evidence.trim().is_empty() {
                    bail!("Split decision missing evidence");
                }
                if parent_bead_id.trim().is_empty() {
                    bail!("Split decision missing parent_bead_id");
                }
                if child_titles.is_empty() {
                    bail!("Split decision must have at least one child title");
                }
                if child_titles.len() > 10 {
                    bail!("Split decision cannot have more than 10 child titles");
                }
                for (idx, title) in child_titles.iter().enumerate() {
                    if title.trim().is_empty() {
                        bail!("Child {} has empty title", idx);
                    }
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for ResolveDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveDecision::Complete { commit_message, .. } => {
                write!(f, "Complete: {}", commit_message)
            }
            ResolveDecision::Retry { evidence, .. } => {
                write!(f, "Retry: {}", evidence)
            }
            ResolveDecision::Blocked {
                blocker_type,
                description,
                ..
            } => {
                write!(f, "Blocked on {}: {}", blocker_type, description)
            }
            ResolveDecision::Split { child_titles, .. } => {
                write!(f, "Split into {} child tasks", child_titles.len())
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
    #[allow(dead_code)]
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
    pub fn as_str(&self) -> &'static str {
        match self {
            BlockSeverity::Low => "low",
            BlockSeverity::Medium => "medium",
            BlockSeverity::High => "high",
        }
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
    /// Resolve configuration.
    config: crate::config::ResolveConfig,
    /// Prompt builder for constructing the resolve prompt.
    prompt_builder: PromptBuilder,
    /// Telemetry client for emitting events.
    telemetry: Option<Telemetry>,
    /// Backend descriptor for binary identity verification (optional).
    backend: Option<BeadBackend>,
}

impl Resolver {
    /// Create a new resolver with default timeout.
    pub fn new(prompt_builder: PromptBuilder) -> Self {
        Self {
            config: crate::config::ResolveConfig::default(),
            prompt_builder,
            telemetry: None,
            backend: None,
        }
    }

    /// Create a new resolver with custom configuration.
    pub fn with_config(
        prompt_builder: PromptBuilder,
        config: crate::config::ResolveConfig,
    ) -> Self {
        Self {
            config,
            prompt_builder,
            telemetry: None,
            backend: None,
        }
    }

    /// Set the timeout for resolve agent calls.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.config.timeout_secs = timeout.as_secs();
        self
    }

    /// Set the telemetry client for event emission.
    pub fn with_telemetry(mut self, telemetry: Telemetry) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    /// Set the backend descriptor for binary identity verification.
    pub fn with_backend(mut self, backend: BeadBackend) -> Self {
        self.backend = Some(backend);
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

        // Verify binary identity BEFORE invoking the resolve agent (fail fast)
        if let Err(e) = self.verify_binary_identity_before_agent() {
            tracing::warn!(
                bead_id = %context.bead.id,
                error = %e,
                "resolver: binary identity verification failed before agent, using fallback"
            );
            return ResolveDecision::Retry {
                evidence: format!("Binary identity verification failed: {}", e),
                strategy: "different_approach".to_string(),
            };
        }

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
        let timeout = Duration::from_secs(self.config.timeout_secs);
        let output = tokio::time::timeout(timeout, async {
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
        // If a custom template path is configured, load and use it
        if let Some(ref template_path) = self.config.custom_template_path {
            return self.build_custom_prompt(context, template_path);
        }

        // Use default template unless explicitly disabled
        if !self.config.use_default_template {
            bail!(
                "Resolve is configured to not use default template, but no custom template path was provided"
            );
        }

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

    /// Build a prompt from a custom template file.
    fn build_custom_prompt(
        &self,
        context: &ResolveContext<'_>,
        template_path: &std::path::Path,
    ) -> Result<String> {
        let template = std::fs::read_to_string(template_path).with_context(|| {
            format!(
                "failed to read custom resolve template from {}",
                template_path.display()
            )
        })?;

        // Replace template variables with actual values
        let prompt = template
            .replace("{bead_title}", &context.bead.title)
            .replace("{bead_id}", context.bead.id.as_ref())
            .replace(
                "{bead_body}",
                context.bead.body.as_deref().unwrap_or("(no description)"),
            )
            .replace("{exit_code}", &context.exit_code.to_string())
            .replace("{duration}", &context.formatted_duration())
            .replace("{stdout}", &context.truncated_stdout())
            .replace("{stderr}", &context.truncated_stderr())
            .replace(
                "{exit_status}",
                if context.exit_code == 0 {
                    "success"
                } else {
                    "failure"
                },
            )
            .replace(
                "{was_interrupted}",
                if context.was_interrupted {
                    "true"
                } else {
                    "false"
                },
            );

        Ok(prompt)
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
            evidence: format!(
                "Resolver error: {}. Safe fallback: retry with same approach.",
                reason
            ),
            strategy: "same".to_string(),
        }
    }

    /// Verify binary identity before invoking the resolve agent.
    ///
    /// This method runs the resolved binary with --version and verifies that it
    /// claims to be the expected backend based on its version output.
    ///
    /// # Verification Process
    ///
    /// 1. Locates the binary using the backend's detect_paths
    /// 2. Runs the binary with the configured version_command (e.g., ["--version"])
    /// 3. Matches the full output against the backend's identity_pattern regex
    /// 4. Extracts the backend name from the pattern match
    /// 5. Verifies the name matches expected values for the backend
    ///
    /// # Returns
    ///
    /// - `Ok(())` - Verification passed or no backend configured
    /// - `Err(VerificationError::VerificationFailed)` - Binary identity doesn't match
    /// - `Err(VerificationError::NotSupported)` - Backend not configured or binary not found
    fn verify_binary_identity_before_agent(&self) -> Result<(), VerificationError> {
        let backend = match &self.backend {
            Some(backend) => backend,
            None => {
                // No backend configured - skip verification
                tracing::debug!("No backend configured for binary identity verification");
                return Ok(());
            }
        };

        // Try to find the binary using the backend's detect_paths
        let binary_path = self.find_binary_path(backend)?;

        tracing::debug!(
            backend_name = %backend.name,
            binary_path = %binary_path.display(),
            "Verifying binary identity"
        );

        // Run the binary with version command to get actual identity output
        let version_output = Self::run_binary_for_version(&binary_path, &backend.version_command)
            .map_err(|e| {
            VerificationError::VerificationFailed(format!(
                "Failed to run binary {:?} with command {:?}: {}",
                binary_path, backend.version_command, e
            ))
        })?;

        // Use the backend's identity_pattern regex to verify the output
        let identity_pattern = regex::Regex::new(&backend.identity_pattern).map_err(|e| {
            VerificationError::VerificationFailed(format!(
                "Invalid identity_pattern regex for backend '{}': {:?} - {}",
                backend.name, backend.identity_pattern, e
            ))
        })?;

        let trimmed_output = version_output.trim_start();
        if !identity_pattern.is_match(trimmed_output) {
            return Err(VerificationError::VerificationFailed(
                format!(
                    "Binary identity mismatch: binary at '{}' reported version {:?}, which does not match expected pattern {:?} for backend '{}'",
                    binary_path.display(),
                    trimmed_output,
                    backend.identity_pattern,
                    backend.name
                )
            ));
        }

        // Extract the backend name from the version output
        // The pattern captures the name at the start (e.g., "bf " or "bead ")
        let captured_name = trimmed_output
            .split_whitespace()
            .next()
            .ok_or_else(|| {
                VerificationError::VerificationFailed(
                    format!(
                        "Failed to extract backend name from version output: binary at '{}' reported empty version output",
                        binary_path.display()
                    )
                )
            })?;

        // Map the captured name to the expected backend name
        // Both "bf" and "bead-forge" should map to bead-forge, "bead" and "bead-rs" to bead-rs
        let expected_names = match backend.name.as_str() {
            "bead-forge" => vec!["bf", "bead-forge"],
            "bead-rs" => vec!["bead", "bead-rs"],
            _ => vec![backend.name.as_str()],
        };

        if !expected_names.contains(&captured_name) {
            return Err(VerificationError::VerificationFailed(
                format!(
                    "Binary identity mismatch: binary at '{}' reported name {:?}, but expected one of {:?} for backend '{}'",
                    binary_path.display(),
                    captured_name,
                    expected_names,
                    backend.name
                )
            ));
        }

        tracing::info!(
            backend_name = %backend.name,
            binary_path = %binary_path.display(),
            captured_name = %captured_name,
            "Binary identity verified successfully"
        );

        Ok(())
    }

    /// Run a binary with the given version command and return its stdout+stderr output.
    fn run_binary_for_version(binary_path: &PathBuf, version_command: &[String]) -> Result<String> {
        use std::process::Stdio;

        let output = std::process::Command::new(binary_path)
            .args(version_command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| {
                format!(
                    "failed to spawn binary '{}' for version check",
                    binary_path.display()
                )
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "binary '{}' exited with status {}: {}",
                binary_path.display(),
                output.status,
                stderr.trim()
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(format!("{}{}", stdout, stderr))
    }

    /// Find the binary path from the backend's detect_paths.
    fn find_binary_path(&self, backend: &BeadBackend) -> Result<PathBuf, VerificationError> {
        for detect_path in &backend.detect_paths {
            // Expand tilde if present
            let expanded_path = if detect_path.starts_with("~/") {
                if let Ok(home) = std::env::var("HOME") {
                    PathBuf::from(home).join(detect_path.strip_prefix("~/").unwrap_or(detect_path))
                } else {
                    detect_path.clone()
                }
            } else {
                detect_path.clone()
            };

            if expanded_path.exists() && expanded_path.is_file() {
                return Ok(expanded_path);
            }
        }

        // If no detect_paths found, try the binary name directly (assuming it's in PATH)
        if let Ok(which_output) = std::process::Command::new("which")
            .arg(&backend.binary)
            .output()
        {
            if which_output.status.success() {
                let path_str = String::from_utf8_lossy(&which_output.stdout).to_string();
                let path = path_str.trim();
                if !path.is_empty() {
                    return Ok(PathBuf::from(path));
                }
            }
        }

        Err(VerificationError::NotSupported(format!(
            "Binary '{}' not found in detect_paths or PATH",
            backend.binary
        )))
    }

    /// Normalize backend name for comparison.
    /// Maps short names to descriptor names: "bead" -> "bead-rs", "bf" -> "bead-forge"
    #[allow(dead_code)]
    fn normalize_backend_name(&self, name: &str) -> String {
        match name {
            "bead" => "bead-rs".to_string(),
            "bf" => "bead-forge".to_string(),
            other => other.to_string(),
        }
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
            commit_message: "Implemented feature".to_string(),
        };
        assert_eq!(format!("{}", complete), "Complete: Implemented feature");

        let retry = ResolveDecision::Retry {
            evidence: "Network timeout".to_string(),
            strategy: "increase_timeout".to_string(),
        };
        assert_eq!(format!("{}", retry), "Retry: Network timeout");

        let blocked = ResolveDecision::Blocked {
            evidence: "Log shows missing key".to_string(),
            blocker_type: "API key".to_string(),
            description: "Provide credentials".to_string(),
        };
        assert!(format!("{}", blocked).contains("Blocked on API key"));

        let split = ResolveDecision::Split {
            evidence: "Complex task analysis".to_string(),
            parent_bead_id: "needle-parent".to_string(),
            child_titles: vec!["Child 1".to_string()],
        };
        assert_eq!(format!("{}", split), "Split into 1 child tasks");
    }

    #[test]
    fn resolve_decision_as_str_returns_correct_strings() {
        assert_eq!(
            ResolveDecision::Complete {
                evidence: "x".to_string(),
                commit_message: "y".to_string(),
            }
            .as_str(),
            "complete"
        );

        assert_eq!(
            ResolveDecision::Retry {
                evidence: "x".to_string(),
                strategy: "same".to_string(),
            }
            .as_str(),
            "retry"
        );

        assert_eq!(
            ResolveDecision::Blocked {
                evidence: "x".to_string(),
                blocker_type: "y".to_string(),
                description: "z".to_string(),
            }
            .as_str(),
            "blocked"
        );

        assert_eq!(
            ResolveDecision::Split {
                evidence: "x".to_string(),
                parent_bead_id: "y".to_string(),
                child_titles: vec![],
            }
            .as_str(),
            "split"
        );
    }

    #[test]
    fn complete_decision_requires_all_fields() {
        let decision = ResolveDecision::Complete {
            evidence: "Commit abc123".to_string(),
            commit_message: "Implemented feature".to_string(),
        };
        assert!(decision.validate().is_ok());

        let invalid = ResolveDecision::Complete {
            evidence: "".to_string(),
            commit_message: "Implemented feature".to_string(),
        };
        assert!(invalid.validate().is_err());

        let invalid = ResolveDecision::Complete {
            evidence: "Commit abc123".to_string(),
            commit_message: "".to_string(),
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn retry_decision_requires_all_fields() {
        let decision = ResolveDecision::Retry {
            evidence: "Network timeout".to_string(),
            strategy: "increase_timeout".to_string(),
        };
        assert!(decision.validate().is_ok());

        let invalid = ResolveDecision::Retry {
            evidence: "".to_string(),
            strategy: "same".to_string(),
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn blocked_decision_requires_all_fields() {
        let decision = ResolveDecision::Blocked {
            evidence: "Log shows missing key".to_string(),
            blocker_type: "API key".to_string(),
            description: "Provide credentials".to_string(),
        };
        assert!(decision.validate().is_ok());

        let invalid = ResolveDecision::Blocked {
            evidence: "".to_string(),
            blocker_type: "API key".to_string(),
            description: "Provide credentials".to_string(),
        };
        assert!(invalid.validate().is_err());

        let invalid = ResolveDecision::Blocked {
            evidence: "Log shows missing key".to_string(),
            blocker_type: "".to_string(),
            description: "Provide credentials".to_string(),
        };
        assert!(invalid.validate().is_err());

        let invalid = ResolveDecision::Blocked {
            evidence: "Log shows missing key".to_string(),
            blocker_type: "API key".to_string(),
            description: "".to_string(),
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn split_decision_requires_children_and_rationale() {
        let decision = ResolveDecision::Split {
            evidence: "Complex task analysis".to_string(),
            parent_bead_id: "needle-parent".to_string(),
            child_titles: vec!["Child 1".to_string()],
        };
        assert!(decision.validate().is_ok());

        let invalid = ResolveDecision::Split {
            evidence: "Complex task analysis".to_string(),
            parent_bead_id: "needle-parent".to_string(),
            child_titles: vec![],
        };
        assert!(invalid.validate().is_err());

        let invalid = ResolveDecision::Split {
            evidence: "".to_string(),
            parent_bead_id: "needle-parent".to_string(),
            child_titles: vec!["Child 1".to_string()],
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn split_decision_limits_child_count() {
        let child_titles: Vec<_> = (0..11).map(|i| format!("Child {}", i)).collect();

        let invalid = ResolveDecision::Split {
            evidence: "Too many children".to_string(),
            parent_bead_id: "needle-parent".to_string(),
            child_titles,
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn resolve_response_parse_and_validate() {
        let json =
            r#"{"decision":{"complete":{"evidence":"Commit abc123","commit_message":"Done"}}}"#;
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
            r#"{"decision":{"complete":{"evidence":"x","commit_message":"y"}}}"#
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

        assert_eq!(resolver.config.timeout_secs, 300);
    }

    #[test]
    fn fallback_decision_has_safe_retry_strategy() {
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder);

        let fallback = resolver.fallback_decision("test_reason");

        assert!(matches!(
            fallback,
            ResolveDecision::Retry { strategy: _, .. }
        ));
        if let ResolveDecision::Retry { evidence, strategy } = fallback {
            assert!(evidence.contains("Resolver error: test_reason"));
            assert_eq!(strategy, "same");
        }
    }

    #[test]
    fn verify_binary_identity_before_agent_returns_ok_when_no_backend_configured() {
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder);

        let result = resolver.verify_binary_identity_before_agent();
        assert!(result.is_ok());
    }

    #[test]
    fn verify_binary_identity_before_agent_returns_error_when_binary_not_found() {
        use std::path::PathBuf;
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());

        // Create a backend with non-existent detect_paths
        let backend = BeadBackend {
            name: "test-backend".to_string(),
            binary: "nonexistent-binary".to_string(),
            detect_paths: vec![PathBuf::from("/nonexistent/path/binary")],
            identity_pattern: r"^test\s".to_string(),
            version_command: vec!["--version".to_string()],
            verified_against: "test 1.0.0".to_string(),
            verified_on: "2026-08-28".to_string(),
            operations: std::collections::HashMap::new(),
            capabilities: Default::default(),
            quirks: vec![],
            error_markers: Default::default(),
        };

        let resolver = Resolver::new(prompt_builder).with_backend(backend);

        let result = resolver.verify_binary_identity_before_agent();
        assert!(result.is_err());
        match result {
            Err(VerificationError::NotSupported(msg)) => {
                assert!(msg.contains("not found"));
            }
            _ => panic!("Expected NotSupported error"),
        }
    }

    #[test]
    fn normalize_backend_name_maps_short_to_descriptor_names() {
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder);

        assert_eq!(resolver.normalize_backend_name("bead"), "bead-rs");
        assert_eq!(resolver.normalize_backend_name("bf"), "bead-forge");
        assert_eq!(resolver.normalize_backend_name("bead-rs"), "bead-rs");
        assert_eq!(resolver.normalize_backend_name("bead-forge"), "bead-forge");
        assert_eq!(resolver.normalize_backend_name("other"), "other");
    }

    #[test]
    fn verification_error_can_be_created_and_displayed() {
        let error = VerificationError::VerificationFailed("checksum mismatch".to_string());
        assert!(error.to_string().contains("checksum mismatch"));

        let error = VerificationError::NotSupported("binary type".to_string());
        assert!(error.to_string().contains("not supported"));
    }

    #[tokio::test]
    async fn resolver_calls_verification_before_agent() {
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

        // The resolver should call verification before the agent (even though it's a stub that passes)
        let decision = resolver.resolve(&context).await;
        assert!(matches!(decision, ResolveDecision::Retry { .. }));
    }

    #[test]
    fn parse_and_validate_complete_decision() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json =
            r#"{"decision":{"complete":{"evidence":"Commit abc123","commit_message":"Done"}}}"#;
        let decision = resolver.parse_and_validate_response(json).unwrap();

        assert!(matches!(decision, ResolveDecision::Complete { .. }));
        if let ResolveDecision::Complete {
            evidence,
            commit_message,
        } = decision
        {
            assert_eq!(evidence, "Commit abc123");
            assert_eq!(commit_message, "Done");
        }
    }

    #[test]
    fn parse_and_validate_retry_decision() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json = r#"{"decision":{"retry":{"evidence":"Network timeout","strategy":"increase_timeout"}}}"#;
        let decision = resolver.parse_and_validate_response(json).unwrap();

        assert!(matches!(decision, ResolveDecision::Retry { .. }));
        if let ResolveDecision::Retry { evidence, strategy } = decision {
            assert_eq!(evidence, "Network timeout");
            assert_eq!(strategy, "increase_timeout");
        }
    }

    #[test]
    fn parse_and_validate_blocked_decision() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json = r#"{"decision":{"blocked":{"evidence":"Log shows missing key","blocker_type":"API key","description":"Provide credentials"}}}"#;
        let decision = resolver.parse_and_validate_response(json).unwrap();

        assert!(matches!(decision, ResolveDecision::Blocked { .. }));
        if let ResolveDecision::Blocked {
            evidence,
            blocker_type,
            description,
        } = decision
        {
            assert_eq!(blocker_type, "API key");
            assert_eq!(description, "Provide credentials");
            assert!(evidence.contains("missing key"));
        }
    }

    #[test]
    fn parse_and_validate_split_decision() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json = r#"{"decision":{"split":{"evidence":"Complex task analysis","parent_bead_id":"needle-parent","child_titles":["Child 1"]}}}"#;
        let decision = resolver.parse_and_validate_response(json).unwrap();

        assert!(matches!(decision, ResolveDecision::Split { .. }));
        if let ResolveDecision::Split {
            evidence,
            parent_bead_id,
            child_titles,
        } = decision
        {
            assert_eq!(parent_bead_id, "needle-parent");
            assert_eq!(child_titles.len(), 1);
            assert_eq!(child_titles[0], "Child 1");
            assert!(evidence.contains("analysis"));
        }
    }

    #[test]
    fn parse_and_validate_strips_markdown_fencing() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let json = r#"```json
{"decision":{"complete":{"evidence":"Commit abc123","commit_message":"Done"}}}
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
        let json = r#"{"decision":{"complete":{"commit_message":"Done"}}}"#;
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
        let json = r#"{"decision":{"complete":{"evidence":"","commit_message":"Done"}}}"#;
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

    #[test]
    fn resolver_with_config_uses_custom_timeout() {
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());

        let config = crate::config::ResolveConfig {
            enabled: true,
            timeout_secs: 300,
            custom_template_path: None,
            use_default_template: true,
        };

        let resolver = Resolver::with_config(prompt_builder, config);
        assert_eq!(resolver.config.timeout_secs, 300);
    }

    #[test]
    fn resolver_with_custom_template_path() {
        use std::path::PathBuf;
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());

        let config = crate::config::ResolveConfig {
            enabled: true,
            timeout_secs: 60,
            custom_template_path: Some(PathBuf::from("/tmp/resolve-template.txt")),
            use_default_template: false,
        };

        let resolver = Resolver::with_config(prompt_builder, config);
        assert_eq!(
            resolver.config.custom_template_path,
            Some(PathBuf::from("/tmp/resolve-template.txt"))
        );
        assert!(!resolver.config.use_default_template);
    }

    #[test]
    fn resolve_config_validation_rejects_zero_timeout() {
        let config = crate::config::ResolveConfig {
            enabled: true,
            timeout_secs: 0,
            custom_template_path: None,
            use_default_template: true,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be greater than 0"));
    }

    #[test]
    fn resolve_config_validation_requires_custom_template_when_disabled_default() {
        let config = crate::config::ResolveConfig {
            enabled: true,
            timeout_secs: 60,
            custom_template_path: None,
            use_default_template: false,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("no custom_template_path provided"));
    }

    #[test]
    fn resolve_config_validation_fails_on_missing_template_file() {
        let config = crate::config::ResolveConfig {
            enabled: true,
            timeout_secs: 60,
            custom_template_path: Some(PathBuf::from("/nonexistent/template.txt")),
            use_default_template: false,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn resolve_config_validation_fails_on_directory_path() {
        let config = crate::config::ResolveConfig {
            enabled: true,
            timeout_secs: 60,
            custom_template_path: Some(PathBuf::from("/tmp")), // /tmp is a directory
            use_default_template: false,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a file"));
    }

    #[test]
    fn resolve_config_validation_passes_with_valid_config() {
        let config = crate::config::ResolveConfig {
            enabled: true,
            timeout_secs: 120,
            custom_template_path: None,
            use_default_template: true,
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn resolve_config_validation_passes_with_custom_template() {
        use std::fs::File;
        use std::io::Write;

        // Create a temporary template file
        let temp_dir = std::env::temp_dir();
        let template_path = temp_dir.join("test-resolve-template.txt");
        let mut file = File::create(&template_path).unwrap();
        file.write_all(b"Test template with {bead_title}").unwrap();

        let config = crate::config::ResolveConfig {
            enabled: true,
            timeout_secs: 120,
            custom_template_path: Some(template_path.clone()),
            use_default_template: false,
        };

        let result = config.validate();
        assert!(result.is_ok());

        // Clean up
        std::fs::remove_file(template_path).unwrap();
    }

    #[tokio::test]
    async fn resolver_build_custom_prompt_replaces_variables() {
        use std::fs::File;
        use std::io::Write;

        let bead = test_bead();
        let bead_ref = Box::leak(Box::new(bead));

        let context = ResolveContext::new(
            bead_ref,
            0,
            "stdout content".to_string(),
            "stderr content".to_string(),
            Duration::from_secs(60),
            Utc::now(),
            false,
        );

        // Create a temporary template file
        let temp_dir = std::env::temp_dir();
        let template_path = temp_dir.join("test-resolve-template.txt");
        let mut file = File::create(&template_path).unwrap();
        file.write_all(
            b"Bead: {bead_title}\nExit: {exit_code}\nStdout: {stdout}\nStderr: {stderr}",
        )
        .unwrap();

        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());

        let config = crate::config::ResolveConfig {
            enabled: true,
            timeout_secs: 60,
            custom_template_path: Some(template_path.clone()),
            use_default_template: false,
        };

        let resolver = Resolver::with_config(prompt_builder, config);
        let prompt = resolver
            .build_custom_prompt(&context, &template_path)
            .unwrap();

        assert!(prompt.contains("Bead: Test bead"));
        assert!(prompt.contains("Exit: 0"));
        assert!(prompt.contains("Stdout: stdout content"));
        assert!(prompt.contains("Stderr: stderr content"));

        // Clean up
        std::fs::remove_file(template_path).unwrap();
    }

    #[tokio::test]
    async fn resolver_build_custom_prompt_fails_on_missing_file() {
        use std::path::PathBuf;

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

        let nonexistent_path = PathBuf::from("/nonexistent/template.txt");

        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());

        let resolver = Resolver::new(prompt_builder);
        let result = resolver.build_custom_prompt(&context, &nonexistent_path);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed to read"));
    }

    #[test]
    fn resolver_with_config_preserves_all_fields() {
        use std::path::PathBuf;
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());

        let config = crate::config::ResolveConfig {
            enabled: true,
            timeout_secs: 180,
            custom_template_path: Some(PathBuf::from("/custom/template.txt")),
            use_default_template: false,
        };

        let resolver = Resolver::with_config(prompt_builder, config);

        assert!(resolver.config.enabled);
        assert_eq!(resolver.config.timeout_secs, 180);
        assert_eq!(
            resolver.config.custom_template_path,
            Some(PathBuf::from("/custom/template.txt"))
        );
        assert!(!resolver.config.use_default_template);
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Evidence and Field Preservation Tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn complete_decision_preserves_evidence_field_through_roundtrip() {
        let original = ResolveDecision::Complete {
            evidence: "Commit abc123: Added feature X\nTests passed: 42/42".to_string(),
            commit_message: "Successfully implemented feature X with full test coverage"
                .to_string(),
        };

        // Serialize to JSON through ResolveResponse
        let response_original = ResolveResponse {
            decision: original.clone(),
        };
        let json = serde_json::to_string(&response_original).unwrap();

        // Parse back through ResolveResponse
        let response: ResolveResponse = serde_json::from_str(&json).unwrap();

        // Verify evidence is preserved exactly
        match response.decision {
            ResolveDecision::Complete {
                evidence,
                commit_message,
            } => {
                assert_eq!(
                    evidence,
                    "Commit abc123: Added feature X\nTests passed: 42/42"
                );
                assert_eq!(
                    commit_message,
                    "Successfully implemented feature X with full test coverage"
                );
            }
            _ => panic!("Expected Complete decision"),
        }
    }

    #[test]
    fn retry_decision_preserves_all_fields_through_roundtrip() {
        let original = ResolveDecision::Retry {
            evidence: "HTTP 503 Service Unavailable".to_string(),
            strategy: "backoff".to_string(),
        };

        let response_original = ResolveResponse {
            decision: original.clone(),
        };
        let json = serde_json::to_string(&response_original).unwrap();
        let response: ResolveResponse = serde_json::from_str(&json).unwrap();

        match response.decision {
            ResolveDecision::Retry { evidence, strategy } => {
                assert_eq!(evidence, "HTTP 503 Service Unavailable");
                assert_eq!(strategy, "backoff");
            }
            _ => panic!("Expected Retry decision"),
        }
    }

    #[test]
    fn blocked_decision_preserves_all_fields_through_roundtrip() {
        let original = ResolveDecision::Blocked {
            evidence: "Log shows missing OpenBao credentials".to_string(),
            blocker_type: "OpenBao credentials".to_string(),
            description: "Run bao write secret/app credentials".to_string(),
        };

        let response_original = ResolveResponse {
            decision: original.clone(),
        };
        let json = serde_json::to_string(&response_original).unwrap();
        let response: ResolveResponse = serde_json::from_str(&json).unwrap();

        match response.decision {
            ResolveDecision::Blocked {
                evidence,
                blocker_type,
                description,
            } => {
                assert_eq!(blocker_type, "OpenBao credentials");
                assert_eq!(description, "Run bao write secret/app credentials");
                assert!(evidence.contains("missing"));
            }
            _ => panic!("Expected Blocked decision"),
        }
    }

    #[test]
    fn split_decision_preserves_all_fields_through_roundtrip() {
        let original = ResolveDecision::Split {
            evidence: "Task can be parallelized across two developers".to_string(),
            parent_bead_id: "needle-parent".to_string(),
            child_titles: vec![
                "Implement authentication".to_string(),
                "Implement data storage".to_string(),
            ],
        };

        let response_original = ResolveResponse {
            decision: original.clone(),
        };
        let json = serde_json::to_string(&response_original).unwrap();
        let response: ResolveResponse = serde_json::from_str(&json).unwrap();

        match response.decision {
            ResolveDecision::Split {
                evidence,
                parent_bead_id,
                child_titles,
            } => {
                assert_eq!(parent_bead_id, "needle-parent");
                assert_eq!(child_titles.len(), 2);
                assert_eq!(child_titles[0], "Implement authentication");
                assert_eq!(child_titles[1], "Implement data storage");
                assert!(evidence.contains("parallelized"));
            }
            _ => panic!("Expected Split decision"),
        }
    }

    #[test]
    fn all_retry_strategies_preserve_through_roundtrip() {
        let strategies = vec![
            "same".to_string(),
            "increase_timeout".to_string(),
            "different_approach".to_string(),
            "backoff".to_string(),
        ];

        for strategy in strategies {
            let original = ResolveDecision::Retry {
                evidence: "Test".to_string(),
                strategy: strategy.clone(),
            };

            let response_original = ResolveResponse {
                decision: original.clone(),
            };
            let json = serde_json::to_string(&response_original).unwrap();
            let response: ResolveResponse = serde_json::from_str(&json).unwrap();

            match response.decision {
                ResolveDecision::Retry { strategy: s, .. } => {
                    assert_eq!(s, strategy);
                }
                _ => panic!("Expected Retry decision with strategy {:?}", strategy),
            }
        }
    }

    #[test]
    fn all_block_types_preserve_through_roundtrip() {
        let blocker_types = vec![
            "API key".to_string(),
            "Human decision".to_string(),
            "Resource".to_string(),
        ];

        for blocker_type in blocker_types {
            let original = ResolveDecision::Blocked {
                evidence: "Test".to_string(),
                blocker_type: blocker_type.clone(),
                description: "Test".to_string(),
            };

            let response_original = ResolveResponse {
                decision: original.clone(),
            };
            let json = serde_json::to_string(&response_original).unwrap();
            let response: ResolveResponse = serde_json::from_str(&json).unwrap();

            match response.decision {
                ResolveDecision::Blocked {
                    blocker_type: bt, ..
                } => {
                    assert_eq!(bt, blocker_type);
                }
                _ => panic!(
                    "Expected Blocked decision with blocker_type {:?}",
                    blocker_type
                ),
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Malformed JSON Failure Mode Tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_and_validate_fails_on_empty_string() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response("");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("parse"));
    }

    #[test]
    fn parse_and_validate_fails_on_garbage_string() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response("!!not-json!!");
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_partial_json() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver
            .parse_and_validate_response("{\"decision\":{\"complete\":{\"evidence\":\"test\"}}");
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_json_array_instead_of_object() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response("[]");
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_string_instead_of_object() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response("\"decision\"");
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_number_instead_of_object() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response("42");
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_bool_instead_of_object() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response("true");
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_null_instead_of_object() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response("null");
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_whitespace_only() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response("   \n\t  ");
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_json_with_bom() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // JSON with UTF-8 BOM
        let json_with_bom =
            "\u{FEFF}{\"decision\":{\"complete\":{\"evidence\":\"test\",\"summary\":\"test\"}}}";
        let result = resolver.parse_and_validate_response(json_with_bom);
        assert!(result.is_err());
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Invalid Decision Type Tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_and_validate_fails_on_unknown_decision_type() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Completely unknown decision type
        let result = resolver
            .parse_and_validate_response(r#"{"decision":{"unknown_type":{"field":"value"}}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_malformed_decision_type_name() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Typo in decision type
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"compleet":{"evidence":"test","commit_message":"test"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_wrong_case_decision_type() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Wrong case (should be snake_case)
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"Complete":{"evidence":"test","commit_message":"test"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_decision_as_string() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Decision field is a string, not an object
        let result = resolver.parse_and_validate_response(r#"{"decision":"complete"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_decision_as_array() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Decision field is an array, not an object
        let result = resolver.parse_and_validate_response(
            r#"{"decision":[{"complete":{"evidence":"test","commit_message":"test"}}]}"#,
        );
        assert!(result.is_err());
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Missing Required Field Tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_and_validate_fails_on_complete_without_evidence() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Missing evidence field in Complete
        let result = resolver
            .parse_and_validate_response(r#"{"decision":{"complete":{"commit_message":"Done"}}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_complete_without_summary() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Missing summary field in Complete
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"complete":{"evidence":"Commit abc123"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_retry_without_reason() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Missing reason field in Retry
        let result =
            resolver.parse_and_validate_response(r#"{"decision":{"retry":{"strategy":"same"}}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_retry_without_strategy() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Missing strategy field in Retry
        let result =
            resolver.parse_and_validate_response(r#"{"decision":{"retry":{"reason":"Timeout"}}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_blocked_without_blocker() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Missing blocker field in Blocked
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"blocked":{"required_action":"Get approval","severity":"high"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_blocked_without_required_action() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Missing required_action field in Blocked
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"blocked":{"blocker":"Management approval","severity":"high"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_blocked_without_severity() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Missing severity field in Blocked
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"blocked":{"blocker":"Management approval","required_action":"Get approval"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_split_without_children() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Missing children field in Split
        let result = resolver
            .parse_and_validate_response(r#"{"decision":{"split":{"rationale":"Complex task"}}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_split_without_rationale() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Missing rationale field in Split
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"split":{"children":[{"title":"Task 1","body":"Description","priority":1}]}}}"#,
        );
        assert!(result.is_err());
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Invalid Field Value Tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_and_validate_fails_on_invalid_retry_strategy() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Invalid retry strategy
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"retry":{"reason":"Timeout","strategy":"invalid_strategy"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_invalid_block_severity() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Invalid block severity
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"blocked":{"blocker":"API key","required_action":"Get key","severity":"critical"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_invalid_priority_value() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Invalid priority value (too high)
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"split":{"children":[{"title":"Task 1","body":"Description","priority":10}],"rationale":"Test"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_zero_priority_value() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Invalid priority value (zero)
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"split":{"children":[{"title":"Task 1","body":"Description","priority":0}],"rationale":"Test"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_negative_priority_value() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        // Invalid priority value (negative)
        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"split":{"children":[{"title":"Task 1","body":"Description","priority":-1}],"rationale":"Test"}}}"#,
        );
        assert!(result.is_err());
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Empty String Validation Tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_and_validate_fails_on_empty_complete_evidence() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"complete":{"evidence":"  ","commit_message":"Done"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_empty_complete_commit_message() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"complete":{"evidence":"Commit abc123","commit_message":"\t\n"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_empty_retry_reason() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"retry":{"reason":"","strategy":"same"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_empty_blocked_blocker() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"blocked":{"blocker":"  ","required_action":"Approve","severity":"high"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_empty_blocked_required_action() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"blocked":{"blocker":"Management","required_action":"","severity":"high"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_empty_split_rationale() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"split":{"children":[{"title":"Task 1","body":"Description","priority":1}],"rationale":"\n\t"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_empty_child_title() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"split":{"children":[{"title":"","body":"Description","priority":1}],"rationale":"Test"}}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_and_validate_fails_on_empty_child_body() {
        let resolver = Resolver::new(crate::prompt::PromptBuilder::new(
            &crate::config::PromptConfig::default(),
        ));

        let result = resolver.parse_and_validate_response(
            r#"{"decision":{"split":{"children":[{"title":"Task 1","body":"   ","priority":1}],"rationale":"Test"}}}"#,
        );
        assert!(result.is_err());
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Timeout Behavior Tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_fallback_returns_safe_retry_decision() {
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder);

        let fallback = resolver.fallback_decision("test_failure_reason");

        match fallback {
            ResolveDecision::Retry { evidence, strategy } => {
                assert!(evidence.contains("Resolver error: test_failure_reason"));
                assert!(evidence.contains("Safe fallback: retry with same approach"));
                assert_eq!(strategy, "same");
            }
            _ => panic!("Fallback must return Retry decision"),
        }
    }

    #[tokio::test]
    async fn resolve_timeout_returns_fallback_decision() {
        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder);

        let bead = test_bead();
        let bead_ref = Box::leak(Box::new(bead));

        let context = ResolveContext::new(
            bead_ref,
            1,
            "stdout".to_string(),
            "stderr".to_string(),
            Duration::from_secs(60),
            Utc::now(),
            false,
        );

        // Mock a prompt build failure to trigger fallback
        // This tests the timeout fallback path without actually waiting for a timeout
        let decision = resolver.resolve(&context).await;

        // Should return a safe fallback (Retry)
        assert!(matches!(decision, ResolveDecision::Retry { .. }));
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Fixture Shim Tests - Binary Identity Mismatch Detection
    // ──────────────────────────────────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn verify_binary_identity_before_agent_detects_shim_mismatch() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();

        // Create a "bead" shim script that execs a "bf" binary (different identity)
        // This simulates the br->bf case where a binary claims to be one thing
        // but actually execs a different binary
        let shim_path = temp_dir.path().join("fake-bead-shim");
        let shim_content = r#"#!/bin/sh
# This shim claims to be "bead" but execs "bf" (different identity)
# This simulates a misconfigured or malicious binary
exec echo "bf 0.4.1"
"#;
        std::fs::write(&shim_path, shim_content).unwrap();
        let mut permissions = std::fs::metadata(&shim_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim_path, permissions).unwrap();

        // Create a backend descriptor expecting "bead-rs" identity
        let backend = BeadBackend {
            name: "bead-rs".to_string(),
            binary: "fake-bead-shim".to_string(),
            detect_paths: vec![shim_path.clone()],
            identity_pattern: r"^bead\s".to_string(),
            version_command: vec!["--version".to_string()],
            verified_against: "bead 0.1.0".to_string(),
            verified_on: "2026-08-28".to_string(),
            operations: std::collections::HashMap::new(),
            capabilities: Default::default(),
            quirks: vec![],
            error_markers: Default::default(),
        };

        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder).with_backend(backend);

        // The verification should detect the mismatch
        let result = resolver.verify_binary_identity_before_agent();

        // Assert that the mismatch was detected (not silently accepted)
        assert!(
            result.is_err(),
            "verify_binary_identity_before_agent should return error for mismatch"
        );

        // Assert the error is actionable and names both path and identity
        match result {
            Err(VerificationError::VerificationFailed(msg)) => {
                // Error message should mention the identity mismatch - specifically:
                // - The actual detected identity ("bf" in this case)
                // - The expected backend ("bead-rs" in this case)
                // - The word "mismatch" or "normalized" to explain what's wrong
                assert!(
                    msg.contains("bf") && msg.contains("bead-rs") &&
                    (msg.contains("mismatch") || msg.contains("normalized") || msg.contains("claims to be")),
                    "Error message should detail the identity mismatch (got: {}), but it didn't contain expected identifiers", msg
                );

                // Error message should be descriptive and actionable
                assert!(
                    msg.len() > 30,
                    "Error message should be descriptive and actionable: {}",
                    msg
                );

                // Verify the specific error format includes both identities
                assert!(
                    (msg.contains("claims to be") || msg.contains("reported"))
                        && (msg.contains("expected backend") || msg.contains("expected pattern")),
                    "Error message should explain the mismatch clearly: {}",
                    msg
                );
            }
            Err(VerificationError::NotSupported(msg)) => {
                panic!("Expected VerificationFailed, got NotSupported: {}", msg);
            }
            Ok(()) => {
                panic!("Expected verification to fail for mismatched identity");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn verify_binary_identity_before_agent_detects_reverse_shim_mismatch() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();

        // Create a "bf" shim script that execs a "bead" binary (reverse case)
        let shim_path = temp_dir.path().join("fake-bf-shim");
        let shim_content = r#"#!/bin/sh
# This shim claims to be "bf" but execs "bead" (different identity)
exec echo "bead 0.1.3"
"#;
        std::fs::write(&shim_path, shim_content).unwrap();
        let mut permissions = std::fs::metadata(&shim_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim_path, permissions).unwrap();

        // Create a backend descriptor expecting "bead-forge" identity
        let backend = BeadBackend {
            name: "bead-forge".to_string(),
            binary: "fake-bf-shim".to_string(),
            detect_paths: vec![shim_path.clone()],
            identity_pattern: r"^bf\s".to_string(),
            version_command: vec!["--version".to_string()],
            verified_against: "bf 0.4.0".to_string(),
            verified_on: "2026-08-28".to_string(),
            operations: std::collections::HashMap::new(),
            capabilities: Default::default(),
            quirks: vec![],
            error_markers: Default::default(),
        };

        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder).with_backend(backend);

        // The verification should detect the reverse mismatch
        let result = resolver.verify_binary_identity_before_agent();

        // Assert that the mismatch was detected
        assert!(
            result.is_err(),
            "verify_binary_identity_before_agent should return error for reverse mismatch"
        );

        match result {
            Err(VerificationError::VerificationFailed(msg)) => {
                // Error message should mention both identities involved in the mismatch
                assert!(
                    msg.contains("bead")
                        && msg.contains("bf")
                        && (msg.contains("mismatch")
                            || msg.contains("normalized")
                            || msg.contains("claims to be")),
                    "Error message should detail the identity mismatch: {}",
                    msg
                );

                // Error message should be descriptive
                assert!(
                    msg.len() > 30,
                    "Error message should be descriptive and actionable: {}",
                    msg
                );
            }
            Err(VerificationError::NotSupported(msg)) => {
                panic!("Expected VerificationFailed, got NotSupported: {}", msg);
            }
            Ok(()) => {
                panic!("Expected verification to fail for reverse mismatched identity");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn verify_binary_identity_before_agent_passes_for_matching_identity() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();

        // Create a "bead" binary that reports correct identity (no mismatch)
        let binary_path = temp_dir.path().join("correct-bead");
        let binary_content = r#"#!/bin/sh
# This binary correctly reports its identity as "bead"
echo "bead 0.1.3"
"#;
        std::fs::write(&binary_path, binary_content).unwrap();
        let mut permissions = std::fs::metadata(&binary_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary_path, permissions).unwrap();

        // Create a backend descriptor expecting "bead-rs" identity
        let backend = BeadBackend {
            name: "bead-rs".to_string(),
            binary: "correct-bead".to_string(),
            detect_paths: vec![binary_path.clone()],
            identity_pattern: r"^bead\s".to_string(),
            version_command: vec!["--version".to_string()],
            verified_against: "bead 0.1.0".to_string(),
            verified_on: "2026-08-28".to_string(),
            operations: std::collections::HashMap::new(),
            capabilities: Default::default(),
            quirks: vec![],
            error_markers: Default::default(),
        };

        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder).with_backend(backend);

        // The verification should pass for matching identity
        let result = resolver.verify_binary_identity_before_agent();

        // Assert that verification passed
        assert!(
            result.is_ok(),
            "verify_binary_identity_before_agent should pass for matching identity, got: {:?}",
            result
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_binary_identity_before_agent_error_message_is_actionable() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();

        // Create a shim with a mismatch
        let shim_path = temp_dir.path().join("actionable-test-shim");
        std::fs::write(
            &shim_path,
            r#"#!/bin/sh
echo "wrong-identity 1.0.0"
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&shim_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim_path, permissions).unwrap();

        let backend = BeadBackend {
            name: "bead-rs".to_string(),
            binary: "actionable-test-shim".to_string(),
            detect_paths: vec![shim_path.clone()],
            identity_pattern: r"^bead\s".to_string(),
            version_command: vec!["--version".to_string()],
            verified_against: "bead 0.1.0".to_string(),
            verified_on: "2026-08-28".to_string(),
            operations: std::collections::HashMap::new(),
            capabilities: Default::default(),
            quirks: vec![],
            error_markers: Default::default(),
        };

        let prompt_builder =
            crate::prompt::PromptBuilder::new(&crate::config::PromptConfig::default());
        let resolver = Resolver::new(prompt_builder).with_backend(backend);

        let result = resolver.verify_binary_identity_before_agent();

        match result {
            Err(VerificationError::VerificationFailed(msg)) => {
                // Verify error message is actionable by checking it contains:
                // 1. The actual identity found ("wrong-identity" in this case)
                // 2. The expected identity ("bead" in this case)
                // 3. Clear explanation of the mismatch
                assert!(
                    msg.contains("wrong-identity")
                        && msg.contains("bead-rs")
                        && (msg.contains("mismatch")
                            || msg.contains("claims to be")
                            || msg.contains("expected backend")),
                    "Error should detail the identity mismatch clearly: {}",
                    msg
                );

                // Error message should be descriptive (not a generic "failed" message)
                assert!(
                    msg.len() > 40,
                    "Error message should be descriptive and actionable: {}",
                    msg
                );

                // Error message should not be empty or whitespace-only
                assert!(!msg.trim().is_empty(), "Error message should not be empty");

                // Error message should use actionable language ("claims", "expected", "normalized")
                assert!(
                    msg.contains("claims")
                        || msg.contains("expected")
                        || msg.contains("normalized"),
                    "Error message should use actionable, explanatory language: {}",
                    msg
                );
            }
            Err(VerificationError::NotSupported(msg)) => {
                panic!("Expected VerificationFailed, got NotSupported: {}", msg);
            }
            Ok(()) => {
                panic!("Expected verification to fail for mismatched identity");
            }
        }
    }
}
