//! Transcript discovery and parsing for reflect strand.
//!
//! This module provides functionality to discover and parse Claude Code session
//! transcripts from `.claude/projects/` directories. It extracts key events
//! (assistant actions, tool calls, outcomes) from JSONL transcript files.
//!
//! ## Transcript Location
//!
//! Session transcripts are stored in:
//! ```text
//! ~/.claude/projects/<project-name>/<session-id>.jsonl
//! ```
//!
//! Where `<project-name>` is the workspace path with slashes replaced by dashes
//! (e.g., `/home/coding/NEEDLE` → `-home-coding-NEEDLE`).
//!
//! ## Discovery Strategy
//!
//! 1. Given a workspace path, derive the Claude project name
//! 2. Scan `~/.claude/projects/<project-name>/` for JSONL files
//! 3. Filter by recency (last N sessions or time-based cutoff)
//! 4. Parse each JSONL file to extract structured events

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::learning::DecisionContext;
use crate::types::BeadId;

// ──────────────────────────────────────────────────────────────────────────────
// Transcript entry types (parsed from JSONL)
// ──────────────────────────────────────────────────────────────────────────────

/// A single JSONL entry from a Claude Code transcript.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
enum TranscriptEntry {
    /// User message (task description, context).
    #[serde(rename = "user")]
    User { message: UserMessage },
    /// Assistant response (text, tool calls).
    #[serde(rename = "assistant")]
    Assistant { message: AssistantMessage },
    /// Tool result (outcome of a tool_use).
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: Option<bool>,
    },
    /// Attachment (deferred tools, companion intro).
    #[serde(rename = "attachment")]
    Attachment { attachment: Attachment },
    /// System event (init, status, hooks).
    #[serde(rename = "system")]
    System { subtype: String, session_id: String },
    /// Stream event (content blocks, deltas).
    #[serde(rename = "stream_event")]
    StreamEvent {
        event: StreamEvent,
        session_id: String,
    },
    /// Queue operation (enqueue/dequeue).
    #[serde(rename = "queue-operation")]
    QueueOperation {
        operation: String,
        timestamp: String,
    },
}

/// User message content.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct UserMessage {
    content: String,
}

/// Assistant message content.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct AssistantMessage {
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
}

/// Content block (text, thinking, tool use).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

/// Attachment metadata.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct Attachment {
    #[serde(rename = "type")]
    attachment_type: String,
}

/// Stream event (content block updates).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
#[allow(clippy::enum_variant_names)]
enum StreamEvent {
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: usize, delta: ContentDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
}

/// Content delta (incremental update).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
enum ContentDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
}

// ──────────────────────────────────────────────────────────────────────────────
// Parsed transcript summary
// ──────────────────────────────────────────────────────────────────────────────

/// A parsed and summarized transcript.
#[derive(Debug, Clone)]
pub struct ParsedTranscript {
    /// Session ID (filename without .jsonl).
    pub session_id: String,
    /// File modification time (when session was last updated).
    pub modified_at: DateTime<Utc>,
    /// User task description (from first user message).
    pub task_description: Option<String>,
    /// Assistant actions extracted from the transcript.
    pub actions: Vec<TranscriptAction>,
    /// Action-outcome pairs (tool_use → tool_result sequences).
    pub action_outcomes: Vec<ActionOutcome>,
    /// Bead ID if referenced in the task context.
    pub bead_id: Option<BeadId>,
    /// Workspace path this transcript belongs to (set during multi-workspace discovery).
    pub workspace: Option<PathBuf>,
}

/// An action extracted from a transcript (tool call or text output).
#[derive(Debug, Clone)]
pub struct TranscriptAction {
    /// Action type (tool call or text).
    pub action_type: ActionType,
    /// Tool name (if tool use).
    pub tool_name: Option<String>,
    /// Action description (text output or tool input summary).
    pub description: String,
}

/// An action-outcome pair extracted from a transcript.
///
/// Represents a tool_use action and its corresponding tool_result,
/// capturing the full cause-effect relationship.
#[derive(Debug, Clone)]
pub struct ActionOutcome {
    /// Tool name (e.g., "Read", "Bash", "Grep").
    pub tool_name: String,
    /// Tool input (file path, command, pattern, etc.).
    pub input: String,
    /// Tool result output (success content, error message, etc.).
    pub output: String,
    /// Whether the result indicates an error.
    pub is_error: bool,
    /// Action description (human-readable summary).
    pub action_description: String,
    /// Outcome description (human-readable summary).
    pub outcome_description: String,
}

impl ActionOutcome {
    /// Create a new action-outcome pair.
    pub fn new(tool_name: String, input: String, output: String, is_error: bool) -> Self {
        let action_description = Self::format_action(&tool_name, &input);
        let outcome_description = Self::format_outcome(&tool_name, &output, is_error);

        ActionOutcome {
            tool_name,
            input,
            output,
            is_error,
            action_description,
            outcome_description,
        }
    }

    /// Format the action as a human-readable description.
    fn format_action(tool_name: &str, input: &str) -> String {
        match tool_name {
            "Read" | "Edit" | "Write" => {
                format!("{}: {}", tool_name, truncate_path(input))
            }
            "Bash" => format!("bash: {}", truncate_cmd(input)),
            "Grep" => format!("grep: {}", truncate_pattern(input)),
            _ => format!("{}: {}", tool_name, truncate_text(input, 100)),
        }
    }

    /// Format the outcome as a human-readable description.
    fn format_outcome(tool_name: &str, output: &str, is_error: bool) -> String {
        if is_error {
            return format!("Error: {}", truncate_text(output, 200));
        }

        match tool_name {
            "Read" => format!("File read successfully ({} bytes)", output.len()),
            "Write" | "Edit" => "File written successfully".to_string(),
            "Bash" => format!("Exit: {}", truncate_text(output.trim(), 100)),
            "Grep" => format!("Found {} matches", output.lines().count()),
            _ => truncate_text(output, 200),
        }
    }

    /// Convert to a TranscriptPattern for learning extraction.
    pub fn to_pattern(
        &self,
        session_id: &str,
        timestamp: DateTime<Utc>,
    ) -> crate::learning::TranscriptPattern {
        crate::learning::TranscriptPattern::new(
            self.tool_name.clone(),
            self.input.clone(),
            self.outcome_description.clone(),
            session_id.to_string(),
            timestamp,
        )
    }
}

/// Truncate a path to at most N characters, showing end if truncated.
fn truncate_path(path: &str) -> String {
    if path.len() <= 80 {
        path.to_string()
    } else {
        format!("...{}", &path[path.len() - 77..])
    }
}

/// Truncate a command to at most N characters.
fn truncate_cmd(cmd: &str) -> String {
    truncate_text(cmd, 100)
}

/// Truncate a grep pattern to at most N characters.
fn truncate_pattern(pattern: &str) -> String {
    truncate_text(pattern, 60)
}

/// Truncate text to at most N characters with ellipsis.
fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        text.to_string()
    } else {
        format!("{}...", &text[..max_chars - 3])
    }
}

/// Type of action performed by the assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionType {
    /// Text output to user.
    Text,
    /// Tool use (Read, Write, Bash, etc.).
    ToolUse,
    /// Thinking/reasoning block.
    Thinking,
}

// ──────────────────────────────────────────────────────────────────────────────
// Transcript discovery
// ──────────────────────────────────────────────────────────────────────────────

/// Discovers and parses Claude Code session transcripts for a workspace.
pub struct TranscriptDiscovery {
    /// Path to `.claude/projects/` directory.
    claude_projects_dir: PathBuf,
    /// Workspace path (used to derive project name).
    workspace: PathBuf,
    /// Maximum number of recent sessions to return.
    max_sessions: usize,
    /// Recency cutoff: only sessions modified after this time.
    recency_cutoff: Option<DateTime<Utc>>,
}

impl TranscriptDiscovery {
    /// Create a new transcript discovery for a workspace.
    ///
    /// `workspace` is the path to the workspace root (e.g., `/home/coding/NEEDLE`).
    /// `claude_dir` is the path to the `.claude` directory (defaults to `~/.claude`).
    /// `max_sessions` limits the number of sessions returned (sorted by recency).
    pub fn new(workspace: &Path, claude_dir: Option<&Path>, max_sessions: usize) -> Self {
        let home_claude = PathBuf::from(std::env::var("HOME").unwrap()).join(".claude");
        let claude_root = claude_dir.unwrap_or(&home_claude);
        let claude_projects_dir = claude_root.join("projects");

        TranscriptDiscovery {
            claude_projects_dir,
            workspace: workspace.to_path_buf(),
            max_sessions,
            recency_cutoff: None,
        }
    }

    /// Set a recency cutoff (only sessions modified after this time).
    pub fn with_recency_cutoff(mut self, cutoff: DateTime<Utc>) -> Self {
        self.recency_cutoff = Some(cutoff);
        self
    }

    /// Discover all recent session transcripts for this workspace.
    ///
    /// Returns transcripts sorted by modification time (most recent first).
    pub fn discover(&self) -> Result<Vec<ParsedTranscript>> {
        let project_name = self.derive_project_name();
        let project_dir = self.claude_projects_dir.join(&project_name);

        if !project_dir.exists() {
            tracing::debug!(
                project_dir = %project_dir.display(),
                "transcript discovery: project directory not found"
            );
            return Ok(Vec::new());
        }

        let mut transcripts = Vec::new();

        // Iterate through JSONL files in the project directory
        for entry in std::fs::read_dir(&project_dir).with_context(|| {
            format!(
                "failed to read project directory: {}",
                project_dir.display()
            )
        })? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();

            // Only process .jsonl files
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }

            // Get file metadata
            let metadata = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            // Get modification time
            let modified_at: DateTime<Utc> = metadata
                .modified()
                .ok()
                .map(DateTime::<Utc>::from)
                .unwrap_or_else(Utc::now);

            // Check recency cutoff
            if let Some(cutoff) = self.recency_cutoff {
                if modified_at < cutoff {
                    continue;
                }
            }

            // Parse the transcript
            match self.parse_transcript(&path, modified_at) {
                Ok(transcript) => transcripts.push(transcript),
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to parse transcript"
                    );
                }
            }
        }

        // Sort by modification time (most recent first)
        transcripts.sort_by_key(|b| std::cmp::Reverse(b.modified_at));

        // Limit to max_sessions
        transcripts.truncate(self.max_sessions);

        tracing::debug!(
            project_name,
            count = transcripts.len(),
            "transcript discovery: found transcripts"
        );

        Ok(transcripts)
    }

    /// Derive the Claude project name from the workspace path.
    ///
    /// `/home/coding/NEEDLE` → `-home-coding-NEEDLE`
    fn derive_project_name(&self) -> String {
        self.workspace
            .to_string_lossy()
            .replace(['/', '\\'], "-")
            .trim_start_matches('-')
            .to_string()
    }

    /// Parse a single transcript file.
    fn parse_transcript(
        &self,
        path: &Path,
        modified_at: DateTime<Utc>,
    ) -> Result<ParsedTranscript> {
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read transcript: {}", path.display()))?;

        let mut task_description = None;
        let mut actions = Vec::new();
        let mut action_outcomes = Vec::new();
        let mut bead_id = None;

        // Track pending tool_use actions for pairing with tool_result
        let mut pending_tool_uses: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();

        // Parse each JSONL line
        for (line_no, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match serde_json::from_str::<TranscriptEntry>(line) {
                Ok(entry) => {
                    // Extract task description from first user message
                    if task_description.is_none() {
                        if let TranscriptEntry::User { ref message } = entry {
                            task_description =
                                Some(self.extract_task_bead_id(&message.content, &mut bead_id));
                        }
                    }

                    // Extract actions from assistant messages
                    if let TranscriptEntry::Assistant { ref message } = entry {
                        for block in &message.content {
                            match block {
                                ContentBlock::Text { text } => {
                                    if !text.trim().is_empty() {
                                        actions.push(TranscriptAction {
                                            action_type: ActionType::Text,
                                            tool_name: None,
                                            description: text.chars().take(200).collect(),
                                        });
                                    }
                                }
                                ContentBlock::ToolUse { id, name, input } => {
                                    actions.push(TranscriptAction {
                                        action_type: ActionType::ToolUse,
                                        tool_name: Some(name.clone()),
                                        description: self.format_tool_input(name, input),
                                    });

                                    // Track pending tool_use for pairing with tool_result
                                    let input_str = serde_json::to_string(input)
                                        .unwrap_or_else(|_| input.to_string());
                                    pending_tool_uses.insert(id.clone(), (name.clone(), input_str));
                                }
                                ContentBlock::Thinking { thinking } => {
                                    if !thinking.trim().is_empty() {
                                        actions.push(TranscriptAction {
                                            action_type: ActionType::Thinking,
                                            tool_name: None,
                                            description: thinking.chars().take(200).collect(),
                                        });
                                    }
                                }
                            }
                        }
                    }

                    // Extract tool_result and pair with pending tool_use
                    if let TranscriptEntry::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } = entry
                    {
                        if let Some((tool_name, input)) = pending_tool_uses.remove(&tool_use_id) {
                            let outcome = ActionOutcome::new(
                                tool_name,
                                input,
                                content,
                                is_error.unwrap_or(false),
                            );
                            action_outcomes.push(outcome);
                        } else {
                            tracing::trace!(
                                tool_use_id = %tool_use_id,
                                "tool_result without matching tool_use"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::trace!(
                        path = %path.display(),
                        line = line_no + 1,
                        error = %e,
                        "failed to parse JSONL line"
                    );
                }
            }
        }

        Ok(ParsedTranscript {
            session_id,
            modified_at,
            task_description,
            actions,
            action_outcomes,
            bead_id,
            workspace: Some(self.workspace.clone()),
        })
    }

    /// Extract task description and parse bead ID from user message.
    fn extract_task_bead_id(&self, content: &str, bead_id: &mut Option<BeadId>) -> String {
        // Extract bead ID if present (format: "Bead ID: needle-xxx")
        if let Some(idx) = content.find("Bead ID:") {
            let after = &content[idx + "Bead ID:".len()..];
            let id = after
                .split_whitespace()
                .next()
                .filter(|s| s.starts_with("needle-"))
                .map(|s| s.trim().to_string());
            *bead_id = id.map(BeadId::from);
        }

        // Return first 500 chars as task description
        content.chars().take(500).collect()
    }

    /// Format tool input as a short description.
    fn format_tool_input(&self, name: &str, input: &serde_json::Value) -> String {
        match name {
            "Read" | "Edit" | "Write" => {
                if let Some(path) = input.get("file_path").and_then(|v| v.as_str()) {
                    format!("{}: {}", name, path)
                } else if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                    format!("{}: {}", name, path)
                } else {
                    name.to_string()
                }
            }
            "Bash" => input
                .get("command")
                .and_then(|v| v.as_str())
                .map(|cmd| format!("bash: {}", cmd.chars().take(100).collect::<String>()))
                .unwrap_or_else(|| name.to_string()),
            "Grep" => input
                .get("pattern")
                .and_then(|v| v.as_str())
                .map(|p| format!("grep: {}", p))
                .unwrap_or_else(|| name.to_string()),
            _ => name.to_string(),
        }
    }
}

/// Discover recent transcripts across multiple workspaces.
///
/// Combines results from all workspaces, sorted by modification time (most recent
/// first), and truncated to `max_total`. Each returned transcript has its
/// `workspace` field set to the source workspace path.
///
/// Workspaces that have no transcript directory are silently skipped.
pub fn discover_workspaces(
    workspaces: &[&Path],
    claude_dir: Option<&Path>,
    max_per_workspace: usize,
    max_total: usize,
    recency_cutoff: Option<DateTime<Utc>>,
) -> Result<Vec<ParsedTranscript>> {
    let mut all = Vec::new();

    for &workspace in workspaces {
        let mut d = TranscriptDiscovery::new(workspace, claude_dir, max_per_workspace);
        if let Some(cutoff) = recency_cutoff {
            d = d.with_recency_cutoff(cutoff);
        }

        match d.discover() {
            Ok(transcripts) => all.extend(transcripts),
            Err(e) => {
                tracing::warn!(
                    workspace = %workspace.display(),
                    error = %e,
                    "transcript discovery: skipping workspace due to error"
                );
            }
        }
    }

    // Sort by modification time (most recent first) and cap
    all.sort_by_key(|t| std::cmp::Reverse(t.modified_at));
    all.truncate(max_total);

    Ok(all)
}

// ──────────────────────────────────────────────────────────────────────────────
// Decision Detection
// ──────────────────────────────────────────────────────────────────────────────

/// Decision patterns that indicate an agent chose between alternatives.
///
/// These patterns are searched for in thinking blocks and assistant text
/// to identify decision points worth preserving as ADR-style learnings.
const DECISION_PATTERNS: &[&str] = &[
    "chose",
    "decided to",
    "opted for",
    "selected",
    "picked",
    "went with",
    "choosing",
    "decision",
    "instead of",
    "rather than",
    "over",
    "preference",
    "prefer",
];

/// Rationale indicator patterns that follow a decision.
///
/// These patterns help extract the "why" behind a decision.
const RATIONALE_PATTERNS: &[&str] = &[
    "because",
    "since",
    "as",
    "due to",
    "given that",
    "considering",
    "reason",
    "rationale",
    "why",
    "motivation",
    "justification",
];

/// A detected decision with its context and rationale.
#[derive(Debug, Clone)]
pub struct DetectedDecision {
    /// The decision that was made.
    pub decision: String,
    /// Context/problem space.
    pub context: String,
    /// Rationale (why this choice).
    pub rationale: String,
    /// Alternatives mentioned (if any).
    pub alternatives: Vec<String>,
    /// Confidence that this is a genuine decision (0.0-1.0).
    pub confidence: f32,
}

/// Detect decisions in a parsed transcript.
///
/// Analyzes thinking blocks and assistant text for decision patterns,
/// extracting the decision, context, and rationale.
pub fn detect_decisions(transcript: &ParsedTranscript) -> Vec<DetectedDecision> {
    let mut decisions = Vec::new();

    // Group consecutive thinking blocks for better context
    let mut thinking_blocks = Vec::new();
    for action in &transcript.actions {
        if action.action_type == ActionType::Thinking {
            thinking_blocks.push(action.description.clone());
        }
    }

    // Analyze each thinking block for decision patterns
    for thinking in &thinking_blocks {
        if let Some(decision) = analyze_thinking_for_decision(thinking) {
            decisions.push(decision);
        }
    }

    // Also analyze assistant text for decisions (lower confidence)
    for action in &transcript.actions {
        if action.action_type == ActionType::Text {
            if let Some(decision) = analyze_text_for_decision(&action.description) {
                decisions.push(decision);
            }
        }
    }

    decisions
}

/// Analyze a thinking block for decision patterns.
fn analyze_thinking_for_decision(thinking: &str) -> Option<DetectedDecision> {
    let thinking_lower = thinking.to_lowercase();

    // Check if any decision pattern is present
    let decision_match = DECISION_PATTERNS
        .iter()
        .find(|p| thinking_lower.contains(*p));

    let _ = decision_match?;

    // Extract the decision sentence(s)
    let decision = extract_decision_sentence(thinking)?;

    // Extract context (sentence(s) before the decision)
    let context = extract_context_before(thinking, &decision)?;

    // Extract rationale (look for "because", "since", etc. after decision)
    let rationale = extract_rationale_after(thinking, &decision).unwrap_or_default();

    // Extract alternatives (look for "instead of X", "over Y", etc.)
    let alternatives = extract_alternatives(thinking);

    // Calculate confidence based on signal strength
    let confidence = calculate_decision_confidence(&decision, &rationale, &alternatives);

    if confidence < 0.3 {
        return None;
    }

    Some(DetectedDecision {
        decision,
        context,
        rationale,
        alternatives,
        confidence,
    })
}

/// Analyze assistant text for decision patterns (lower confidence).
fn analyze_text_for_decision(text: &str) -> Option<DetectedDecision> {
    let text_lower = text.to_lowercase();

    // Check if any decision pattern is present
    let decision_match = DECISION_PATTERNS.iter().find(|p| text_lower.contains(*p));

    let _ = decision_match?;

    // Extract the decision (first sentence with decision pattern)
    let decision = extract_decision_sentence(text)?;

    // For text, context is limited (use preceding sentence if available)
    let context = extract_context_before(text, &decision).unwrap_or_default();

    // Extract rationale
    let rationale = extract_rationale_after(text, &decision).unwrap_or_default();

    // Extract alternatives
    let alternatives = extract_alternatives(text);

    // Lower confidence for text vs thinking blocks
    let confidence = calculate_decision_confidence(&decision, &rationale, &alternatives) * 0.7;

    if confidence < 0.3 {
        return None;
    }

    Some(DetectedDecision {
        decision,
        context,
        rationale,
        alternatives,
        confidence,
    })
}

/// Extract the sentence containing the decision.
fn extract_decision_sentence(text: &str) -> Option<String> {
    // Split into sentences (naive: split on periods)
    let sentences: Vec<&str> = text.split('.').collect();

    for sentence in sentences {
        let s = sentence.trim();
        let s_lower = s.to_lowercase();
        if DECISION_PATTERNS.iter().any(|p| s_lower.contains(p)) {
            return Some(s.to_string());
        }
    }

    None
}

/// Extract context (text before the decision sentence).
fn extract_context_before(text: &str, decision: &str) -> Option<String> {
    let idx = text.find(decision)?;
    if idx == 0 {
        return None;
    }

    let before = &text[..idx];
    // Take last 2 sentences as context (200 chars max)
    let context: String = before
        .rsplit('.')
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(".")
        .chars()
        .rev()
        .take(200)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();

    if context.trim().is_empty() {
        None
    } else {
        Some(context.trim().to_string())
    }
}

/// Extract rationale (text after decision with rationale indicators).
fn extract_rationale_after(text: &str, decision: &str) -> Option<String> {
    let decision_end = text.find(decision)? + decision.len();
    let after = text.get(decision_end..)?;

    // Look for rationale pattern in next 300 chars
    let search_window: String = after.chars().take(300).collect();
    let search_lower = search_window.to_lowercase();

    for pattern in RATIONALE_PATTERNS {
        if let Some(idx) = search_lower.find(pattern) {
            // Extract from pattern to next period or end
            let rationale_start = idx + pattern.len();
            let rationale_end = search_window[rationale_start..]
                .find('.')
                .unwrap_or(search_window[rationale_start..].len());

            let rationale = search_window[rationale_start..rationale_start + rationale_end]
                .trim()
                .to_string();
            if !rationale.is_empty() {
                return Some(rationale);
            }
        }
    }

    None
}

/// Extract alternatives mentioned in the text.
fn extract_alternatives(text: &str) -> Vec<String> {
    let mut alternatives = Vec::new();
    let text_lower = text.to_lowercase();

    // Look for "instead of X", "over Y", "not Z"
    let patterns = [
        ("instead of", "instead of"),
        ("over ", "over"),
        ("not ", "not"),
    ];

    for (marker, _name) in patterns {
        let mut search_start = 0;
        while let Some(idx) = text_lower[search_start..].find(marker) {
            let absolute_idx = search_start + idx;
            let after_marker = absolute_idx + marker.len();

            // Extract next few words as alternative (up to 50 chars)
            let alt_text: String = text
                .get(after_marker..)
                .map(|s| s.chars().take(50).collect::<String>())
                .unwrap_or_default();

            // Extract first phrase (up to comma, period, or 5 words)
            let alt: String = alt_text
                .split([',', '.'])
                .next()
                .unwrap_or("")
                .split_whitespace()
                .take(5)
                .collect::<Vec<_>>()
                .join(" ");

            if !alt.is_empty() && alt.len() > 2 {
                alternatives.push(alt);
            }

            search_start = after_marker;
        }
    }

    // Dedupe and limit
    alternatives.sort();
    alternatives.dedup();
    alternatives.truncate(3);
    alternatives
}

/// Calculate confidence score for a detected decision.
fn calculate_decision_confidence(decision: &str, rationale: &str, alternatives: &[String]) -> f32 {
    let mut confidence: f32 = 0.3; // Base confidence

    // Strong decision phrases
    if decision.to_lowercase().contains("decided to")
        || decision.to_lowercase().contains("chose")
        || decision.to_lowercase().contains("opted for")
    {
        confidence += 0.3;
    }

    // Has rationale
    if !rationale.is_empty() {
        confidence += 0.2;
    }

    // Has alternatives (shows comparison was made)
    if !alternatives.is_empty() {
        confidence += 0.1;
    }

    // Decision is specific (mentions concrete action)
    if decision.len() > 15 && decision.len() < 200 {
        confidence += 0.1;
    }

    confidence.min(1.0_f32)
}

/// Convert a detected decision to a DecisionContext for learning entries.
impl From<DetectedDecision> for DecisionContext {
    fn from(detected: DetectedDecision) -> Self {
        DecisionContext {
            decision: detected.decision,
            context: detected.context,
            rationale: detected.rationale,
            alternatives: detected.alternatives,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn derive_project_name_unix() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().join("home").join("coding").join("NEEDLE");
        let discovery = TranscriptDiscovery::new(&workspace, None, 10);
        // The full path is converted, including the temp directory prefix
        let result = discovery.derive_project_name();
        // Verify it ends with the expected suffix (temp dir prefix varies)
        assert!(result.ends_with("home-coding-NEEDLE"));
        // Verify it doesn't start with a dash (trim_start_matches worked)
        assert!(!result.starts_with('-'));
    }

    #[test]
    fn derive_project_name_absolute() {
        let workspace = PathBuf::from("/home/coding/NEEDLE");
        let discovery = TranscriptDiscovery::new(&workspace, None, 10);
        assert_eq!(discovery.derive_project_name(), "home-coding-NEEDLE");
    }

    #[test]
    fn discovery_returns_empty_when_no_project_dir() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().join("nonexistent");
        let claude_dir = temp_dir.path().join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();

        let discovery = TranscriptDiscovery::new(&workspace, Some(&claude_dir), 10);
        let result = discovery.discover().unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn discovery_parses_valid_transcript() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().join("home").join("coding").join("NEEDLE");
        let claude_dir = temp_dir.path().join(".claude");

        // Create project directory
        let project_name = workspace
            .to_string_lossy()
            .replace('/', "-")
            .trim_start_matches('-')
            .to_string();
        let project_dir = claude_dir.join("projects").join(&project_name);
        fs::create_dir_all(&project_dir).unwrap();

        // Create a test transcript with tool_use and tool_result
        let transcript_path = project_dir.join("test-session.jsonl");
        let mut file = fs::File::create(&transcript_path).unwrap();

        writeln!(file, "{{\"type\":\"user\",\"message\":{{\"content\":\"## Task\\n\\nFix the bug\\n\\nBead ID: needle-test123\"}}}}").unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"I'll fix it"}}], "stop_reason":"end_turn"}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"1","name":"Read","input":{{"file_path":"/tmp/test.txt"}}}}]}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"tool_result","tool_use_id":"1","content":"file content here","is_error":false}}"#).unwrap();

        let discovery = TranscriptDiscovery::new(&workspace, Some(&claude_dir), 10);
        let results = discovery.discover().unwrap();

        assert_eq!(results.len(), 1);
        let transcript = &results[0];
        assert_eq!(transcript.session_id, "test-session");
        assert!(transcript.task_description.is_some());
        assert_eq!(transcript.bead_id.as_deref(), Some("needle-test123"));
        assert_eq!(transcript.actions.len(), 2);
        assert_eq!(transcript.actions[0].action_type, ActionType::Text);
        assert_eq!(transcript.actions[1].action_type, ActionType::ToolUse);
        assert_eq!(transcript.actions[1].tool_name.as_deref(), Some("Read"));
        // Check action-outcome pairs
        assert_eq!(transcript.action_outcomes.len(), 1);
        assert_eq!(transcript.action_outcomes[0].tool_name, "Read");
        assert!(!transcript.action_outcomes[0].is_error);
        assert_eq!(transcript.action_outcomes[0].output, "file content here");
    }

    #[test]
    fn discovery_respects_max_sessions() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().join("home").join("coding").join("NEEDLE");
        let claude_dir = temp_dir.path().join(".claude");

        // Create project directory
        let project_name = workspace
            .to_string_lossy()
            .replace('/', "-")
            .trim_start_matches('-')
            .to_string();
        let project_dir = claude_dir.join("projects").join(&project_name);
        fs::create_dir_all(&project_dir).unwrap();

        // Create 5 transcripts
        for i in 1..=5 {
            let path = project_dir.join(format!("session-{}.jsonl", i));
            fs::write(&path, r#"{"type":"user","message":{"content":"test"}}"#).unwrap();
            // Add small delay to ensure different modification times
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let discovery = TranscriptDiscovery::new(&workspace, Some(&claude_dir), 3);
        let results = discovery.discover().unwrap();

        assert_eq!(results.len(), 3);
    }

    #[test]
    fn discovery_respects_recency_cutoff() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().join("home").join("coding").join("NEEDLE");
        let claude_dir = temp_dir.path().join(".claude");

        // Create project directory
        let project_name = workspace
            .to_string_lossy()
            .replace('/', "-")
            .trim_start_matches('-')
            .to_string();
        let project_dir = claude_dir.join("projects").join(&project_name);
        fs::create_dir_all(&project_dir).unwrap();

        // Create a transcript
        let path = project_dir.join("old-session.jsonl");
        fs::write(&path, r#"{"type":"user","message":{"content":"old"}}"#).unwrap();

        // Set recency cutoff to now + 1 hour (future)
        let future_cutoff = Utc::now() + chrono::Duration::hours(1);
        let discovery = TranscriptDiscovery::new(&workspace, Some(&claude_dir), 10)
            .with_recency_cutoff(future_cutoff);
        let results = discovery.discover().unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn format_tool_input_read() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().join("test");
        let discovery = TranscriptDiscovery::new(&workspace, None, 10);

        let input = serde_json::json!({"file_path": "/tmp/test.txt"});
        let result = discovery.format_tool_input("Read", &input);
        assert_eq!(result, "Read: /tmp/test.txt");
    }

    #[test]
    fn format_tool_input_bash() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().join("test");
        let discovery = TranscriptDiscovery::new(&workspace, None, 10);

        let input = serde_json::json!({"command": "cargo test"});
        let result = discovery.format_tool_input("Bash", &input);
        assert_eq!(result, "bash: cargo test");
    }

    #[test]
    fn extract_task_bead_id() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().join("test");
        let discovery = TranscriptDiscovery::new(&workspace, None, 10);

        let mut bead_id = None;
        let content = "## Task\n\nFix the bug\n\nBead ID: needle-abc123";
        let desc = discovery.extract_task_bead_id(content, &mut bead_id);

        assert!(desc.contains("Fix the bug"));
        assert_eq!(bead_id.as_deref(), Some("needle-abc123"));
    }

    #[test]
    fn extract_task_without_bead_id() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().join("test");
        let discovery = TranscriptDiscovery::new(&workspace, None, 10);

        let mut bead_id = None;
        let content = "## Task\n\nJust do something";
        let desc = discovery.extract_task_bead_id(content, &mut bead_id);

        assert!(desc.contains("Just do something"));
        assert!(bead_id.is_none());
    }

    #[test]
    fn parsed_transcript_workspace_field_is_set() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().join("home").join("coding").join("NEEDLE");
        let claude_dir = temp_dir.path().join(".claude");

        let project_name = workspace
            .to_string_lossy()
            .replace('/', "-")
            .trim_start_matches('-')
            .to_string();
        let project_dir = claude_dir.join("projects").join(&project_name);
        fs::create_dir_all(&project_dir).unwrap();

        fs::write(
            project_dir.join("ws-session.jsonl"),
            r#"{"type":"user","message":{"content":"test"}}"#,
        )
        .unwrap();

        let discovery = TranscriptDiscovery::new(&workspace, Some(&claude_dir), 10);
        let results = discovery.discover().unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].workspace.as_deref(), Some(workspace.as_path()));
    }

    #[test]
    fn discover_workspaces_combines_multiple() {
        let temp_dir = TempDir::new().unwrap();
        let claude_dir = temp_dir.path().join(".claude");

        // Create two workspaces, each with one transcript
        let ws_a = temp_dir.path().join("home").join("coding").join("project-a");
        let ws_b = temp_dir.path().join("home").join("coding").join("project-b");

        for ws in [&ws_a, &ws_b] {
            let project_name = ws
                .to_string_lossy()
                .replace('/', "-")
                .trim_start_matches('-')
                .to_string();
            let project_dir = claude_dir.join("projects").join(&project_name);
            fs::create_dir_all(&project_dir).unwrap();
            fs::write(
                project_dir.join("session.jsonl"),
                r#"{"type":"user","message":{"content":"task"}}"#,
            )
            .unwrap();
        }

        let results = super::discover_workspaces(
            &[ws_a.as_path(), ws_b.as_path()],
            Some(&claude_dir),
            10,
            100,
            None,
        )
        .unwrap();

        assert_eq!(results.len(), 2);
        // Both workspaces should be represented
        let found_a = results.iter().any(|t| t.workspace.as_deref() == Some(ws_a.as_path()));
        let found_b = results.iter().any(|t| t.workspace.as_deref() == Some(ws_b.as_path()));
        assert!(found_a, "workspace A not found in results");
        assert!(found_b, "workspace B not found in results");
    }

    #[test]
    fn discover_workspaces_respects_max_total() {
        let temp_dir = TempDir::new().unwrap();
        let claude_dir = temp_dir.path().join(".claude");

        // Create two workspaces, each with 3 transcripts
        for proj in ["proj-x", "proj-y"] {
            let ws = temp_dir.path().join("home").join(proj);
            let project_name = ws
                .to_string_lossy()
                .replace('/', "-")
                .trim_start_matches('-')
                .to_string();
            let project_dir = claude_dir.join("projects").join(&project_name);
            fs::create_dir_all(&project_dir).unwrap();
            for i in 1..=3 {
                fs::write(
                    project_dir.join(format!("session-{}.jsonl", i)),
                    r#"{"type":"user","message":{"content":"t"}}"#,
                )
                .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        let ws_x = temp_dir.path().join("home").join("proj-x");
        let ws_y = temp_dir.path().join("home").join("proj-y");

        let results = super::discover_workspaces(
            &[ws_x.as_path(), ws_y.as_path()],
            Some(&claude_dir),
            10,
            4,
            None,
        )
        .unwrap();

        assert_eq!(results.len(), 4);
    }

    #[test]
    fn discover_workspaces_skips_missing_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let claude_dir = temp_dir.path().join(".claude");

        // One valid workspace
        let ws_good = temp_dir.path().join("home").join("coding").join("good");
        let project_name = ws_good
            .to_string_lossy()
            .replace('/', "-")
            .trim_start_matches('-')
            .to_string();
        let project_dir = claude_dir.join("projects").join(&project_name);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("session.jsonl"),
            r#"{"type":"user","message":{"content":"x"}}"#,
        )
        .unwrap();

        // One workspace with no transcript directory
        let ws_missing = temp_dir.path().join("home").join("coding").join("missing");

        let results = super::discover_workspaces(
            &[ws_good.as_path(), ws_missing.as_path()],
            Some(&claude_dir),
            10,
            100,
            None,
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].workspace.as_deref(), Some(ws_good.as_path()));
    }
}
