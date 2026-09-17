//! Per-bead attempt history: what the next attempt is told about the last ones.
//!
//! Before this module a failed attempt taught its successor nothing. The gate
//! output was computed, fingerprinted and written to telemetry, gate-health
//! state and the trace directory — and the next agent received a
//! byte-identical prompt. Only labels (`failure-count:N`, `verification-failed`,
//! `quarantine-until:`) survived a failure, and none of them says *why*.
//!
//! This is plan revision 31 leaf R3 (`needle-60163eac`): the admitted recovery
//! context is injected into the next attempt. It is deliberately the smallest
//! shape that closes the loop — a bounded, append-only record per attempt and
//! a rendering of the newest few into the dispatch prompt — and it does not
//! wait for the checkpoint journal or the learning inbox (C1–C5, D1–D6).
//!
//! Two stores, one truth:
//!
//! - **Local journal** `<workspace>/.beads/traces/<bead-id>/attempts.jsonl`,
//!   next to the attempt's own trace. Mend prunes `trace.jsonl`/`stdout.txt`
//!   from that directory but never the directory itself, so the journal
//!   outlives the transcript. It is gitignored with the rest of `traces/`.
//! - **Cross-host mirror** in bead-rs structured data, namespace
//!   [`DATA_NAMESPACE`], so a worker on another host — or a fresh clone —
//!   sees the same history. It survives `bead update --notes`, which
//!   *replaces* notes and is what the prompt tells agents to run.
//!
//! The record carries what the ledger row carries plus bounded, sanitized
//! failure context. It never carries a transcript, model reasoning, or a
//! credential: the legacy summary and the opt-in structured evidence are
//! derived from gate/output data, sanitized, and cut to fixed sizes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::bead_store::BeadStore;
use crate::sanitize::Sanitizer;
use crate::types::BeadId;
use crate::validation::{GateReport, GateResult};

/// Schema version stamped on every record.
pub const SCHEMA_VERSION: u32 = 1;
/// bead-rs structured-data namespace holding the cross-host mirror.
pub const DATA_NAMESPACE: &str = "needle-attempts";
/// Immutable schema reference declared with the structured data.
pub const DATA_SCHEMA_REF: &str = "urn:needle:schema:attempt-history:v1";
/// bead-rs structured-data namespace holding candidate lessons.
pub const LESSONS_DATA_NAMESPACE: &str = crate::learning::CANDIDATE_LESSONS_DATA_NAMESPACE;
/// Immutable schema reference declared with candidate-lesson data.
pub const LESSONS_DATA_SCHEMA_REF: &str = crate::learning::CANDIDATE_LESSONS_DATA_SCHEMA_REF;
/// Records kept in the local journal before the oldest are dropped.
const LOCAL_KEEP: usize = 50;
/// Records mirrored into bead-rs structured data.
const MIRROR_KEEP: usize = 5;
/// Bytes of failure summary kept per record in the mirror.
const MIRROR_SUMMARY_BYTES: usize = 600;
/// Bytes of failure summary kept per record in the local journal.
pub const LOCAL_SUMMARY_BYTES: usize = 1200;
/// Semantic outcome of an attempt whose evidence was accepted.
const VERIFIED_SUCCESS: &str = "verified_success";

/// Maximum serialized size of the failure evidence attached to a local record.
///
/// This is deliberately smaller than the history prompt cap because a record
/// also carries the legacy summary and attempt metadata. The bounded object is
/// safe to mirror without allowing a single failed attempt to consume the
/// whole bead-rs data value.
pub const MAX_FAILURE_EVIDENCE_BYTES: usize = 2400;
/// Maximum number of tool failures retained, newest failures last in storage
/// order. Rendering reverses the containing attempts, not this list.
pub const MAX_TOOL_ERRORS: usize = 3;
/// Maximum bytes retained for the final assistant message before the overall
/// evidence cap is applied.
const FINAL_MESSAGE_BYTES: usize = 600;
/// Maximum bytes retained for a normalized tool signature.
const TOOL_SIGNATURE_BYTES: usize = 320;
/// Maximum bytes retained for a tool error excerpt.
const TOOL_EXCERPT_BYTES: usize = 400;
/// Maximum bytes retained for one gate's first error block.
const GATE_ERROR_BLOCK_BYTES: usize = 650;
/// Maximum bytes retained for names in the evidence object.
const EVIDENCE_NAME_BYTES: usize = 64;
/// Marker used when sanitization cannot safely return the source content.
pub const SANITIZER_BLOCKED_MARKER: &str = "[failure evidence blocked by sanitizer]";
/// Marker used when a failed attempt had no structured transcript content.
pub const EVIDENCE_UNAVAILABLE_MARKER: &str = "[no structured failure evidence captured]";

/// One failed tool invocation retained as attempt evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolErrorEvidence {
    /// Adapter-normalized tool name (for example, `Bash` or `shell`).
    pub tool_name: String,
    /// Stable signature of the sanitized error output.
    pub signature: String,
    /// Short sanitized excerpt useful to the next attempt.
    pub excerpt: String,
}

/// One failing validation gate's first useful error block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateDiagnostic {
    /// Name of the gate that rejected the attempt.
    pub gate_name: String,
    /// First error-shaped block from the gate output, sanitized and bounded.
    pub error_block: String,
}

/// Bounded, sanitized evidence captured from a failed attempt.
///
/// This is intentionally a small derived record, not a transcript archive.
/// The complete transcript remains owned by the existing trace/archive path;
/// this object contains only the pieces useful for retry context and retrieval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailureEvidence {
    /// The last assistant text message observed in the partial or complete
    /// transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_message: Option<String>,
    /// The newest failed tool results, in transcript order.
    #[serde(default)]
    pub tool_errors: Vec<ToolErrorEvidence>,
    /// Failing gates, ordered by gate name for byte-stable records.
    #[serde(default)]
    pub gate_diagnostics: Vec<GateDiagnostic>,
}

impl FailureEvidence {
    /// Construct an explicit marker record when no structured transcript was
    /// available. A missing field would look indistinguishable from a feature
    /// that was not enabled, so failures retain this fact explicitly.
    pub fn unavailable() -> Self {
        Self {
            final_message: Some(EVIDENCE_UNAVAILABLE_MARKER.to_string()),
            tool_errors: Vec::new(),
            gate_diagnostics: Vec::new(),
        }
    }

    /// Return the serialized byte size of this evidence object.
    pub fn serialized_bytes(&self) -> usize {
        serde_json::to_vec(self)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX)
    }

    /// Bound a record before it is persisted or mirrored.
    pub fn bounded(mut self, max_bytes: usize) -> Self {
        self.final_message = self
            .final_message
            .take()
            .map(|text| truncate_head_tail(&text, FINAL_MESSAGE_BYTES));
        for error in &mut self.tool_errors {
            error.tool_name = truncate_bytes(&error.tool_name, EVIDENCE_NAME_BYTES);
            error.signature = truncate_head_tail(&error.signature, TOOL_SIGNATURE_BYTES);
            error.excerpt = truncate_head_tail(&error.excerpt, TOOL_EXCERPT_BYTES);
        }
        self.tool_errors
            .truncate(self.tool_errors.len().min(MAX_TOOL_ERRORS));
        for diagnostic in &mut self.gate_diagnostics {
            diagnostic.gate_name = truncate_bytes(&diagnostic.gate_name, EVIDENCE_NAME_BYTES);
            diagnostic.error_block =
                truncate_head_tail(&diagnostic.error_block, GATE_ERROR_BLOCK_BYTES);
        }

        // The per-field limits above bound normal records. This final pass
        // also bounds records assembled by callers or old fixtures with
        // unusually many gate diagnostics. Keep a marker in every content
        // field while shrinking so sanitization is never silently represented
        // by an empty string.
        let max_bytes = max_bytes.max(SANITIZER_BLOCKED_MARKER.len() + 128);
        while self.serialized_bytes() > max_bytes {
            let current = self.serialized_bytes();
            let excess = current.saturating_sub(max_bytes).max(1);
            let mut longest = FieldRef::None;
            let mut longest_len = 0;

            if let Some(text) = self.final_message.as_ref() {
                if text.len() > longest_len {
                    longest = FieldRef::Final;
                    longest_len = text.len();
                }
            }
            for (index, error) in self.tool_errors.iter().enumerate() {
                for (kind, len) in [
                    (ToolField::ToolName, error.tool_name.len()),
                    (ToolField::Signature, error.signature.len()),
                    (ToolField::Excerpt, error.excerpt.len()),
                ] {
                    if len > longest_len {
                        longest = FieldRef::Tool(index, kind);
                        longest_len = len;
                    }
                }
            }
            for (index, diagnostic) in self.gate_diagnostics.iter().enumerate() {
                for (kind, len) in [
                    (GateField::GateName, diagnostic.gate_name.len()),
                    (GateField::ErrorBlock, diagnostic.error_block.len()),
                ] {
                    if len > longest_len {
                        longest = FieldRef::Gate(index, kind);
                        longest_len = len;
                    }
                }
            }

            if longest_len == 0 {
                // A very small caller-supplied cap can be smaller than the
                // JSON framing for every retained item. Compact the lists in
                // that case, keeping at least one explicit piece of evidence
                // rather than silently dropping a blocked marker.
                if self.drop_one_for_cap() {
                    continue;
                }
                break;
            }
            let target = longest_len.saturating_sub(excess);
            let before = self.serialized_bytes();
            match longest {
                FieldRef::Final => {
                    if let Some(text) = self.final_message.as_mut() {
                        *text = truncate_preserving_marker(text, target);
                    }
                }
                FieldRef::Tool(index, ToolField::ToolName) => {
                    self.tool_errors[index].tool_name =
                        truncate_bytes(&self.tool_errors[index].tool_name, target);
                }
                FieldRef::Tool(index, ToolField::Signature) => {
                    self.tool_errors[index].signature =
                        truncate_preserving_marker(&self.tool_errors[index].signature, target);
                }
                FieldRef::Tool(index, ToolField::Excerpt) => {
                    self.tool_errors[index].excerpt =
                        truncate_preserving_marker(&self.tool_errors[index].excerpt, target);
                }
                FieldRef::Gate(index, GateField::GateName) => {
                    self.gate_diagnostics[index].gate_name =
                        truncate_bytes(&self.gate_diagnostics[index].gate_name, target);
                }
                FieldRef::Gate(index, GateField::ErrorBlock) => {
                    self.gate_diagnostics[index].error_block = truncate_preserving_marker(
                        &self.gate_diagnostics[index].error_block,
                        target,
                    );
                }
                FieldRef::None => break,
            }
            if self.serialized_bytes() >= before && !self.drop_one_for_cap() {
                break;
            }
        }
        self
    }

    fn drop_one_for_cap(&mut self) -> bool {
        if self.tool_errors.len() > 1 {
            self.tool_errors.remove(0);
        } else if self.gate_diagnostics.len() > 1 {
            self.gate_diagnostics.remove(0);
        } else if !self.gate_diagnostics.is_empty() && !self.tool_errors.is_empty() {
            self.gate_diagnostics.clear();
        } else if !self.tool_errors.is_empty() {
            self.tool_errors.clear();
        } else if !self.gate_diagnostics.is_empty() {
            self.gate_diagnostics.clear();
        } else {
            return false;
        }
        true
    }
}

#[derive(Clone, Copy)]
enum FieldRef {
    None,
    Final,
    Tool(usize, ToolField),
    Gate(usize, GateField),
}

#[derive(Clone, Copy)]
enum ToolField {
    ToolName,
    Signature,
    Excerpt,
}

#[derive(Clone, Copy)]
enum GateField {
    GateName,
    ErrorBlock,
}

/// One resolved attempt, as the next attempt should see it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttemptRecord {
    /// [`SCHEMA_VERSION`] at the time of writing.
    pub schema_version: u32,
    /// The `attempt.resolved` ledger row's attempt ID.
    pub attempt_id: String,
    /// RFC 3339 time the attempt resolved.
    pub recorded_at: String,
    /// Worker that ran the attempt.
    pub worker: String,
    /// Adapter that executed it (e.g. `claude-code-glm-5.3-flash`).
    pub adapter: String,
    /// Model identifier when the adapter declares one.
    #[serde(default)]
    pub model: Option<String>,
    /// Semantic outcome class (`verified_success`, `work_failure`,
    /// `infrastructure_failure`, `indeterminate`, `cancelled`).
    pub outcome: String,
    /// Machine-readable terminal reason (`gate:definition-of-done`,
    /// `exit_code:1`, `timeout`, `signal:9`).
    #[serde(default)]
    pub terminal_reason: Option<String>,
    /// Process exit code (observation only).
    pub exit_code: i32,
    /// Lifecycle action NEEDLE requested afterwards (`Released`, `Deferred`…).
    pub requested_action: String,
    /// Commits the attempt produced in the workspace.
    #[serde(default)]
    pub commits: Vec<String>,
    /// Wall-clock duration of the attempt.
    pub duration_ms: u64,
    /// Bounded, sanitized failure text — the part the next agent must read.
    #[serde(default)]
    pub failure_summary: Option<String>,
    /// Bounded, sanitized structured evidence from the failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_evidence: Option<FailureEvidence>,
}

impl AttemptRecord {
    /// Whether this attempt's evidence was accepted.
    pub fn is_verified_success(&self) -> bool {
        self.outcome == VERIFIED_SUCCESS
    }
}

/// Capture structured failure evidence from a raw or normalized JSONL
/// transcript and an optional gate report.
///
/// The parser is deliberately tolerant: adapters can be killed between any
/// two lines, so malformed trailing JSON and unmatched tool calls are normal
/// timeout shapes. It understands the raw Claude/Codex/opencode forms as well
/// as the normalized [`crate::agent_event::AgentEvent`] form written by the
/// existing output transforms.
pub fn capture_failure_evidence(
    transcript: &str,
    gate_report: Option<&GateReport>,
    sanitizer: Option<&Sanitizer>,
) -> Option<FailureEvidence> {
    capture_failure_evidence_with_limit(
        transcript,
        gate_report,
        sanitizer,
        MAX_FAILURE_EVIDENCE_BYTES,
        false,
    )
}

/// Capture evidence while explicitly recording that sanitization was blocked.
///
/// This is used by the outcome controller when the configured sanitizer could
/// not be built. Source content is never allowed through in that case: every
/// captured content field receives [`SANITIZER_BLOCKED_MARKER`].
pub fn capture_failure_evidence_blocked(
    transcript: &str,
    gate_report: Option<&GateReport>,
) -> Option<FailureEvidence> {
    capture_failure_evidence_with_limit(
        transcript,
        gate_report,
        None,
        MAX_FAILURE_EVIDENCE_BYTES,
        true,
    )
}

/// Capture evidence with the caller's local-record cap.
pub fn capture_failure_evidence_with_limit(
    transcript: &str,
    gate_report: Option<&GateReport>,
    sanitizer: Option<&Sanitizer>,
    max_bytes: usize,
    sanitizer_blocked: bool,
) -> Option<FailureEvidence> {
    let mut parsed = ParsedTranscript::default();
    for line in transcript.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        parse_transcript_value(&value, &mut parsed);
    }

    let final_message = parsed
        .final_message
        .map(|text| sanitize_content(&text, sanitizer, sanitizer_blocked, FINAL_MESSAGE_BYTES));
    let tool_errors: Vec<ToolErrorEvidence> = parsed
        .tool_errors
        .into_iter()
        .rev()
        .take(MAX_TOOL_ERRORS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|error| {
            let sanitized_output = sanitize_untrusted(&error.output, sanitizer, sanitizer_blocked);
            let content = truncate_head_tail(&sanitized_output, TOOL_EXCERPT_BYTES);
            ToolErrorEvidence {
                tool_name: sanitize_name(&error.tool_name, sanitizer, sanitizer_blocked),
                signature: truncate_head_tail(
                    &crate::verification_fingerprint::normalize_output(&sanitized_output),
                    TOOL_SIGNATURE_BYTES,
                ),
                excerpt: content,
            }
        })
        .collect();

    let gate_diagnostics: Vec<GateDiagnostic> = gate_report
        .map(|report| {
            let mut results: Vec<(&String, &GateResult)> = report.results.iter().collect();
            results.sort_by(|a, b| a.0.cmp(b.0));
            results
                .into_iter()
                .filter_map(|(name, result)| {
                    let raw = match result {
                        GateResult::Pass => return None,
                        GateResult::Fail(reason) => first_error_block(reason).to_string(),
                        GateResult::ExecutionError { command, reason } => {
                            let combined = format!("{reason}\ncommand: {command}");
                            first_error_block(&combined).to_string()
                        }
                    };
                    Some(GateDiagnostic {
                        gate_name: sanitize_name(name, sanitizer, sanitizer_blocked),
                        error_block: sanitize_content(
                            &raw,
                            sanitizer,
                            sanitizer_blocked,
                            GATE_ERROR_BLOCK_BYTES,
                        ),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if final_message.is_none() && tool_errors.is_empty() && gate_diagnostics.is_empty() {
        return None;
    }

    Some(
        FailureEvidence {
            final_message,
            tool_errors,
            gate_diagnostics,
        }
        .bounded(max_bytes),
    )
}

#[derive(Default)]
struct ParsedTranscript {
    final_message: Option<String>,
    tool_errors: Vec<RawToolError>,
    pending_tools: HashMap<String, String>,
}

struct RawToolError {
    tool_name: String,
    output: String,
}

fn parse_transcript_value(value: &Value, parsed: &mut ParsedTranscript) {
    match value.get("type").and_then(Value::as_str) {
        Some("assistant") => parse_assistant_message(value.get("message"), parsed),
        Some("user") => parse_user_message(value.get("message"), parsed),
        Some("tool_result") => parse_tool_result(value, parsed),
        Some("agent_message") => {
            let is_user = value
                .get("role")
                .and_then(Value::as_str)
                .map(|role| role.eq_ignore_ascii_case("user"))
                .unwrap_or(false);
            if !is_user {
                if let Some(content) = value.get("content").and_then(Value::as_str) {
                    remember_final_message(content, parsed);
                }
            }
        }
        Some("result") => {
            if let Some(result) = value.get("result").and_then(Value::as_str) {
                remember_final_message(result, parsed);
            }
        }
        Some("item.completed") => parse_completed_item(value.get("item"), parsed),
        Some("tool_use") => {
            if let (Some(id), Some(name)) = (
                value.get("id").and_then(Value::as_str),
                value.get("name").and_then(Value::as_str),
            ) {
                parsed
                    .pending_tools
                    .insert(id.to_string(), name.to_string());
            }
        }
        Some("tool_call") => {
            if let (Some(id), Some(name)) = (
                value.get("id").and_then(Value::as_str),
                value.get("tool").and_then(Value::as_str),
            ) {
                parsed
                    .pending_tools
                    .insert(id.to_string(), name.to_string());
            }
        }
        _ => {}
    }

    // opencode carries tool calls and their result in one `tool_use` line.
    if value.get("type").and_then(Value::as_str) == Some("tool_use") {
        parse_opencode_tool(value.get("part"), parsed);
    }
}

fn parse_assistant_message(message: Option<&Value>, parsed: &mut ParsedTranscript) {
    let Some(message) = message else { return };
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        if let Some(text) = message.get("content").and_then(Value::as_str) {
            remember_final_message(text, parsed);
        }
        return;
    };
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    remember_final_message(text, parsed);
                }
            }
            Some("tool_use") => {
                if let (Some(id), Some(name)) = (
                    block.get("id").and_then(Value::as_str),
                    block.get("name").and_then(Value::as_str),
                ) {
                    parsed
                        .pending_tools
                        .insert(id.to_string(), name.to_string());
                }
            }
            _ => {}
        }
    }
}

fn parse_user_message(message: Option<&Value>, parsed: &mut ParsedTranscript) {
    let Some(content) = message.and_then(|m| m.get("content")) else {
        return;
    };
    let Some(blocks) = content.as_array() else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("tool_result") {
            parse_tool_result(block, parsed);
        }
    }
}

fn parse_tool_result(value: &Value, parsed: &mut ParsedTranscript) {
    let is_error = value
        .get("is_error")
        .and_then(Value::as_bool)
        .or_else(|| value.get("success").and_then(Value::as_bool).map(|v| !v))
        .unwrap_or(false);
    if !is_error {
        return;
    }
    let tool_name = value
        .get("tool")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("tool_use_id")
                .and_then(Value::as_str)
                .and_then(|id| parsed.pending_tools.get(id).map(String::as_str))
        })
        .unwrap_or("unknown")
        .to_string();
    let output = value
        .get("output")
        .or_else(|| value.get("content"))
        .map(value_as_text)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "tool failed without an error message".to_string());
    parsed.tool_errors.push(RawToolError { tool_name, output });
    if let Some(id) = value.get("tool_use_id").and_then(Value::as_str) {
        parsed.pending_tools.remove(id);
    }
}

fn parse_completed_item(item: Option<&Value>, parsed: &mut ParsedTranscript) {
    let Some(item) = item else { return };
    match item.get("type").and_then(Value::as_str) {
        Some("agent_message") => {
            let content = item
                .get("content")
                .or_else(|| item.get("text"))
                .and_then(Value::as_str);
            if let Some(content) = content {
                remember_final_message(content, parsed);
            }
        }
        Some("command_execution") => {
            let failed = item
                .get("exit_code")
                .and_then(Value::as_i64)
                .map(|code| code != 0)
                .unwrap_or(false);
            if failed {
                let output = item
                    .get("output")
                    .map(value_as_text)
                    .filter(|text| !text.trim().is_empty())
                    .unwrap_or_else(|| {
                        format!(
                            "command exited with code {}",
                            item.get("exit_code").and_then(Value::as_i64).unwrap_or(-1)
                        )
                    });
                parsed.tool_errors.push(RawToolError {
                    tool_name: "shell".to_string(),
                    output,
                });
            }
        }
        Some("mcp_tool_call") | Some("collab_tool_call") => {
            if let Some(error) = item.get("error") {
                parsed.tool_errors.push(RawToolError {
                    tool_name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("mcp_tool")
                        .to_string(),
                    output: value_as_text(error),
                });
            }
        }
        _ => {}
    }
}

fn parse_opencode_tool(part: Option<&Value>, parsed: &mut ParsedTranscript) {
    let Some(part) = part else { return };
    let Some(state) = part.get("state") else {
        return;
    };
    if state.get("status").and_then(Value::as_str) != Some("error") {
        return;
    }
    let output = state
        .get("error")
        .or_else(|| state.get("output"))
        .map(value_as_text)
        .unwrap_or_else(|| "tool failed without an error message".to_string());
    parsed.tool_errors.push(RawToolError {
        tool_name: part
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        output,
    });
}

fn remember_final_message(text: &str, parsed: &mut ParsedTranscript) {
    if !text.trim().is_empty() {
        parsed.final_message = Some(text.to_string());
    }
}

fn value_as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(value_as_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => map
            .get("text")
            .or_else(|| map.get("content"))
            .or_else(|| map.get("message"))
            .map(value_as_text)
            .unwrap_or_else(|| value.to_string()),
        _ => value.to_string(),
    }
}

fn first_error_block(text: &str) -> &str {
    let mut first = None;
    for block in text.split("\n\n").map(str::trim).filter(|b| !b.is_empty()) {
        if first.is_none() {
            first = Some(block);
        }
        let lower = block.to_ascii_lowercase();
        if [
            "error",
            "failed",
            "failure",
            "panic",
            "fatal",
            "test result",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            return block;
        }
    }
    first.unwrap_or("gate failed without diagnostic output")
}

fn sanitize_content(
    text: &str,
    sanitizer: Option<&Sanitizer>,
    sanitizer_blocked: bool,
    max_bytes: usize,
) -> String {
    truncate_head_tail(
        &sanitize_untrusted(text, sanitizer, sanitizer_blocked),
        max_bytes,
    )
}

fn sanitize_untrusted(
    text: &str,
    sanitizer: Option<&Sanitizer>,
    sanitizer_blocked: bool,
) -> String {
    if text.trim().is_empty() {
        return if sanitizer_blocked {
            SANITIZER_BLOCKED_MARKER.to_string()
        } else {
            "failure reported without diagnostic output".to_string()
        };
    }
    if sanitizer_blocked || sanitizer.is_none() {
        return SANITIZER_BLOCKED_MARKER.to_string();
    }
    let Some(sanitizer) = sanitizer else {
        return SANITIZER_BLOCKED_MARKER.to_string();
    };
    let sanitized = sanitizer.sanitize(text);
    if sanitized.trim().is_empty() {
        SANITIZER_BLOCKED_MARKER.to_string()
    } else {
        sanitized
    }
}

fn sanitize_name(text: &str, sanitizer: Option<&Sanitizer>, sanitizer_blocked: bool) -> String {
    let name = sanitize_untrusted(text, sanitizer, sanitizer_blocked);
    truncate_bytes(&name, EVIDENCE_NAME_BYTES)
}

/// How much history the prompt renderer may spend.
#[derive(Debug, Clone, Copy)]
pub struct HistoryLimits {
    /// Newest attempts rendered.
    pub max_attempts: usize,
    /// Byte cap on the rendered section.
    pub max_bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        HistoryLimits {
            max_attempts: 3,
            max_bytes: 4000,
        }
    }
}

/// Journal root for `workspace`: the workspace's own `.beads/traces`, or,
/// while a state-root override is in force (ADR-030 decision 5, N-T52), a
/// stable per-workspace directory beneath the override — a fixture suite
/// journaling against any workspace path then stays inside the override. The
/// hash keeps distinct workspaces distinct and is stable across processes;
/// every reader and writer resolves through these same functions, so both
/// sides of an override agree.
fn journal_root(workspace: &Path) -> PathBuf {
    if let Some(root) = crate::state_dir::attempt_journals_under_override() {
        let mut hasher = Sha256::new();
        hasher.update(workspace.to_string_lossy().as_bytes());
        let digest = hasher.finalize();
        let id: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        return root.join(id);
    }
    workspace.join(".beads").join("traces")
}

/// Path of the local journal for `bead_id` in `workspace`.
pub fn history_path(workspace: &Path, bead_id: &BeadId) -> PathBuf {
    journal_root(workspace)
        .join(bead_id.as_ref())
        .join("attempts.jsonl")
}

/// Path of the local candidate-lesson journal for `bead_id` in `workspace`.
pub fn lessons_path(workspace: &Path, bead_id: &BeadId) -> PathBuf {
    journal_root(workspace)
        .join(bead_id.as_ref())
        .join("lessons.jsonl")
}

/// Load locally produced candidate lessons, oldest first.
pub fn load_candidate_lessons(
    workspace: &Path,
    bead_id: &BeadId,
) -> Result<Vec<crate::learning::CandidateLesson>> {
    let path = lessons_path(workspace, bead_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// Load candidate lessons from every bead trace directory in one workspace.
///
/// Lessons are per-bead for provenance, while retrieval needs a workspace-local
/// source so a recovery on a different bead can still ask the configured
/// command whether an earlier candidate applies.
pub fn load_candidate_lessons_for_workspace(
    workspace: &Path,
) -> Result<Vec<crate::learning::CandidateLesson>> {
    let traces = workspace.join(".beads").join("traces");
    if !traces.exists() {
        return Ok(Vec::new());
    }
    let mut lessons: Vec<crate::learning::CandidateLesson> = Vec::new();
    for entry in std::fs::read_dir(&traces)
        .with_context(|| format!("failed to read {}", traces.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("lessons.jsonl");
        if !path.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        lessons.extend(
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .filter_map(|line| serde_json::from_str(line).ok()),
        );
    }
    lessons.sort_by(|left, right| left.id.cmp(&right.id));
    lessons.dedup_by(|left, right| left.id == right.id);
    Ok(lessons)
}

/// Append one candidate lesson unless its stable ID is already present.
///
/// The stable-ID check is the replay boundary: replaying a resolved attempt
/// pair does not append another local record.
pub fn append_candidate_lesson(
    workspace: &Path,
    bead_id: &BeadId,
    lesson: &crate::learning::CandidateLesson,
) -> Result<bool> {
    const KEEP: usize = 50;
    let path = lessons_path(workspace, bead_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut lessons = load_candidate_lessons(workspace, bead_id).unwrap_or_default();
    if lessons.iter().any(|existing| existing.id == lesson.id) {
        return Ok(false);
    }
    lessons.push(lesson.clone());
    if lessons.len() > KEEP {
        let drop = lessons.len() - KEEP;
        lessons.drain(..drop);
    }
    let mut body = String::new();
    for lesson in &lessons {
        body.push_str(&serde_json::to_string(lesson)?);
        body.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(true)
}

/// Persist a candidate lesson locally and, when supported, in bead-rs data.
///
/// Effects stay in this storage/controller boundary; candidate construction
/// itself remains the pure reducer in [`crate::learning`].
pub async fn record_candidate_lesson(
    workspace: &Path,
    bead_id: &BeadId,
    lesson: crate::learning::CandidateLesson,
    store: &dyn BeadStore,
    mirror: bool,
) {
    let appended = match append_candidate_lesson(workspace, bead_id, &lesson) {
        Ok(appended) => appended,
        Err(error) => {
            tracing::warn!(
                bead_id = %bead_id,
                workspace = %workspace.display(),
                %error,
                "candidate lesson: failed to append local journal"
            );
            return;
        }
    };
    if !appended || !mirror {
        return;
    }
    let lessons = load_candidate_lessons(workspace, bead_id).unwrap_or_else(|_| vec![lesson]);
    let value = crate::learning::candidate_lessons_to_data_value(&lessons);
    match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        store.set_data(
            bead_id,
            LESSONS_DATA_NAMESPACE,
            LESSONS_DATA_SCHEMA_REF,
            &value,
        ),
    )
    .await
    {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => tracing::debug!(
            bead_id = %bead_id,
            "candidate lesson: backend has no structured data support; local journal only"
        ),
        Ok(Err(error)) => tracing::debug!(
            bead_id = %bead_id,
            %error,
            "candidate lesson: mirror write failed; local journal only"
        ),
        Err(_) => tracing::debug!(
            bead_id = %bead_id,
            "candidate lesson: mirror write timed out; local journal only"
        ),
    }
}

/// Load local candidate lessons, falling back to bead-rs data on another host.
pub async fn load_candidate_lessons_for_retrieval(
    workspace: &Path,
    bead_id: &BeadId,
    store: &dyn BeadStore,
) -> Vec<crate::learning::CandidateLesson> {
    match load_candidate_lessons_for_workspace(workspace) {
        Ok(lessons) if !lessons.is_empty() => return lessons,
        Ok(_) => {}
        Err(error) => tracing::debug!(
            bead_id = %bead_id,
            %error,
            "candidate lesson: local journal unreadable, trying bead-rs mirror"
        ),
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        store.get_data(bead_id, LESSONS_DATA_NAMESPACE),
    )
    .await
    {
        Ok(Ok(Some(value))) => crate::learning::candidate_lessons_from_data_value(&value),
        Ok(Ok(None)) | Ok(Err(_)) | Err(_) => Vec::new(),
    }
}

/// Append `record` to the local journal, keeping the newest [`LOCAL_KEEP`].
pub fn append_local(workspace: &Path, bead_id: &BeadId, record: &AttemptRecord) -> Result<()> {
    let path = history_path(workspace, bead_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut records = load_local(workspace, bead_id).unwrap_or_default();
    let mut record = record.clone();
    record.failure_evidence = record
        .failure_evidence
        .take()
        .map(|evidence| evidence.bounded(MAX_FAILURE_EVIDENCE_BYTES));
    records.push(record);
    if records.len() > LOCAL_KEEP {
        let drop = records.len() - LOCAL_KEEP;
        records.drain(..drop);
    }
    let mut body = String::new();
    for r in &records {
        body.push_str(&serde_json::to_string(r)?);
        body.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

/// Load the local journal, oldest first. A missing file is an empty history;
/// a malformed line is skipped rather than failing the whole read.
pub fn load_local(workspace: &Path, bead_id: &BeadId) -> Result<Vec<AttemptRecord>> {
    let path = history_path(workspace, bead_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<AttemptRecord>(l).ok())
        .collect())
}

/// The bounded mirror written to bead-rs structured data: the newest
/// [`MIRROR_KEEP`] records with legacy summaries and structured evidence cut
/// to [`MIRROR_SUMMARY_BYTES`].
pub fn to_data_value(records: &[AttemptRecord]) -> serde_json::Value {
    let start = records.len().saturating_sub(MIRROR_KEEP);
    let attempts: Vec<AttemptRecord> = records[start..]
        .iter()
        .map(|r| AttemptRecord {
            failure_summary: r
                .failure_summary
                .as_deref()
                .map(|s| truncate_head_tail(s, MIRROR_SUMMARY_BYTES)),
            failure_evidence: r
                .failure_evidence
                .clone()
                .map(|evidence| evidence.bounded(MIRROR_SUMMARY_BYTES)),
            ..r.clone()
        })
        .collect();
    serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "total_attempts": records.len(),
        "attempts": attempts,
    })
}

/// Parse a mirror written by [`to_data_value`]. Unknown shapes yield nothing.
pub fn from_data_value(value: &serde_json::Value) -> Vec<AttemptRecord> {
    value
        .get("attempts")
        .and_then(|a| a.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| {
                    let mut record = serde_json::from_value::<AttemptRecord>(v.clone()).ok()?;
                    record.failure_evidence = record
                        .failure_evidence
                        .take()
                        .map(|evidence| evidence.bounded(MIRROR_SUMMARY_BYTES));
                    Some(record)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Load the history a prompt should show: the local journal first, the
/// cross-host mirror when the local journal is empty. Never fails — a store
/// error is logged and yields an empty history.
pub async fn load_for_prompt(
    workspace: &Path,
    bead_id: &BeadId,
    store: &dyn BeadStore,
) -> Vec<AttemptRecord> {
    match load_local(workspace, bead_id) {
        Ok(records) if !records.is_empty() => return records,
        Ok(_) => {}
        Err(e) => tracing::debug!(
            bead_id = %bead_id,
            error = %e,
            "attempt history: local journal unreadable, trying the bead-rs mirror"
        ),
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        store.get_data(bead_id, DATA_NAMESPACE),
    )
    .await
    {
        Ok(Ok(Some(value))) => from_data_value(&value),
        Ok(Ok(None)) => Vec::new(),
        Ok(Err(e)) => {
            tracing::debug!(bead_id = %bead_id, error = %e, "attempt history: mirror read failed");
            Vec::new()
        }
        Err(_) => {
            tracing::debug!(bead_id = %bead_id, "attempt history: mirror read timed out");
            Vec::new()
        }
    }
}

/// Persist one resolved attempt: append it locally and refresh the mirror.
///
/// Best-effort on both stores. The local append failing is logged at warn
/// (it is the primary source); the mirror failing is logged at debug (an
/// older backend has no `data` command, and that is not an error).
pub async fn record(
    workspace: &Path,
    bead_id: &BeadId,
    record: AttemptRecord,
    store: &dyn BeadStore,
    mirror: bool,
) {
    if let Err(e) = append_local(workspace, bead_id, &record) {
        tracing::warn!(
            bead_id = %bead_id,
            workspace = %workspace.display(),
            error = %e,
            "attempt history: failed to append the local journal"
        );
        return;
    }
    if !mirror {
        return;
    }
    let records = load_local(workspace, bead_id).unwrap_or_else(|_| vec![record]);
    let value = to_data_value(&records);
    match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        store.set_data(bead_id, DATA_NAMESPACE, DATA_SCHEMA_REF, &value),
    )
    .await
    {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => tracing::debug!(
            bead_id = %bead_id,
            "attempt history: backend has no structured data support; local journal only"
        ),
        Ok(Err(e)) => tracing::debug!(
            bead_id = %bead_id,
            error = %e,
            "attempt history: mirror write failed; local journal only"
        ),
        Err(_) => tracing::debug!(
            bead_id = %bead_id,
            "attempt history: mirror write timed out; local journal only"
        ),
    }
}

/// Render the newest attempts as the prompt section, or `""` when there is
/// nothing an agent needs to know (no prior attempts).
///
/// Newest first. The header states how many attempts failed so the agent
/// treats the bead as a retry, and the instruction is explicit: read the
/// failure, do not repeat it, make a failed gate pass first.
pub fn render(records: &[AttemptRecord], limits: HistoryLimits) -> String {
    if records.is_empty() || limits.max_attempts == 0 {
        return String::new();
    }
    let failed = records.iter().filter(|r| !r.is_verified_success()).count();
    let mut out = String::new();
    out.push_str("## Previous attempts on this bead (newest first)\n\n");
    out.push_str(&format!(
        "This bead has been attempted {} time{} before ({} without a verified success). \
         Read the failures below before you start. Do NOT repeat an approach that already \
         failed the same way. If a verification gate failed, make that gate pass first — \
         the gate is what decides whether your work is accepted.\n\n",
        records.len(),
        if records.len() == 1 { "" } else { "s" },
        failed
    ));
    if out.len() >= limits.max_bytes {
        return truncate_bytes(&out, limits.max_bytes)
            .trim_end()
            .to_string();
    }

    let newest_first = records.iter().rev().take(limits.max_attempts);
    for (i, r) in newest_first.enumerate() {
        let n = records.len() - i;
        let mut block = String::new();
        block.push_str(&format!(
            "### Attempt {n} — {} — {} — outcome: {}{}\n",
            short_time(&r.recorded_at),
            r.adapter,
            r.outcome,
            r.terminal_reason
                .as_deref()
                .map(|t| format!(" ({t})"))
                .unwrap_or_default()
        ));
        if !r.commits.is_empty() {
            block.push_str(&format!(
                "Commits it left in the workspace: {}\n",
                r.commits
                    .iter()
                    .map(|c| c.chars().take(12).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(summary) = r
            .failure_summary
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            if r.failure_evidence.is_none() {
                block.push_str("```\n");
                block.push_str(summary.trim_end());
                block.push_str("\n```\n");
            }
        }
        if let Some(evidence) = r.failure_evidence.as_ref() {
            block.push_str(&render_failure_evidence(evidence));
        }
        block.push('\n');
        if out.len() + block.len() > limits.max_bytes {
            let omitted = format!(
                "(older attempts omitted — history capped at {} bytes)\n",
                limits.max_bytes
            );
            let remaining = limits.max_bytes.saturating_sub(out.len());
            if remaining > 0 {
                out.push_str(&truncate_bytes(&omitted, remaining));
            }
            break;
        }
        out.push_str(&block);
    }
    out.trim_end().to_string()
}

fn render_failure_evidence(evidence: &FailureEvidence) -> String {
    let mut out = String::from("Failure evidence:\n");
    if let Some(message) = evidence
        .final_message
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        out.push_str("Final assistant message:\n```\n");
        out.push_str(message.trim_end());
        out.push_str("\n```\n");
    }
    for error in &evidence.tool_errors {
        out.push_str(&format!(
            "Tool error — {} — signature: {}\n```\n{}\n```\n",
            error.tool_name,
            error.signature,
            error.excerpt.trim_end()
        ));
    }
    for diagnostic in &evidence.gate_diagnostics {
        out.push_str(&format!(
            "Gate diagnostic — {}:\n```\n{}\n```\n",
            diagnostic.gate_name,
            diagnostic.error_block.trim_end()
        ));
    }
    out
}

/// `2026-09-12T14:19:11.123Z` → `2026-09-12T14:19Z`; anything else unchanged.
fn short_time(ts: &str) -> String {
    if ts.len() >= 16 && ts.as_bytes()[10] == b'T' {
        format!("{}Z", &ts[..16])
    } else {
        ts.to_string()
    }
}

/// Keep the first two thirds and last third of `text` when it exceeds
/// `max_bytes`, on char boundaries, with an elision marker between.
pub fn truncate_head_tail(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let marker = "\n… [elided] …\n";
    if max_bytes <= marker.len() + 2 {
        return truncate_bytes(text, max_bytes);
    }
    let budget = max_bytes - marker.len();
    let head_len = budget * 2 / 3;
    let tail_len = budget - head_len;
    let mut head_end = head_len;
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len() - tail_len;
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!("{}{}{}", &text[..head_end], marker, &text[tail_start..])
}

/// Truncate a string to a byte cap without splitting UTF-8.
fn truncate_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .take_while(|(index, _)| *index < max_bytes)
        .map(|(index, _)| index)
        .last()
        .unwrap_or(0);
    text[..end].to_string()
}

fn truncate_preserving_marker(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        text.to_string()
    } else if max_bytes < SANITIZER_BLOCKED_MARKER.len() {
        SANITIZER_BLOCKED_MARKER.to_string()
    } else {
        truncate_head_tail(text, max_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(n: u32, outcome: &str, summary: Option<&str>) -> AttemptRecord {
        AttemptRecord {
            schema_version: SCHEMA_VERSION,
            attempt_id: format!("att-{n}"),
            recorded_at: format!("2026-09-12T14:{:02}:00.000Z", n),
            worker: "w".into(),
            adapter: "claude-code-glm-5.3-flash".into(),
            model: Some("glm-5.3-flash".into()),
            outcome: outcome.into(),
            terminal_reason: Some("gate:definition-of-done".into()),
            exit_code: 0,
            requested_action: "Released".into(),
            commits: vec!["abcdef1234567890".into()],
            duration_ms: 1000,
            failure_summary: summary.map(str::to_string),
            failure_evidence: None,
        }
    }

    #[test]
    fn append_and_load_round_trip_keeps_order_and_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let id = BeadId::from("nd-1");
        for n in 0..(LOCAL_KEEP as u32 + 5) {
            append_local(dir.path(), &id, &rec(n, "work_failure", Some("boom"))).unwrap();
        }
        let loaded = load_local(dir.path(), &id).unwrap();
        assert_eq!(loaded.len(), LOCAL_KEEP);
        assert_eq!(loaded[0].attempt_id, "att-5");
        assert_eq!(
            loaded.last().unwrap().attempt_id,
            format!("att-{}", LOCAL_KEEP + 4)
        );
        assert!(history_path(dir.path(), &id).exists());
    }

    #[test]
    fn load_skips_malformed_lines_and_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let id = BeadId::from("nd-2");
        assert!(load_local(dir.path(), &id).unwrap().is_empty());
        let path = history_path(dir.path(), &id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "not json\n{}\n",
                serde_json::to_string(&rec(1, "work_failure", None)).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(load_local(dir.path(), &id).unwrap().len(), 1);
    }

    #[test]
    fn render_is_empty_without_history_and_newest_first_with() {
        assert_eq!(render(&[], HistoryLimits::default()), "");
        let records = vec![
            rec(1, "work_failure", Some("error[E0308]: mismatched types")),
            rec(2, "indeterminate", None),
        ];
        let text = render(&records, HistoryLimits::default());
        assert!(text.starts_with("## Previous attempts on this bead"));
        assert!(text.contains("attempted 2 times before (2 without a verified success)"));
        let a2 = text.find("### Attempt 2").unwrap();
        let a1 = text.find("### Attempt 1").unwrap();
        assert!(a2 < a1, "newest first");
        assert!(text.contains("error[E0308]"));
        assert!(text.contains("abcdef123456"));
        assert!(text.contains("Do NOT repeat"));
    }

    #[test]
    fn render_respects_attempt_and_byte_caps() {
        let records: Vec<_> = (0..6)
            .map(|n| rec(n, "work_failure", Some(&"x".repeat(500))))
            .collect();
        let capped = render(
            &records,
            HistoryLimits {
                max_attempts: 2,
                max_bytes: 100_000,
            },
        );
        assert_eq!(capped.matches("### Attempt").count(), 2);
        let bytes = render(
            &records,
            HistoryLimits {
                max_attempts: 6,
                max_bytes: 900,
            },
        );
        assert!(bytes.len() <= 1000, "{}", bytes.len());
        assert!(bytes.contains("older attempts omitted"));
    }

    #[test]
    fn mirror_value_round_trips_and_is_bounded() {
        let records: Vec<_> = (0..9)
            .map(|n| rec(n, "work_failure", Some(&"y".repeat(2000))))
            .collect();
        let value = to_data_value(&records);
        assert_eq!(value["total_attempts"], 9);
        let back = from_data_value(&value);
        assert_eq!(back.len(), MIRROR_KEEP);
        assert_eq!(back[0].attempt_id, "att-4");
        let summary = back[0].failure_summary.as_ref().unwrap();
        assert!(
            summary.len() <= MIRROR_SUMMARY_BYTES + 20,
            "{}",
            summary.len()
        );
        assert!(summary.contains("[elided]"));
        assert!(from_data_value(&serde_json::json!({"nope": 1})).is_empty());
    }

    #[test]
    fn truncate_head_tail_keeps_both_ends() {
        let text = format!("{}MIDDLE{}", "h".repeat(400), "t".repeat(400));
        let cut = truncate_head_tail(&text, 300);
        assert!(cut.starts_with("hhh"));
        assert!(cut.ends_with("ttt"));
        assert!(!cut.contains("MIDDLE"));
        assert!(cut.len() <= 300);
        assert_eq!(truncate_head_tail("short", 300), "short");
        let multi = "é".repeat(300);
        let cut = truncate_head_tail(&multi, 100);
        assert!(cut.len() <= 100);
    }
}
