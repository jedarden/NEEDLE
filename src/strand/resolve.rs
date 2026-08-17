//! Resolve strand: post-Pluck decision analysis.
//!
//! Resolve evaluates beads after Pluck selection to determine whether they
//! are ready for immediate dispatch, need to be blocked/retried, or should be
//! split into smaller tasks. It uses an LLM prompt to analyze bead context and
//! returns a structured decision with evidence.
//!
//! This is a decision-analysis layer only — it never mutates beads or performs
//! implementation work. All actions are delegated to other strands.

use crate::config::ResolveConfig;
use crate::prompt::PromptBuilder;
use crate::telemetry::Telemetry;
use crate::types::{Bead, BeadId, ResolveDecision, ResolveOutcome, StrandError, StrandResult};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use tracing::Instrument;
use tracing::info_span;

/// Default template for the Resolve prompt.
const DEFAULT_RESOLVE_TEMPLATE: &str = r#"## Resolve Analysis

You are a decision analyst. Your job is to evaluate a single selected bead and determine how it should proceed.

**IMPORTANT:**
- You MUST output ONLY valid JSON (no markdown, no explanation)
- You MUST NOT suggest any implementation work or code changes
- You MUST NOT mutate the bead state or labels
- You ONLY decide the disposition of this bead

### Bead Context

**ID:** {bead_id}
**Title:** {bead_title}
**Description:**
{bead_body}

**Priority:** {priority}
**Status:** {status}
**Assignee:** {assignee}
**Labels:** {labels}

**Workspace:** {workspace_path}

**Dependencies ({dependency_count}):**
{dependencies}

**Dependents ({dependent_count}):**
{dependents}

### Recent Comments (if any)
{bead_comments}

### Your Task

Analyze this bead and output ONE of these decisions:

1. **complete** - The bead is ready for immediate dispatch and implementation
2. **retry** - The bead cannot proceed right now but may succeed later (external dependency, resource contention)
3. **blocked** - The bead has an unmet dependency that blocks all progress
4. **split** - The bead is too large/complex and should be split into smaller child beads

### Output Format

Output ONLY a JSON object (no markdown fencing):

```json
{{
  "decision": "complete|retry|blocked|split",
  "evidence": "Brief explanation of the decision (1-2 sentences)",
  "retry_after_seconds": 600,
  "blocker_id": "bead-id-blocking-this-one",
  "split_reason": "why this needs splitting"
}}
```

### Decision Rules

**Use "complete" when:**
- The bead has unmet dependencies or unclear requirements
- All dependencies are satisfied (or can be worked around)
- The workspace exists and is accessible
- Requirements are clear enough to implement

**Use "retry" when:**
- External service is temporarily unavailable
- File lock or resource contention exists
- Network or infrastructure transient issue
- The issue may resolve within {retry_after_seconds} seconds (default 600)

**Use "blocked" when:**
- A dependency bead is not yet complete
- The blocker cannot be bypassed or worked around
- Set block_id to the blocking bead's ID

**Use "split" when:**
- The bead describes multiple independent deliverables
- The task would take >2 hours to implement
- The bead has accumulated 3+ consecutive failures
- Provide split_reason explaining the decomposition

### Default Behavior

If unsure, default to "complete" — let the dispatcher handle it.
"#;

/// Structured response from the Resolve LLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveResponse {
    /// The decision made.
    pub decision: String,
    /// Evidence supporting the decision.
    pub evidence: String,
    /// For "retry" decisions: how many seconds to wait before retry.
    #[serde(default = "default_retry_after")]
    pub retry_after_seconds: u64,
    /// For "blocked" decisions: the ID of the blocking bead.
    pub blocker_id: Option<String>,
    /// For "split" decisions: explanation of why splitting is needed.
    pub split_reason: Option<String>,
}

fn default_retry_after() -> u64 {
    600
}

/// Validation result for a ResolveResponse.
#[derive(Debug)]
pub enum ValidationResult {
    /// Response is valid and can be parsed.
    Valid {
        decision: ResolveDecision,
        outcome: ResolveOutcome,
    },
    /// Response is invalid or incomplete.
    Invalid { reason: String },
}

/// The Resolve strand — post-Pluck decision analysis.
pub struct ResolveStrand {
    /// Configuration for resolve behavior.
    config: ResolveConfig,
    /// Telemetry emitter.
    telemetry: Telemetry,
    /// Custom prompt template content (if loaded from file).
    custom_template: Option<String>,
}

impl ResolveStrand {
    /// Create a new ResolveStrand with default configuration.
    pub fn new(config: ResolveConfig, telemetry: Telemetry) -> Self {
        let custom_template = if let Some(ref path) = config.custom_template_path {
            // Try to load custom template
            std::fs::read_to_string(path)
                .ok()
                .map(|s| s.trim().to_string())
        } else {
            None
        };

        ResolveStrand {
            config,
            telemetry,
            custom_template,
        }
    }

    /// Build the resolve prompt for a bead.
    fn build_prompt(&self, bead: &Bead, workspace: &Path) -> Result<String> {
        let template = if let Some(ref custom) = self.custom_template {
            custom.as_str()
        } else if self.config.use_default_template {
            DEFAULT_RESOLVE_TEMPLATE
        } else {
            return Ok(String::new());
        };

        // Format dependencies
        let dependencies = if bead.dependencies.is_empty() {
            "None".to_string()
        } else {
            bead
                .dependencies
                .iter()
                .map(|d| format!("- {} ({})", d.id, d.dependency_type))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // Format dependents
        let dependents = if bead.dependents.is_empty() {
            "None".to_string()
        } else {
            bead
                .dependents
                .iter()
                .map(|d| format!("- {} ({})", d.id, d.dependency_type))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // Format labels
        let labels = if bead.labels.is_empty() {
            "None".to_string()
        } else {
            bead.labels.join(", ")
        };

        // Format comments (truncated)
        let comments = if bead.comments.is_empty() {
            "None".to_string()
        } else {
            bead
                .comments
                .iter()
                .take(3) // Only show most recent 3
                .map(|c| {
                    format!(
                        "- [{}] {}",
                        c.created_at.format("%Y-%m-%d %H:%M"),
                        c.text
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let assignee = bead.assignee.as_deref().unwrap_or("unassigned");

        // Substitute variables
        Ok(template
            .replace("{bead_id}", bead.id.as_ref())
            .replace("{bead_title}", &bead.title)
            .replace("{bead_body}", bead.body.as_deref().unwrap_or("(no description)"))
            .replace("{priority}", &bead.priority.to_string())
            .replace("{status}", &format!("{:?}", bead.status))
            .replace("{assignee}", assignee)
            .replace("{labels}", &labels)
            .replace("{workspace_path}", &workspace.display().to_string())
            .replace("{dependency_count}", &bead.dependencies.len().to_string())
            .replace("{dependencies}", &dependencies)
            .replace("{dependent_count}", &bead.dependents.len().to_string())
            .replace("{dependents}", &dependents)
            .replace("{bead_comments}", &comments))
    }

    /// Parse and validate the LLM response.
    fn parse_response(&self, response_text: &str) -> ValidationResult {
        // Strip markdown code fencing if present
        let cleaned = response_text.trim().trim_start_matches("```json").trim_end_matches("```").trim();

        // Parse JSON
        let parsed: ResolveResponse = match serde_json::from_str(cleaned) {
            Ok(r) => r,
            Err(e) => {
                return ValidationResult::Invalid {
                    reason: format!("Failed to parse JSON response: {}", e),
                };
            }
        };

        // Validate decision is one of the four allowed values
        let decision = match parsed.decision.to_lowercase().as_str() {
            "complete" => ResolveDecision::Complete,
            "retry" => ResolveDecision::Retry,
            "blocked" => ResolveDecision::Blocked,
            "split" => ResolveDecision::Split,
            _ => {
                return ValidationResult::Invalid {
                    reason: format!("Invalid decision value: {}", parsed.decision),
                };
            }
        };

        // Build outcome based on decision
        let outcome = match decision {
            ResolveDecision::Complete => ResolveOutcome::Complete {
                evidence: parsed.evidence.clone(),
            },
            ResolveDecision::Retry => ResolveOutcome::Retry {
                evidence: parsed.evidence.clone(),
                retry_after_seconds: parsed.retry_after_seconds,
            },
            ResolveDecision::Blocked => {
                if let Some(blocker_id) = &parsed.blocker_id {
                    ResolveOutcome::Blocked {
                        evidence: parsed.evidence.clone(),
                        blocker_id: BeadId::from(blocker_id.clone()),
                    }
                } else {
                    return ValidationResult::Invalid {
                        reason: "Blocked decision requires blocker_id".to_string(),
                    };
                }
            }
            ResolveDecision::Split => ResolveOutcome::Split {
                evidence: parsed.evidence.clone(),
                split_reason: parsed.split_reason.clone(),
            },
        };

        ValidationResult::Valid { decision, outcome }
    }

    /// Invoke the LLM resolver.
    async fn invoke_resolver(&self, prompt: &str) -> Result<Output> {
        let timeout = Duration::from_secs(self.config.timeout_secs);

        // Use claude as the default resolver (configurable in future)
        let mut child = Command::new("claude")
            .arg("--message")
            .arg(prompt)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn claude process")?;

        // Wait with timeout
        let output = tokio::time::timeout(timeout, async {
            let output = child.wait_with_output().await?;
            Ok::<Output, anyhow::Error>(output)
        })
        .await
        .context("Resolver call timed out")??;

        Ok(output)
    }
}

#[async_trait::async_trait]
impl super::Strand for ResolveStrand {
    fn name(&self) -> &str {
        "resolve"
    }

    async fn evaluate(&self, _store: &dyn crate::bead_store::BeadStore, _exclusions: &HashSet<BeadId>) -> StrandResult {
        // Resolve doesn't query the store — it receives beads from the caller
        // This is evaluated in the worker loop, not in the strand waterfall
        StrandResult::NoWork
    }
}

/// Evaluate a single bead for resolution.
///
/// This is called by the worker after Pluck selection to determine the
/// disposition of the bead before dispatch.
impl ResolveStrand {
    /// Resolve a single bead and return the outcome.
    pub async fn resolve_bead(&self, bead: &Bead, workspace: &Path) -> Result<ResolveOutcome> {
        let start = std::time::Instant::now();

        // Build prompt
        let prompt = self.build_prompt(bead, workspace)?;

        if prompt.is_empty() {
            // No template configured — default to complete
            tracing::warn!("No resolve template configured, defaulting to complete");
            return Ok(ResolveOutcome::Complete {
                evidence: "No resolve template configured — defaulting to complete".to_string(),
            });
        }

        tracing::debug!(
            bead_id = %bead.id,
            prompt_length = prompt.len(),
            "Invoking resolver"
        );

        // Invoke resolver with span instrumentation
        // Do NOT hold an EnteredSpan guard across the await — use .instrument() instead
        let span = info_span!(
            "strand.resolve",
            bead_id = %bead.id,
            bead_title = %bead.title,
        );

        let output = match self.invoke_resolver(&prompt).instrument(span).await {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(error = %e, "Resolver invocation failed");
                // On failure, default to complete with a note
                return Ok(ResolveOutcome::Complete {
                    evidence: format!("Resolver failed: {}", e),
                });
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        tracing::debug!(
            stdout_len = stdout.len(),
            stderr_len = stderr.len(),
            exit_code = output.status.code(),
            "Resolver completed"
        );

        // Parse and validate response
        let validation = self.parse_response(&stdout);

        match validation {
            ValidationResult::Valid { decision, outcome } => {
                tracing::info!(
                    bead_id = %bead.id,
                    decision = ?decision,
                    evidence = %outcome.evidence(),
                    "Resolve decision valid"
                );

                // Emit telemetry
                let _ = self.telemetry.emit(crate::telemetry::EventKind::ResolveEvaluated {
                    bead_id: bead.id.clone(),
                    decision: format!("{:?}", decision),
                    evidence: outcome.evidence().to_string(),
                    duration_ms: start.elapsed().as_millis() as u64,
                });

                Ok(outcome)
            }
            ValidationResult::Invalid { reason } => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %reason,
                    "Invalid resolve response, defaulting to complete"
                );

                // On invalid response, default to complete with error evidence
                Ok(ResolveOutcome::Complete {
                    evidence: format!("Invalid resolver response: {}", reason),
                })
            }
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BeadId, BeadStatus, ClaimResult, BrDependency};
    use crate::bead_store::Filters;
    use crate::telemetry::test_utils::TestHelper;
    use chrono::{Utc, TimeZone};
    use std::path::PathBuf;

    /// A mock store for tests.
    struct MockStore;

    #[async_trait::async_trait]
    impl crate::bead_store::BeadStore for MockStore {
        async fn list_all(&self) -> Result<Vec<crate::types::Bead>> {
            Ok(vec![])
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<crate::types::Bead>> {
            Ok(vec![])
        }
        async fn show(&self, _id: &BeadId) -> Result<crate::types::Bead> {
            anyhow::bail!("not found")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn block(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new-bead".to_string()))
        }
        async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
            Ok(crate::bead_store::RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<crate::bead_store::RepairReport> {
            Ok(crate::bead_store::RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "claim_auto not supported in mock".to_string(),
            })
        }
        fn has_valid_store(&self) -> bool {
            true
        }
    }

    fn make_test_bead() -> Bead {
        Bead {
            id: BeadId::from("needle-test"),
            title: "Test bead".to_string(),
            body: Some("Test description".to_string()),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec![],
            workspace: PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn resolve_config_default() {
        let config = ResolveConfig::default();
        assert_eq!(config.timeout_secs, 60);
        assert!(config.custom_template_path.is_none());
        assert!(config.use_default_template);
    }

    #[test]
    fn parse_valid_complete_response() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        let response = r#"{"decision": "complete", "evidence": "All dependencies satisfied, ready to implement"}"#;
        let result = strand.parse_response(response);

        match result {
            ValidationResult::Valid { decision, outcome } => {
                assert_eq!(decision, ResolveDecision::Complete);
                assert_eq!(outcome.evidence(), "All dependencies satisfied, ready to implement");
            }
            ValidationResult::Invalid { .. } => panic!("Expected valid response"),
        }
    }

    #[test]
    fn parse_valid_retry_response() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        let response = r#"{"decision": "retry", "evidence": "External service temporarily unavailable", "retry_after_seconds": 300}"#;
        let result = strand.parse_response(response);

        match result {
            ValidationResult::Valid { decision, outcome } => {
                assert_eq!(decision, ResolveDecision::Retry);
                assert_eq!(outcome.evidence(), "External service temporarily unavailable");
                if let ResolveOutcome::Retry { retry_after_seconds, .. } = outcome {
                    assert_eq!(retry_after_seconds, 300);
                } else {
                    panic!("Expected Retry outcome");
                }
            }
            ValidationResult::Invalid { .. } => panic!("Expected valid response"),
        }
    }

    #[test]
    fn parse_valid_blocked_response() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        let response = r#"{"decision": "blocked", "evidence": "Depends on unimplemented feature", "blocker_id": "needle-dep-001"}"#;
        let result = strand.parse_response(response);

        match result {
            ValidationResult::Valid { decision, outcome } => {
                assert_eq!(decision, ResolveDecision::Blocked);
                assert_eq!(outcome.evidence(), "Depends on unimplemented feature");
                if let ResolveOutcome::Blocked { blocker_id, .. } = outcome {
                    assert_eq!(blocker_id.as_ref(), "needle-dep-001");
                } else {
                    panic!("Expected Blocked outcome");
                }
            }
            ValidationResult::Invalid { .. } => panic!("Expected valid response"),
        }
    }

    #[test]
    fn parse_valid_split_response() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        let response = r#"{"decision": "split", "evidence": "Task is too large", "split_reason": "Should be decomposed into 3 phases"}"#;
        let result = strand.parse_response(response);

        match result {
            ValidationResult::Valid { decision, outcome } => {
                assert_eq!(decision, ResolveDecision::Split);
                assert_eq!(outcome.evidence(), "Task is too large");
                if let ResolveOutcome::Split { split_reason, .. } = outcome {
                    assert_eq!(split_reason, Some("Should be decomposed into 3 phases".to_string()));
                } else {
                    panic!("Expected Split outcome");
                }
            }
            ValidationResult::Invalid { .. } => panic!("Expected valid response"),
        }
    }

    #[test]
    fn parse_invalid_json() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        let response = "not valid json";
        let result = strand.parse_response(response);

        match result {
            ValidationResult::Invalid { reason } => {
                assert!(reason.contains("Failed to parse JSON"));
            }
            ValidationResult::Valid { .. } => panic!("Expected invalid response"),
        }
    }

    #[test]
    fn parse_invalid_decision_value() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        let response = r#"{"decision": "invalid", "evidence": "test"}"#;
        let result = strand.parse_response(response);

        match result {
            ValidationResult::Invalid { reason } => {
                assert!(reason.contains("Invalid decision value"));
            }
            ValidationResult::Valid { .. } => panic!("Expected invalid response"),
        }
    }

    #[test]
    fn parse_blocked_without_blocker_id() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        let response = r#"{"decision": "blocked", "evidence": "test"}"#;
        let result = strand.parse_response(response);

        match result {
            ValidationResult::Invalid { reason } => {
                assert!(reason.contains("requires blocker_id"));
            }
            ValidationResult::Valid { .. } => panic!("Expected invalid response"),
        }
    }

    #[test]
    fn parse_response_with_markdown_fencing() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        let response = r#"```json
{"decision": "complete", "evidence": "test"}
```"#;
        let result = strand.parse_response(response);

        match result {
            ValidationResult::Valid { decision, .. } => {
                assert_eq!(decision, ResolveDecision::Complete);
            }
            ValidationResult::Invalid { .. } => panic!("Expected valid response"),
        }
    }

    #[test]
    fn parse_retry_with_default_timeout() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        let response = r#"{"decision": "retry", "evidence": "test"}"#;
        let result = strand.parse_response(response);

        match result {
            ValidationResult::Valid { decision, outcome } => {
                assert_eq!(decision, ResolveDecision::Retry);
                if let ResolveOutcome::Retry { retry_after_seconds, .. } = outcome {
                    assert_eq!(retry_after_seconds, 600); // default
                } else {
                    panic!("Expected Retry outcome");
                }
            }
            ValidationResult::Invalid { .. } => panic!("Expected valid response"),
        }
    }

    #[test]
    fn build_prompt_includes_all_variables() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        let bead = make_test_bead();
        let workspace = PathBuf::from("/tmp/test");
        let prompt = strand.build_prompt(&bead, &workspace).unwrap();

        assert!(prompt.contains("needle-test"));
        assert!(prompt.contains("Test bead"));
        assert!(prompt.contains("Test description"));
        assert!(prompt.contains("/tmp/test"));
    }

    #[test]
    fn strand_name_is_resolve() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        assert_eq!(strand.name(), "resolve");
    }

    #[tokio::test]
    async fn evaluate_returns_no_work() {
        let strand = ResolveStrand::new(
            ResolveConfig::default(),
            Telemetry::new("test".to_string()),
        );
        let store = MockStore;
        let exclusions = HashSet::new();
        let result = strand.evaluate(&store, &exclusions).await;

        match result {
            StrandResult::NoWork => {}
            other => panic!("Expected NoWork, got {:?}", other),
        }
    }
}
