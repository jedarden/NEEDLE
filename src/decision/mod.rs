//! Decision point detection from transcripts.
//!
//! This module analyzes Claude Code session transcripts to identify decision
//! points — moments where the agent chose between alternatives. Decision points
//! are used to create ADR-style learning records that preserve the "why" behind
//! technical choices, not just the "what".
//!
//! ## Decision Point Detection
//!
//! A decision point is detected when:
//! 1. An attempt fails, followed by a different successful approach
//! 2. Thinking contains decision keywords ("instead", "alternatively", "better approach")
//! 3. Multiple file reads precede an edit (exploration before choice)
//! 4. A failed tool call is followed by a different tool (not a retry)
//!
//! ## ADR Creation
//!
//! Detected decisions are written to `.beads/decisions/<id>.md` with:
//! - Context: Problem description
//! - Alternatives Considered: Options evaluated
//! - Decision: Chosen approach
//! - Rationale: Why this choice was made
//! - Outcome: Results

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::transcript::{ParsedTranscript, TranscriptAction, ActionType};
use crate::types::BeadId;

// ──────────────────────────────────────────────────────────────────────────────
// Decision Point Types
// ──────────────────────────────────────────────────────────────────────────────

/// A detected decision point from a transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionPoint {
    /// Unique decision ID (e.g., "nd-a3f8").
    pub id: String,
    /// Bead ID this decision is associated with.
    pub bead_id: Option<BeadId>,
    /// Session ID where this decision was made.
    pub session_id: String,
    /// When the decision was made.
    pub timestamp: DateTime<Utc>,
    /// Short title of the decision.
    pub title: String,
    /// Problem context (what led to the decision).
    pub context: String,
    /// Alternatives considered.
    pub alternatives: Vec<String>,
    /// The chosen approach.
    pub decision: String,
    /// Rationale for the choice (why this approach).
    pub rationale: String,
    /// Outcome of the decision (what happened).
    pub outcome: String,
    /// Whether the decision succeeded.
    pub succeeded: bool,
}

impl DecisionPoint {
    /// Generate a short decision ID (6-character random suffix).
    pub fn generate_id() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        format!("nd-{:04x}", nanos % 0x10000)
    }

    /// Format as ADR markdown.
    pub fn to_adr_markdown(&self) -> String {
        let mut md = format!("# ADR: {}\n\n", self.title);
        md.push_str(&format!("**Decision ID:** {}\n", self.id));
        if let Some(ref bead) = self.bead_id {
            md.push_str(&format!("**Bead:** {}\n", bead));
        }
        md.push_str(&format!("**Date:** {}\n\n", self.timestamp.format("%Y-%m-%d")));

        md.push_str("## Context\n\n");
        md.push_str(&self.context);
        md.push_str("\n\n");

        if !self.alternatives.is_empty() {
            md.push_str("## Alternatives Considered\n\n");
            for (i, alt) in self.alternatives.iter().enumerate() {
                md.push_str(&format!("{}. {}\n", i + 1, alt));
            }
            md.push_str("\n");
        }

        md.push_str("## Decision\n\n");
        md.push_str(&self.decision);
        md.push_str("\n\n");

        md.push_str("## Rationale\n\n");
        md.push_str(&self.rationale);
        md.push_str("\n\n");

        md.push_str("## Outcome\n\n");
        md.push_str(&self.outcome);
        if self.succeeded {
            md.push_str(" (success)");
        } else {
            md.push_str(" (failed)");
        }
        md.push_str("\n");

        md
    }

    /// Format as ADR-lite for CLAUDE.md.
    pub fn to_adr_lite(&self) -> String {
        format!(
            "<!-- needle-learning:{} -->\n\
             - **Decision**: {}\n\
               **Context**: {}\n\
               **Rationale**: {}\n\
               **ADR**: `.beads/decisions/{}.md`\n\
             <!-- /needle-learning:{} -->",
            self.id,
            self.decision.chars().take(100).collect::<String>(),
            self.context.chars().take(100).collect::<String>(),
            self.rationale.chars().take(150).collect::<String>(),
            self.id,
            self.id
        )
    }
}

/// Analysis result from decision detection.
#[derive(Debug, Clone)]
pub struct DecisionAnalysis {
    /// Detected decision points.
    pub decisions: Vec<DecisionPoint>,
    /// Number of transcripts analyzed.
    pub transcripts_analyzed: usize,
    /// Number of action sequences examined.
    pub sequences_examined: usize,
}

// ──────────────────────────────────────────────────────────────────────────────
// Decision Detector
// ──────────────────────────────────────────────────────────────────────────────

/// Detects decision points in transcripts.
pub struct DecisionDetector {
    /// Minimum confidence threshold for decision detection.
    min_confidence: f32,
}

impl DecisionDetector {
    /// Create a new detector with default settings.
    pub fn new() -> Self {
        DecisionDetector {
            min_confidence: 0.6,
        }
    }

    /// Set the minimum confidence threshold (0.0 to 1.0).
    pub fn with_min_confidence(mut self, threshold: f32) -> Self {
        self.min_confidence = threshold.clamp(0.0, 1.0);
        self
    }

    /// Analyze transcripts for decision points.
    pub fn analyze(&self, transcripts: &[ParsedTranscript]) -> Result<DecisionAnalysis> {
        let mut decisions = Vec::new();
        let mut sequences_examined = 0;

        for transcript in transcripts {
            let detected = self.detect_decisions_in_transcript(transcript)?;
            sequences_examined += detected.len();
            decisions.extend(detected);
        }

        Ok(DecisionAnalysis {
            decisions,
            transcripts_analyzed: transcripts.len(),
            sequences_examined,
        })
    }

    /// Detect decision points within a single transcript.
    fn detect_decisions_in_transcript(&self, transcript: &ParsedTranscript) -> Result<Vec<DecisionPoint>> {
        let mut decisions = Vec::new();
        let actions = &transcript.actions;

        // Scan for decision patterns
        let mut i = 0;
        while i < actions.len() {
            // Check for failure-recovery pattern
            if let Some(decision) = self.detect_failure_recovery(transcript, actions, i)? {
                decisions.push(decision);
                i += 5; // Skip ahead after processing a sequence
                continue;
            }

            // Check for explicit decision in thinking
            if actions[i].action_type == ActionType::Thinking {
                if let Some(decision) = self.detect_explicit_decision(transcript, actions, i)? {
                    decisions.push(decision);
                }
            }

            // Check for exploration-before-choice pattern
            if let Some(decision) = self.detect_exploration_choice(transcript, actions, i)? {
                decisions.push(decision);
                i += 3; // Skip ahead after processing
                continue;
            }

            i += 1;
        }

        Ok(decisions)
    }

    /// Detect attempt → failure → different approach → success pattern.
    fn detect_failure_recovery(
        &self,
        transcript: &ParsedTranscript,
        actions: &[TranscriptAction],
        start_idx: usize,
    ) -> Result<Option<DecisionPoint>> {
        // Need at least 4 actions: attempt, failure, alternative, success
        if start_idx + 4 > actions.len() {
            return Ok(None);
        }

        let window = &actions[start_idx..start_idx + 4];

        // Look for: tool use (attempt) → text (error/failure) → different tool (recovery) → success
        let attempt = &window[0];
        let failure = &window[1];
        let recovery = &window[2];
        let success = &window[3];

        // Verify pattern: tool → text with error → different tool → success
        if attempt.action_type != ActionType::ToolUse {
            return Ok(None);
        }

        // Check for failure indicators in text
        let is_failure = failure.action_type == ActionType::Text
            && self.contains_error_indicator(&failure.description);

        if !is_failure {
            return Ok(None);
        }

        // Recovery must be a different tool
        let is_different_tool = recovery.action_type == ActionType::ToolUse
            && attempt.tool_name.as_ref() != recovery.tool_name.as_ref();

        if !is_different_tool {
            return Ok(None);
        }

        // Success indicates recovery worked
        let is_success = success.action_type == ActionType::Text
            || success.action_type == ActionType::ToolUse;

        if !is_success {
            return Ok(None);
        }

        // Build decision point
        let attempt_tool = attempt.tool_name.as_deref().unwrap_or("unknown");
        let recovery_tool = recovery.tool_name.as_deref().unwrap_or("unknown");

        Ok(Some(DecisionPoint {
            id: DecisionPoint::generate_id(),
            bead_id: transcript.bead_id.clone(),
            session_id: transcript.session_id.clone(),
            timestamp: transcript.modified_at,
            title: format!("Use {} instead of {}", recovery_tool, attempt_tool),
            context: format!(
                "Initial attempt with {} failed: {}",
                attempt_tool,
                failure.description.chars().take(100).collect::<String>()
            ),
            alternatives: vec![
                format!("Retry with {}", attempt_tool),
                format!("Switch to {}", recovery_tool),
            ],
            decision: format!("Use {} to handle the task", recovery_tool),
            rationale: format!(
                "{} failed, {} succeeded",
                attempt_tool, recovery_tool
            ),
            outcome: format!("{} completed the task successfully", recovery_tool),
            succeeded: true,
        }))
    }

    /// Detect explicit decision statements in thinking blocks.
    fn detect_explicit_decision(
        &self,
        transcript: &ParsedTranscript,
        actions: &[TranscriptAction],
        idx: usize,
    ) -> Result<Option<DecisionPoint>> {
        let thinking = &actions[idx];
        if thinking.action_type != ActionType::Thinking {
            return Ok(None);
        }

        let text = thinking.description.to_lowercase();

        // Decision keywords
        let decision_keywords = [
            "instead",
            "alternatively",
            "better approach",
            "decided to",
            "choose to",
            "will use",
            "going with",
        ];

        let has_keyword = decision_keywords.iter().any(|kw| text.contains(kw));
        if !has_keyword {
            return Ok(None);
        }

        // Extract rationale from thinking (first 200 chars)
        let rationale = thinking.description.chars().take(200).collect::<String>();

        // Look ahead for the action that implements the decision
        let decision_action = actions.get(idx + 1);
        let decision = if let Some(action) = decision_action {
            match action.action_type {
                ActionType::ToolUse => action.tool_name.as_deref()
                    .map(|t| format!("Use {}", t))
                    .unwrap_or_else(|| "Take action".to_string()),
                ActionType::Text => "Respond to user".to_string(),
                ActionType::Thinking => "Continue reasoning".to_string(),
            }
        } else {
            "Take action".to_string()
        };

        // Build decision point
        Ok(Some(DecisionPoint {
            id: DecisionPoint::generate_id(),
            bead_id: transcript.bead_id.clone(),
            session_id: transcript.session_id.clone(),
            timestamp: transcript.modified_at,
            title: decision.clone(),
            context: "Analysis of options during task execution".to_string(),
            alternatives: vec!["Considered alternatives".to_string()],
            decision,
            rationale,
            outcome: "Decision implemented".to_string(),
            succeeded: true, // Optimistic default
        }))
    }

    /// Detect exploration-before-choice pattern (multiple reads before edit).
    fn detect_exploration_choice(
        &self,
        transcript: &ParsedTranscript,
        actions: &[TranscriptAction],
        start_idx: usize,
    ) -> Result<Option<DecisionPoint>> {
        // Need at least 3 actions
        if start_idx + 3 > actions.len() {
            return Ok(None);
        }

        let window = &actions[start_idx..start_idx + 3];

        // Look for: Read → Read → Edit/Writes (explored then chose)
        let first = &window[0];
        let second = &window[1];
        let third = &window[2];

        let is_exploration = first.action_type == ActionType::ToolUse
            && first.tool_name.as_deref() == Some("Read")
            && second.action_type == ActionType::ToolUse
            && second.tool_name.as_deref() == Some("Read");

        if !is_exploration {
            return Ok(None);
        }

        let is_choice = third.action_type == ActionType::ToolUse
            && (third.tool_name.as_deref() == Some("Edit")
                || third.tool_name.as_deref() == Some("Write"));

        if !is_choice {
            return Ok(None);
        }

        // Extract file paths from descriptions
        let first_file = self.extract_file_path(&first.description);
        let second_file = self.extract_file_path(&second.description);
        let chosen_file = self.extract_file_path(&third.description);

        Ok(Some(DecisionPoint {
            id: DecisionPoint::generate_id(),
            bead_id: transcript.bead_id.clone(),
            session_id: transcript.session_id.clone(),
            timestamp: transcript.modified_at,
            title: format!("Edit {} over {}", chosen_file, first_file),
            context: format!(
                "Explored multiple files before choosing where to make changes"
            ),
            alternatives: vec![
                format!("Edit {}", first_file),
                format!("Edit {}", second_file),
            ],
            decision: format!("Edit {}", chosen_file),
            rationale: format!(
                "After reviewing both {}, {} was the correct location",
                first_file, chosen_file
            ),
            outcome: format!("Modified {}", chosen_file),
            succeeded: true,
        }))
    }

    /// Check if text contains error/failure indicators.
    fn contains_error_indicator(&self, text: &str) -> bool {
        let indicators = [
            "error",
            "failed",
            "failure",
            "cannot",
            "unable",
            "not found",
            "permission denied",
            "no such file",
        ];

        let text_lower = text.to_lowercase();
        indicators.iter().any(|i| text_lower.contains(i))
    }

    /// Extract file path from tool description.
    fn extract_file_path(&self, description: &str) -> String {
        // Format: "Read: /path/to/file" or "Edit: /path/to/file"
        if let Some(idx) = description.find(':') {
            let path = description[idx + 1..].trim();
            if let Some(last_slash) = path.rfind('/') {
                return path[last_slash + 1..].to_string();
            }
            return path.to_string();
        }
        description.to_string()
    }
}

impl Default for DecisionDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ADR Storage
// ──────────────────────────────────────────────────────────────────────────────

/// Manages storage of ADR decision records.
pub struct AdrStore {
    /// Path to `.beads/decisions/` directory.
    decisions_dir: PathBuf,
}

impl AdrStore {
    /// Create a new ADR store for a workspace.
    pub fn new(workspace: &Path) -> Self {
        let decisions_dir = workspace.join(".beads").join("decisions");
        AdrStore { decisions_dir }
    }

    /// Ensure the decisions directory exists.
    fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.decisions_dir)
            .with_context(|| format!("failed to create decisions directory: {}", self.decisions_dir.display()))?;
        Ok(())
    }

    /// Write a decision point as an ADR file.
    pub fn write_decision(&self, decision: &DecisionPoint) -> Result<PathBuf> {
        self.ensure_dir()?;

        let filename = format!("{}.md", decision.id);
        let path = self.decisions_dir.join(&filename);

        std::fs::write(&path, decision.to_adr_markdown())
            .with_context(|| format!("failed to write ADR: {}", path.display()))?;

        tracing::info!(
            decision_id = %decision.id,
            path = %path.display(),
            "decision: wrote ADR"
        );

        Ok(path)
    }

    /// Write multiple decision points as ADR files.
    pub fn write_decisions(&self, decisions: &[DecisionPoint]) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for decision in decisions {
            paths.push(self.write_decision(decision)?);
        }
        Ok(paths)
    }

    /// List all ADR files in the workspace.
    pub fn list_adrs(&self) -> Result<Vec<PathBuf>> {
        if !self.decisions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut adrs = Vec::new();
        for entry in std::fs::read_dir(&self.decisions_dir)
            .with_context(|| format!("failed to read decisions directory: {}", self.decisions_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                adrs.push(path);
            }
        }
        adrs.sort();
        Ok(adrs)
    }

    /// Load a decision point from an ADR file.
    pub fn load_decision(&self, decision_id: &str) -> Result<DecisionPoint> {
        let path = self.decisions_dir.join(format!("{}.md", decision_id));
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read ADR: {}", path.display()))?;

        self.parse_adr(&content, decision_id)
    }

    /// Parse ADR markdown back into a DecisionPoint.
    fn parse_adr(&self, content: &str, id: &str) -> Result<DecisionPoint> {
        // Simple parsing - extract sections
        let mut title = String::new();
        let mut context = String::new();
        let mut alternatives = Vec::new();
        let mut decision = String::new();
        let mut rationale = String::new();
        let mut outcome = String::new();

        let mut current_section = String::new();
        let mut current_content = String::new();

        for line in content.lines() {
            if line.starts_with("## ") {
                // Save previous section
                match current_section.as_str() {
                    "Context" => context = current_content.trim().to_string(),
                    "Alternatives Considered" => {
                        alternatives = current_content
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(|l| {
                                // Remove "1. " or "1) " prefix
                                let trimmed = l.trim();
                                if let Some(rest) = trimmed.strip_prefix('0') {
                                    rest
                                } else if let Some(rest) = trimmed.strip_prefix('1') {
                                    rest
                                } else if let Some(rest) = trimmed.strip_prefix('2') {
                                    rest
                                } else if let Some(rest) = trimmed.strip_prefix('3') {
                                    rest
                                } else if let Some(rest) = trimmed.strip_prefix('4') {
                                    rest
                                } else if let Some(rest) = trimmed.strip_prefix('5') {
                                    rest
                                } else if let Some(rest) = trimmed.strip_prefix('6') {
                                    rest
                                } else if let Some(rest) = trimmed.strip_prefix('7') {
                                    rest
                                } else if let Some(rest) = trimmed.strip_prefix('8') {
                                    rest
                                } else if let Some(rest) = trimmed.strip_prefix('9') {
                                    rest
                                } else {
                                    trimmed
                                }
                                .trim_start_matches(". ")
                                .trim_start_matches(") ")
                                .to_string()
                            })
                            .collect();
                    }
                    "Decision" => decision = current_content.trim().to_string(),
                    "Rationale" => rationale = current_content.trim().to_string(),
                    "Outcome" => outcome = current_content.trim().to_string(),
                    _ => {}
                }
                current_section = line[3..].to_string();
                current_content = String::new();
            } else if !line.starts_with("**") && !line.starts_with("# ") {
                current_content.push_str(line);
                current_content.push('\n');
            }
        }

        // Handle last section
        match current_section.as_str() {
            "Outcome" => outcome = current_content.trim().to_string(),
            _ => {}
        }

        let succeeded = outcome.to_lowercase().contains("success");

        Ok(DecisionPoint {
            id: id.to_string(),
            bead_id: None,
            session_id: String::new(),
            timestamp: Utc::now(),
            title: title.clone(),
            context,
            alternatives,
            decision,
            rationale,
            outcome,
            succeeded,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{ActionType, TranscriptAction};

    fn make_action(action_type: ActionType, tool_name: Option<&str>, desc: &str) -> TranscriptAction {
        TranscriptAction {
            action_type,
            tool_name: tool_name.map(|s| s.to_string()),
            description: desc.to_string(),
        }
    }

    #[test]
    fn decision_id_generation() {
        let id1 = DecisionPoint::generate_id();
        let id2 = DecisionPoint::generate_id();
        assert!(id1.starts_with("nd-"));
        assert!(id2.starts_with("nd-"));
        assert_ne!(id1, id2); // Should be unique
    }

    #[test]
    fn detect_failure_recovery_pattern() {
        let detector = DecisionDetector::new();

        let actions = vec![
            make_action(ActionType::ToolUse, Some("Read"), "Read: /tmp/file.txt"),
            make_action(ActionType::Text, None, "Error: file not found"),
            make_action(ActionType::ToolUse, Some("Write"), "Write: /tmp/file.txt"),
            make_action(ActionType::Text, None, "File created successfully"),
        ];

        let transcript = ParsedTranscript {
            session_id: "test-session".to_string(),
            modified_at: Utc::now(),
            task_description: Some("Test task".to_string()),
            actions,
            bead_id: None,
        };

        let result = detector.detect_failure_recovery(&transcript, &transcript.actions, 0);
        assert!(result.is_ok());
        let decision = result.unwrap();
        assert!(decision.is_some());
        let decision = decision.unwrap();
        assert_eq!(decision.decision, "Use Write to handle the task");
        assert!(decision.context.contains("file not found"));
    }

    #[test]
    fn detect_exploration_choice_pattern() {
        let detector = DecisionDetector::new();

        let actions = vec![
            make_action(ActionType::ToolUse, Some("Read"), "Read: /tmp/file1.rs"),
            make_action(ActionType::ToolUse, Some("Read"), "Read: /tmp/file2.rs"),
            make_action(ActionType::ToolUse, Some("Edit"), "Edit: /tmp/file2.rs"),
        ];

        let transcript = ParsedTranscript {
            session_id: "test-session".to_string(),
            modified_at: Utc::now(),
            task_description: Some("Test task".to_string()),
            actions,
            bead_id: None,
        };

        let result = detector.detect_exploration_choice(&transcript, &transcript.actions, 0);
        assert!(result.is_ok());
        let decision = result.unwrap();
        assert!(decision.is_some());
        let decision = decision.unwrap();
        assert!(decision.title.contains("file2.rs"));
        assert_eq!(decision.alternatives.len(), 2);
    }

    #[test]
    fn adr_to_markdown_format() {
        let decision = DecisionPoint {
            id: "nd-test".to_string(),
            bead_id: Some(BeadId::from("needle-abc123")),
            session_id: "session-1".to_string(),
            timestamp: Utc::now(),
            title: "Use Write instead of Read".to_string(),
            context: "Read failed with error".to_string(),
            alternatives: vec!["Retry Read".to_string(), "Use Write".to_string()],
            decision: "Use Write to create file".to_string(),
            rationale: "Write creates the file if missing".to_string(),
            outcome: "File created successfully".to_string(),
            succeeded: true,
        };

        let md = decision.to_adr_markdown();
        assert!(md.contains("# ADR: Use Write instead of Read"));
        assert!(md.contains("## Context"));
        assert!(md.contains("## Alternatives Considered"));
        assert!(md.contains("## Decision"));
        assert!(md.contains("## Rationale"));
        assert!(md.contains("## Outcome"));
        assert!(md.contains("**Decision ID:** nd-test"));
        assert!(md.contains("**Bead:** needle-abc123"));
    }

    #[test]
    fn adr_lite_format() {
        let decision = DecisionPoint {
            id: "nd-test".to_string(),
            bead_id: None,
            session_id: "session-1".to_string(),
            timestamp: Utc::now(),
            title: "Test decision".to_string(),
            context: "Test context".to_string(),
            alternatives: vec![],
            decision: "Use this approach".to_string(),
            rationale: "Because it works".to_string(),
            outcome: "Success".to_string(),
            succeeded: true,
        };

        let lite = decision.to_adr_lite();
        assert!(lite.contains("<!-- needle-learning:nd-test -->"));
        assert!(lite.contains("<!-- /needle-learning:nd-test -->"));
        assert!(lite.contains("**Decision**:"));
        assert!(lite.contains("**Context**:"));
        assert!(lite.contains("**Rationale**:"));
        assert!(lite.contains("**ADR**: `.beads/decisions/nd-test.md`"));
    }

    #[test]
    fn contains_error_indicator() {
        let detector = DecisionDetector::new();
        assert!(detector.contains_error_indicator("Error: file not found"));
        assert!(detector.contains_error_indicator("Command failed"));
        assert!(detector.contains_error_indicator("Unable to locate file"));
        assert!(!detector.contains_error_indicator("Operation completed"));
        assert!(!detector.contains_error_indicator("Success"));
    }

    #[test]
    fn extract_file_path_from_description() {
        let detector = DecisionDetector::new();
        assert_eq!(detector.extract_file_path("Read: /home/coding/file.rs"), "file.rs");
        assert_eq!(detector.extract_file_path("Edit: src/main.rs"), "main.rs");
        assert_eq!(detector.extract_file_path("Write: file.txt"), "file.txt");
    }

    #[test]
    fn adr_store_write_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let store = AdrStore::new(workspace);

        let decision = DecisionPoint {
            id: "nd-test42".to_string(),
            bead_id: None,
            session_id: "session-1".to_string(),
            timestamp: Utc::now(),
            title: "Test ADR".to_string(),
            context: "Test context".to_string(),
            alternatives: vec![],
            decision: "Test decision".to_string(),
            rationale: "Test rationale".to_string(),
            outcome: "Test outcome".to_string(),
            succeeded: true,
        };

        let result = store.write_decision(&decision);
        assert!(result.is_ok());

        let adrs = store.list_adrs().unwrap();
        assert_eq!(adrs.len(), 1);
        assert!(adrs[0].to_string_lossy().contains("nd-test42.md"));
    }
}
