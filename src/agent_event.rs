//! Normalized agent event schema (schema version 1).
//!
//! This module defines the canonical JSONL event types that all output
//! transforms must produce.  NEEDLE itself does **not** validate or parse
//! these events at runtime — it writes them opaquely to disk.  Validation
//! and consumption is FABRIC's concern.
//!
//! # Contract
//!
//! Every line written by an output transform is a JSON object that
//! deserializes into [`AgentEvent`].  All events share a top-level
//! `schema_version` field (currently `1`) and a `ts` field (Unix epoch
//! seconds as an `f64`).
//!
//! See `docs/agent-event-schema.md` for the human-readable contract
//! intended for transform authors.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ──────────────────────────────────────────────────────────────────────────────
// Schema version constant
// ──────────────────────────────────────────────────────────────────────────────

/// The current schema version.  Increment when making breaking changes.
pub const SCHEMA_VERSION: u32 = 1;

// ──────────────────────────────────────────────────────────────────────────────
// Top-level envelope
// ──────────────────────────────────────────────────────────────────────────────

/// A single line in the agent event JSONL stream.
///
/// Serialises to / deserialises from a flat JSON object.  The `type` field
/// is used as a discriminant via serde's `tag` representation so that the
/// envelope and the event-specific payload occupy the same JSON object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEvent {
    /// Schema version.  Always `1` for events conforming to this module.
    pub schema_version: u32,

    /// Unix epoch timestamp in seconds (sub-second precision via fractional
    /// part).
    pub ts: f64,

    /// Event-specific payload, discriminated by the `type` field.
    #[serde(flatten)]
    pub payload: EventPayload,
}

// ──────────────────────────────────────────────────────────────────────────────
// Event payload enum
// ──────────────────────────────────────────────────────────────────────────────

/// The event-specific data for an [`AgentEvent`].
///
/// The `type` key in JSON is the serde tag used to select the variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventPayload {
    /// The agent invoked a tool.
    ToolCall(ToolCallEvent),

    /// A tool returned its result.
    ToolResult(ToolResultEvent),

    /// The agent emitted a text message.
    AgentMessage(AgentMessageEvent),

    /// A token-usage update.
    Tokens(TokensEvent),

    /// An agent-level error occurred.
    Error(ErrorEvent),
}

// ──────────────────────────────────────────────────────────────────────────────
// tool_call
// ──────────────────────────────────────────────────────────────────────────────

/// Emitted when the agent invokes a tool.
///
/// ```json
/// {
///   "schema_version": 1,
///   "type": "tool_call",
///   "tool": "Edit",
///   "path": "src/auth.ts",
///   "args": {"old_string": "foo", "new_string": "bar"},
///   "ts": 1711100400.123
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallEvent {
    /// The name of the tool invoked (e.g. `"Edit"`, `"Bash"`, `"Read"`).
    pub tool: String,

    /// File path the tool operates on, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Tool arguments as a free-form JSON object.  May be `null` or absent
    /// for tools that take no structured arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

// ──────────────────────────────────────────────────────────────────────────────
// tool_result
// ──────────────────────────────────────────────────────────────────────────────

/// Emitted when a tool returns its result.
///
/// ```json
/// {
///   "schema_version": 1,
///   "type": "tool_result",
///   "tool": "Edit",
///   "success": true,
///   "ts": 1711100400.456
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResultEvent {
    /// The name of the tool whose result this is.
    pub tool: String,

    /// Whether the tool call succeeded.
    pub success: bool,

    /// A short human-readable summary of the output, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// agent_message
// ──────────────────────────────────────────────────────────────────────────────

/// Emitted when the agent produces a text turn.
///
/// ```json
/// {
///   "schema_version": 1,
///   "type": "agent_message",
///   "role": "assistant",
///   "content": "Updated the auth flow",
///   "ts": 1711100401.0
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMessageEvent {
    /// The role of the message author (e.g. `"assistant"`, `"user"`).
    pub role: MessageRole,

    /// The text content of the message.
    pub content: String,
}

/// Role of the entity that produced an [`AgentMessageEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageRole {
    Assistant,
    User,
    System,
}

// ──────────────────────────────────────────────────────────────────────────────
// tokens
// ──────────────────────────────────────────────────────────────────────────────

/// Emitted to report cumulative or incremental token usage.
///
/// ```json
/// {
///   "schema_version": 1,
///   "type": "tokens",
///   "input": 1200,
///   "output": 450,
///   "model": "claude-sonnet-4-6",
///   "ts": 1711100403.0
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokensEvent {
    /// Number of input (prompt) tokens.
    pub input: u64,

    /// Number of output (completion) tokens.
    pub output: u64,

    /// Model identifier (e.g. `"claude-sonnet-4-6"`).
    pub model: String,

    /// Cache read tokens, if reported by the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,

    /// Cache write tokens, if reported by the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
}

// ──────────────────────────────────────────────────────────────────────────────
// error
// ──────────────────────────────────────────────────────────────────────────────

/// Emitted when the agent encounters an error.
///
/// ```json
/// {
///   "schema_version": 1,
///   "type": "error",
///   "message": "rate limit exceeded",
///   "recoverable": true,
///   "ts": 1711100410.0
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorEvent {
    /// Human-readable error description.
    pub message: String,

    /// Whether the agent may retry or continue after this error.
    pub recoverable: bool,

    /// Optional error code or category (e.g. `"rate_limit"`, `"context_overflow"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip(event: &AgentEvent) {
        let serialized = serde_json::to_string(event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, &deserialized);
    }

    #[test]
    fn tool_call_round_trip() {
        let event = AgentEvent {
            schema_version: SCHEMA_VERSION,
            ts: 1_711_100_400.123,
            payload: EventPayload::ToolCall(ToolCallEvent {
                tool: "Edit".to_string(),
                path: Some("src/auth.ts".to_string()),
                args: Some(json!({"old_string": "foo", "new_string": "bar"})),
            }),
        };
        round_trip(&event);
    }

    #[test]
    fn tool_result_round_trip() {
        let event = AgentEvent {
            schema_version: SCHEMA_VERSION,
            ts: 1_711_100_400.456,
            payload: EventPayload::ToolResult(ToolResultEvent {
                tool: "Edit".to_string(),
                success: true,
                output: None,
            }),
        };
        round_trip(&event);
    }

    #[test]
    fn agent_message_round_trip() {
        let event = AgentEvent {
            schema_version: SCHEMA_VERSION,
            ts: 1_711_100_401.0,
            payload: EventPayload::AgentMessage(AgentMessageEvent {
                role: MessageRole::Assistant,
                content: "Updated the auth flow".to_string(),
            }),
        };
        round_trip(&event);
    }

    #[test]
    fn tokens_round_trip() {
        let event = AgentEvent {
            schema_version: SCHEMA_VERSION,
            ts: 1_711_100_403.0,
            payload: EventPayload::Tokens(TokensEvent {
                input: 1200,
                output: 450,
                model: "claude-sonnet-4-6".to_string(),
                cache_read: None,
                cache_write: None,
            }),
        };
        round_trip(&event);
    }

    #[test]
    fn error_round_trip() {
        let event = AgentEvent {
            schema_version: SCHEMA_VERSION,
            ts: 1_711_100_410.0,
            payload: EventPayload::Error(ErrorEvent {
                message: "rate limit exceeded".to_string(),
                recoverable: true,
                code: Some("rate_limit".to_string()),
            }),
        };
        round_trip(&event);
    }

    #[test]
    fn deserialize_from_literal_jsonl() {
        let lines = [
            r#"{"schema_version":1,"ts":1711100400.123,"type":"tool_call","tool":"Edit","path":"src/auth.ts","args":{"old_string":"foo","new_string":"bar"}}"#,
            r#"{"schema_version":1,"ts":1711100400.456,"type":"tool_result","tool":"Edit","success":true}"#,
            r#"{"schema_version":1,"ts":1711100401.0,"type":"agent_message","role":"assistant","content":"Updated the auth flow"}"#,
            r#"{"schema_version":1,"ts":1711100403.0,"type":"tokens","input":1200,"output":450,"model":"claude-sonnet-4-6"}"#,
            r#"{"schema_version":1,"ts":1711100410.0,"type":"error","message":"rate limit exceeded","recoverable":true}"#,
        ];
        for line in lines {
            let result: Result<AgentEvent, _> = serde_json::from_str(line);
            assert!(
                result.is_ok(),
                "failed to deserialize: {line}\nerr: {result:?}"
            );
        }
    }
}
