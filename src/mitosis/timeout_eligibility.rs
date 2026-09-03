//! Timeout-triggered mitosis eligibility decision.
//!
//! Provides pure eligibility classification for timeouts that may qualify for
//! immediate mitosis (bypassing normal failure-count thresholds).
//!
//! ## Design
//!
//! The eligibility decision is deterministic and exhaustive:
//!
//! **Accepts (eligible for mitosis):**
//! - Agent wall-clock timeouts with evidence of sustained activity
//! - Handler timeouts on legitimate validation work
//!
//! **Rejects (not eligible):**
//! - Outcome::Interrupted (graceful shutdown, not a timeout)
//! - Outcome::Crash (signal kills, not productive work)
//! - Bead-store timeouts (no agent activity evidence)
//! - Outcome-handler timeouts on idle/bead-store operations
//! - Timeouts with insufficient elapsed fraction (too early to diagnose)
//!
//! No human log text parsing — all decisions are based on exit codes,
//! durations, and configuration flags.

use std::time::Duration;

use crate::config::TimeoutTriggeredPolicy;
use crate::types::{AgentOutcome, Outcome};

/// Eligibility decision for timeout-triggered mitosis.
///
/// Carries the verdict and a human-readable reason that explains why
/// the timeout does or does not qualify for immediate mitosis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeoutEligibility {
    /// Timeout qualifies for mitosis — the task was genuinely productive
    /// but exceeded its time budget.
    Eligible { reason: String },

    /// Timeout does not qualify — either not a timeout, or the timeout
    /// does not represent productive long-running work.
    NotEligible { reason: String },
}

impl TimeoutEligibility {
    /// Returns true if the timeout qualifies for mitosis.
    pub fn is_eligible(&self) -> bool {
        matches!(self, TimeoutEligibility::Eligible { .. })
    }

    /// Returns the human-readable reason for the decision.
    pub fn reason(&self) -> &str {
        match self {
            TimeoutEligibility::Eligible { reason } => reason,
            TimeoutEligibility::NotEligible { reason } => reason,
        }
    }
}

/// Classification of timeout origin.
///
/// Distinguishes between different timeout sources so eligibility can
/// reject infrastructure failures while accepting productive work timeouts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeoutOrigin {
    /// Agent process wall-clock timeout (GNU `timeout` wrapper exit code 124).
    ///
    /// This represents legitimate task duration limits — the agent was
    /// actively working but exceeded its configured timeout.
    AgentWallclock { timeout_duration: Duration },

    /// Outcome handler timeout (validation gate exceeded its budget).
    ///
    /// May qualify if the validation itself is substantial work
    /// (e.g., running a real test suite, linting, static analysis).
    HandlerTimeout { gate_name: Option<String> },

    /// Bead-store timeout (bead CLI operation exceeded timeout).
    ///
    /// Never qualifies — no agent activity evidence, represents
    /// infrastructure slowness, not productive work.
    BeadStoreTimeout,

    /// Outcome processing timeout (non-handler phase of outcome handling).
    ///
    /// Never qualifies — represents infrastructure slowness.
    OutcomeProcessingTimeout,
}

/// Evidence that the agent was actively working (not idle/crashed).
///
/// Eligibility requires affirmative evidence that the timeout represents
/// productive computation, not a stuck process or infrastructure failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityEvidence {
    /// Agent emitted tool-use calls (LSP, Bash, Read, etc.) before timeout.
    ///
    /// Inferred from non-empty stderr (agent tools log to stderr).
    HasToolUseCalls,

    /// Agent produced structured output before timeout.
    ///
    /// Inferred from non-empty stdout.
    HasStructuredOutput,

    /// Timeout occurred after substantial elapsed time (not a flaky early timeout).
    ///
    /// Carries the actual elapsed duration and the configured timeout.
    SubstantialElapsedTime {
        elapsed: Duration,
        timeout: Duration,
    },

    /// No evidence of activity — timeout may have occurred on an idle agent.
    NoEvidence,
}

/// Classify a timeout for mitosis eligibility.
///
/// This is a pure decision function — it reads the agent execution result
/// and resolved configuration, then returns a deterministic eligible/ineligible
/// verdict. No bead state mutation, no Mitosis invocation.
///
/// # Arguments
///
/// * `outcome` - Raw agent process result (exit code, stdout, stderr)
/// * `duration` - Wall-clock duration of the agent execution
/// * `policy` - Resolved timeout-triggered mitosis configuration
///
/// # Returns
///
/// A `TimeoutEligibility` carrying the verdict and explanation.
///
/// # Examples
///
/// ```no_run
/// use needle::mitosis::timeout_eligibility::{classify_timeout_eligibility, TimeoutEligibility};
/// use needle::types::AgentOutcome;
/// use std::time::Duration;
///
/// let outcome = AgentOutcome {
///     exit_code: 124, // GNU timeout exit code
///     stdout: "analysis complete".to_string(),
///     stderr: "".to_string(),
/// };
///
/// let eligibility = classify_timeout_eligibility(
///     &outcome,
///     Duration::from_secs(3540), // 59 minutes elapsed
///     &timeout_policy,
/// );
///
/// assert!(eligibility.is_eligible());
/// assert!(eligibility.reason().contains("agent wall-clock timeout"));
/// ```
pub fn classify_timeout_eligibility(
    outcome: &AgentOutcome,
    duration: Duration,
    policy: &TimeoutTriggeredPolicy,
) -> TimeoutEligibility {
    if matches!(outcome.exit_code, 130 | 143) {
        return TimeoutEligibility::NotEligible {
            reason: "interrupted by signal (SIGINT/SIGTERM), not a timeout".to_string(),
        };
    }

    // Step 1: Classify the outcome to determine if this is a timeout at all
    let outcome_classification = Outcome::classify(outcome.exit_code, false);

    // Step 2: Reject non-timeout outcomes immediately
    match outcome_classification {
        Outcome::Timeout => {
            // Continue to timeout-specific analysis below
        }
        Outcome::Interrupted => {
            return TimeoutEligibility::NotEligible {
                reason: "interrupted by signal (SIGINT/SIGTERM), not a timeout".to_string(),
            };
        }
        Outcome::Crash(code) => {
            return TimeoutEligibility::NotEligible {
                reason: format!(
                    "process killed by signal (exit code {}), not a timeout",
                    code
                ),
            };
        }
        // GateUnsatisfiable is a gate-side attribution, never a timeout, so
        // timeout-triggered decomposition can never apply to it.
        Outcome::Success
        | Outcome::Failure
        | Outcome::AgentNotFound
        | Outcome::GateError
        | Outcome::GateUnsatisfiable => {
            return TimeoutEligibility::NotEligible {
                reason: format!(
                    "exit code {} is not a timeout (expected 124)",
                    outcome.exit_code
                ),
            };
        }
    }

    // Step 3: Determine timeout origin and check policy enablement
    let timeout_origin = classify_timeout_origin(outcome, duration);

    // Check if timeout-triggered mitosis is enabled globally
    if !policy.enabled {
        return TimeoutEligibility::NotEligible {
            reason: "timeout-triggered mitosis is disabled in configuration".to_string(),
        };
    }

    // Step 4: Check elapsed fraction threshold (rejects flaky early timeouts)
    let elapsed_fraction = if duration.is_zero() {
        0.0
    } else {
        // Infer timeout duration from origin
        let timeout_duration = match &timeout_origin {
            TimeoutOrigin::AgentWallclock { timeout_duration } => *timeout_duration,
            TimeoutOrigin::HandlerTimeout { .. } => Duration::from_secs(3600), // default
            TimeoutOrigin::BeadStoreTimeout | TimeoutOrigin::OutcomeProcessingTimeout => {
                Duration::from_secs(30)
            }
        };

        if timeout_duration.is_zero() {
            0.0
        } else {
            duration.as_secs_f64() / timeout_duration.as_secs_f64()
        }
    };

    if elapsed_fraction < policy.min_elapsed_fraction {
        return TimeoutEligibility::NotEligible {
            reason: format!(
                "insufficient elapsed fraction ({:.2} < {:.2}) — likely a flaky early timeout",
                elapsed_fraction, policy.min_elapsed_fraction
            ),
        };
    }

    // Step 5: Gather activity evidence
    let activity = detect_activity_evidence(outcome);

    // Step 6: Apply policy rules per timeout origin
    match timeout_origin {
        TimeoutOrigin::AgentWallclock {
            timeout_duration: _,
        } => {
            if !policy.agent_wallclock_timeout {
                return TimeoutEligibility::NotEligible {
                    reason: "agent wall-clock timeouts are not enabled in policy".to_string(),
                };
            }

            // Require affirmative evidence of agent activity
            match activity {
                ActivityEvidence::NoEvidence => {
                    TimeoutEligibility::NotEligible {
                        reason: "no evidence of agent activity (empty stdout/stderr) — timeout may have occurred on an idle agent".to_string(),
                    }
                }
                ActivityEvidence::HasToolUseCalls
                | ActivityEvidence::HasStructuredOutput
                | ActivityEvidence::SubstantialElapsedTime { .. } => {
                    // At least one evidence marker — qualifies
                    let evidence_desc = match activity {
                        ActivityEvidence::HasToolUseCalls => {
                            "agent emitted tool-use calls".to_string()
                        }
                        ActivityEvidence::HasStructuredOutput => {
                            "agent produced structured output".to_string()
                        }
                        ActivityEvidence::SubstantialElapsedTime { .. } => {
                            format!(
                                "substantial time elapsed ({} of timeout budget used)",
                                format_percent(elapsed_fraction)
                            )
                        }
                        ActivityEvidence::NoEvidence => unreachable!(),
                    };

                    TimeoutEligibility::Eligible {
                        reason: format!(
                            "agent wall-clock timeout with evidence of productive work — {} (elapsed: {:.2} of timeout)",
                            evidence_desc,
                            elapsed_fraction
                        ),
                    }
                }
            }
        }

        TimeoutOrigin::HandlerTimeout { ref gate_name } => {
            if !policy.handler_timeout {
                return TimeoutEligibility::NotEligible {
                    reason: "handler timeouts are not enabled in policy".to_string(),
                };
            }

            // Handler timeouts qualify if there's evidence of substantial work
            match activity {
                ActivityEvidence::SubstantialElapsedTime { .. } => {
                    let gate = gate_name
                        .as_ref()
                        .map(|n| n.as_str())
                        .unwrap_or("validation gate");

                    TimeoutEligibility::Eligible {
                        reason: format!(
                            "handler timeout on {} with substantial elapsed time ({:.2} of timeout)",
                            gate,
                            elapsed_fraction
                        ),
                    }
                }
                _ => TimeoutEligibility::NotEligible {
                    reason: "handler timeout without evidence of substantial validation work"
                        .to_string(),
                },
            }
        }

        TimeoutOrigin::BeadStoreTimeout => {
            // Bead-store timeouts never qualify — no agent activity evidence
            TimeoutEligibility::NotEligible {
                reason: "bead-store timeout (no agent activity evidence — represents infrastructure slowness, not productive work)".to_string(),
            }
        }

        TimeoutOrigin::OutcomeProcessingTimeout => {
            // Outcome-processing timeouts never qualify
            TimeoutEligibility::NotEligible {
                reason: "outcome-processing timeout (no agent activity evidence — represents infrastructure slowness)".to_string(),
            }
        }
    }
}

/// Classify the origin of a timeout based on execution context.
///
/// Distinguishes between agent wall-clock timeouts, handler timeouts,
/// bead-store timeouts, and outcome-processing timeouts.
fn classify_timeout_origin(outcome: &AgentOutcome, _duration: Duration) -> TimeoutOrigin {
    // Exit code 124 is GNU timeout wrapper — agent wall-clock timeout
    if outcome.exit_code == 124 {
        // Assume agent timeout of 1 hour if not inferable from context
        return TimeoutOrigin::AgentWallclock {
            timeout_duration: Duration::from_secs(3600),
        };
    }

    // Heuristic: if stderr contains "bead store" or CLI timeout patterns, classify as bead-store timeout
    let stderr_lower = outcome.stderr.to_lowercase();
    if stderr_lower.contains("bead store") || stderr_lower.contains("bead timeout") {
        return TimeoutOrigin::BeadStoreTimeout;
    }

    // Heuristic: if stderr contains "outcome handler" or "validation gate", classify as handler timeout
    if stderr_lower.contains("outcome handler")
        || stderr_lower.contains("validation gate")
        || stderr_lower.contains("gate timeout")
    {
        // Extract gate name if present
        let gate_name = extract_gate_name(&outcome.stderr);
        return TimeoutOrigin::HandlerTimeout { gate_name };
    }

    // Default: assume agent wall-clock timeout
    TimeoutOrigin::AgentWallclock {
        timeout_duration: Duration::from_secs(3600),
    }
}

/// Extract a validation gate name from stderr output.
fn extract_gate_name(stderr: &str) -> Option<String> {
    // Look for patterns like "gate 'cargo-test' timed out" or "validation gate 'tests'"
    let patterns = [
        "gate '",
        "gate \"",
        "validation gate '",
        "validation gate \"",
        "gate `",
        "validation gate `",
    ];

    for pattern in &patterns {
        if let Some(idx) = stderr.find(pattern) {
            let start = idx + pattern.len();
            if let Some(end) = stderr[start..].find(['\'', '"', '`']) {
                return Some(stderr[start..start + end].to_string());
            }
        }
    }

    None
}

/// Detect evidence that the agent was actively working before the timeout.
///
/// Returns the strongest evidence marker found (in order of confidence):
/// 1. Tool-use calls (stderr non-empty and contains tool markers)
/// 2. Structured output (stdout non-empty)
/// 3. Substantial elapsed time (duration >= 90% of timeout)
/// 4. No evidence (all checks failed)
fn detect_activity_evidence(outcome: &AgentOutcome) -> ActivityEvidence {
    // Check for tool-use calls in stderr (strongest evidence)
    if !outcome.stderr.is_empty() {
        let stderr_lower = outcome.stderr.to_lowercase();
        let tool_markers = [
            "tool_use:",
            "tool_use_id",
            "useagent",
            "useread",
            "usebash",
            "uselsp",
            "useedit",
            "usewrite",
            "<invoke>",
            "tool_result",
        ];

        for marker in &tool_markers {
            if stderr_lower.contains(marker) {
                return ActivityEvidence::HasToolUseCalls;
            }
        }
    }

    // Check for structured output in stdout
    if !outcome.stdout.is_empty() {
        return ActivityEvidence::HasStructuredOutput;
    }

    // Default: no evidence found
    ActivityEvidence::NoEvidence
}

/// Format a fraction as a percentage (0.0-1.0 -> "0%").
fn format_percent(fraction: f64) -> String {
    format!("{}%", (fraction * 100.0).round())
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TimeoutTriggeredPolicy;

    fn test_policy() -> TimeoutTriggeredPolicy {
        TimeoutTriggeredPolicy {
            enabled: true,
            agent_wallclock_timeout: true,
            handler_timeout: true,
            min_elapsed_fraction: 0.9,
        }
    }

    #[test]
    fn eligible_agent_wallclock_timeout_with_output() {
        let outcome = AgentOutcome {
            exit_code: 124,
            stdout: "analysis complete\n".to_string(),
            stderr: "".to_string(),
        };

        let eligibility = classify_timeout_eligibility(
            &outcome,
            Duration::from_secs(3540), // 59 minutes of a 1-hour timeout
            &test_policy(),
        );

        assert!(eligibility.is_eligible());
        assert!(eligibility.reason().contains("agent wall-clock timeout"));
        assert!(eligibility.reason().contains("structured output"));
    }

    #[test]
    fn eligible_agent_wallclock_timeout_with_tool_calls() {
        let outcome = AgentOutcome {
            exit_code: 124,
            stdout: "".to_string(),
            stderr: "tool_use_id: \"call-1\"\ntool_result: success".to_string(),
        };

        let eligibility = classify_timeout_eligibility(
            &outcome,
            Duration::from_secs(3540), // 59 minutes of a 1-hour timeout
            &test_policy(),
        );

        assert!(eligibility.is_eligible());
        assert!(eligibility.reason().contains("tool-use calls"));
    }

    #[test]
    fn not_eligible_interrupted() {
        let outcome = AgentOutcome {
            exit_code: 130, // SIGINT
            stdout: "".to_string(),
            stderr: "".to_string(),
        };

        let eligibility =
            classify_timeout_eligibility(&outcome, Duration::from_secs(3600), &test_policy());

        assert!(!eligibility.is_eligible());
        assert!(eligibility.reason().contains("interrupted by signal"));
    }

    #[test]
    fn not_eligible_crash() {
        let outcome = AgentOutcome {
            exit_code: 137, // SIGKILL
            stdout: "".to_string(),
            stderr: "".to_string(),
        };

        let eligibility =
            classify_timeout_eligibility(&outcome, Duration::from_secs(3600), &test_policy());

        assert!(!eligibility.is_eligible());
        assert!(eligibility.reason().contains("signal"));
    }

    #[test]
    fn not_eligible_insufficient_elapsed_fraction() {
        let outcome = AgentOutcome {
            exit_code: 124,
            stdout: "started".to_string(),
            stderr: "".to_string(),
        };

        let eligibility = classify_timeout_eligibility(
            &outcome,
            Duration::from_secs(300), // 5 minutes of a 1-hour timeout (8%)
            &test_policy(),
        );

        assert!(!eligibility.is_eligible());
        assert!(eligibility
            .reason()
            .contains("insufficient elapsed fraction"));
    }

    #[test]
    fn not_eligible_no_activity_evidence() {
        let outcome = AgentOutcome {
            exit_code: 124,
            stdout: "".to_string(),
            stderr: "".to_string(),
        };

        let eligibility = classify_timeout_eligibility(
            &outcome,
            Duration::from_secs(3540), // 59 minutes elapsed
            &test_policy(),
        );

        assert!(!eligibility.is_eligible());
        assert!(eligibility
            .reason()
            .contains("no evidence of agent activity"));
    }

    #[test]
    fn not_eligible_policy_disabled() {
        let outcome = AgentOutcome {
            exit_code: 124,
            stdout: "work done".to_string(),
            stderr: "".to_string(),
        };

        let mut policy = test_policy();
        policy.enabled = false;

        let eligibility =
            classify_timeout_eligibility(&outcome, Duration::from_secs(3540), &policy);

        assert!(!eligibility.is_eligible());
        assert!(eligibility.reason().contains("disabled"));
    }

    #[test]
    fn not_eligible_agent_wallclock_disabled() {
        let outcome = AgentOutcome {
            exit_code: 124,
            stdout: "work done".to_string(),
            stderr: "".to_string(),
        };

        let mut policy = test_policy();
        policy.agent_wallclock_timeout = false;

        let eligibility =
            classify_timeout_eligibility(&outcome, Duration::from_secs(3540), &policy);

        assert!(!eligibility.is_eligible());
        assert!(eligibility
            .reason()
            .contains("agent wall-clock timeouts are not enabled"));
    }

    #[test]
    fn extract_gate_name_from_stderr() {
        let stderr = "validation gate 'cargo-test' exceeded timeout budget";
        assert_eq!(extract_gate_name(stderr), Some("cargo-test".to_string()));

        let stderr = "gate \"integration-tests\" timed out after 120s";
        assert_eq!(
            extract_gate_name(stderr),
            Some("integration-tests".to_string())
        );

        let stderr = "no gate here";
        assert_eq!(extract_gate_name(stderr), None);
    }
}
