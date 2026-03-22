//! `needle-transform-codex` — output transform for OpenAI Codex CLI.
//!
//! Reads Codex CLI `codex exec` JSONL stream from stdin and emits the
//! normalized agent event JSONL schema (v1) to stdout.
//!
//! # Codex event format
//!
//! `codex exec` emits JSONL where each line is a `ThreadEvent` with a `type`
//! field.  The main event types are:
//!
//! - `thread.started` — session metadata (model, instructions).
//! - `turn.started`   — a new LLM turn begins.
//! - `turn.completed` — turn finished with usage info.
//! - `turn.failed`    — turn-level error.
//! - `item.started`   — an item (tool call, message, etc.) begins.
//! - `item.updated`   — streaming progress for an item.
//! - `item.completed` — item finished with full content.
//! - `error`          — top-level error event.
//!
//! Item types include `command_execution`, `file_change`, `mcp_tool_call`,
//! `agent_message`, `web_search`, `todo_list`, `reasoning`, and `error`.
//!
//! Each line of Codex output is a JSON object.  Malformed or unrecognised
//! lines are skipped with a warning on stderr; the process does not crash.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use needle::agent_event::{
    AgentEvent, AgentMessageEvent, ErrorEvent, EventPayload, MessageRole, TokensEvent,
    ToolCallEvent, ToolResultEvent, SCHEMA_VERSION,
};
use serde_json::Value;

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

fn now_ts() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Truncate a string to at most `max` characters.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        s.chars().take(max).collect()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Line processor (pub for unit tests)
// ──────────────────────────────────────────────────────────────────────────────

/// Parse one line of Codex `codex exec` JSONL output and return any
/// normalized [`AgentEvent`]s it produces.
///
/// `item_map` tracks in-flight items (keyed by a synthetic id built from the
/// event index) so that `item.started` → `ToolCall` and `item.completed` →
/// `ToolResult` can be correlated.
pub(crate) fn process_line(
    line: &str,
    item_map: &mut HashMap<String, PendingItem>,
    ts: f64,
) -> Vec<AgentEvent> {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("needle-transform-codex: skipping malformed JSON ({e}): {line}");
            return vec![];
        }
    };

    match value.get("type").and_then(|v| v.as_str()) {
        Some("thread.started") => process_thread_started(&value),
        Some("turn.started") => vec![],
        Some("turn.completed") => process_turn_completed(&value, ts),
        Some("turn.failed") => process_turn_failed(&value, ts),
        Some("item.started") => process_item_started(&value, item_map, ts),
        Some("item.updated") => vec![],
        Some("item.completed") => process_item_completed(&value, item_map, ts),
        Some("error") => process_error(&value, ts),
        Some(_) => vec![], // unknown type — skip silently
        None => {
            eprintln!("needle-transform-codex: skipping line without `type` field");
            vec![]
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// In-flight item tracking
// ──────────────────────────────────────────────────────────────────────────────

/// Tracks an item seen via `item.started` so the matching `item.completed`
/// can correlate the result.
#[derive(Debug, Clone)]
struct PendingItem {
    /// The normalized tool name (e.g. `"shell"`, `"file_edit"`).
    tool: String,
}

/// Build a stable key for an item from its Codex `item` object.
fn item_key(item: &Value) -> String {
    let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let item_type = item
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    if id.is_empty() {
        item_type.to_owned()
    } else {
        format!("{id}:{item_type}")
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Per-type handlers
// ──────────────────────────────────────────────────────────────────────────────

fn process_thread_started(_value: &Value) -> Vec<AgentEvent> {
    // thread.started carries session metadata but no user-visible action.
    // We emit nothing — turn-level events carry all useful info.
    vec![]
}

fn process_turn_completed(value: &Value, ts: f64) -> Vec<AgentEvent> {
    let mut events: Vec<AgentEvent> = vec![];

    let usage = match value.get("usage") {
        Some(u) => u,
        None => return events,
    };

    let input_tokens = usage
        .get("input_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let cached_tokens = usage.get("cached_input_tokens").and_then(|t| t.as_u64());

    let model = value
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_owned();

    if input_tokens > 0 || output_tokens > 0 {
        events.push(AgentEvent {
            schema_version: SCHEMA_VERSION,
            ts,
            payload: EventPayload::Tokens(TokensEvent {
                input: input_tokens,
                output: output_tokens,
                model,
                cache_read: cached_tokens.filter(|&v| v > 0),
                cache_write: None,
            }),
        });
    }

    events
}

fn process_turn_failed(value: &Value, ts: f64) -> Vec<AgentEvent> {
    let message = value
        .get("message")
        .and_then(|m| m.as_str())
        .or_else(|| value.get("error").and_then(|e| e.as_str()))
        .unwrap_or("turn failed")
        .to_owned();

    let code = value
        .get("code")
        .and_then(|c| c.as_str())
        .map(str::to_owned);

    vec![AgentEvent {
        schema_version: SCHEMA_VERSION,
        ts,
        payload: EventPayload::Error(ErrorEvent {
            message,
            recoverable: false,
            code,
        }),
    }]
}

fn process_item_started(
    value: &Value,
    item_map: &mut HashMap<String, PendingItem>,
    ts: f64,
) -> Vec<AgentEvent> {
    let item = match value.get("item") {
        Some(i) => i,
        None => return vec![],
    };

    let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");

    let tool = match item_type {
        "command_execution" => "shell".to_owned(),
        "file_change" => "file_edit".to_owned(),
        "mcp_tool_call" | "collab_tool_call" => item
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("mcp_tool")
            .to_owned(),
        _ => return vec![], // agent_message, reasoning, etc. — no ToolCall on start
    };

    let key = item_key(item);
    item_map.insert(key, PendingItem { tool: tool.clone() });

    let path = extract_item_path(item_type, item);

    let args = match item_type {
        "command_execution" => item
            .get("command")
            .and_then(|c| c.as_str())
            .map(|c| serde_json::json!({"command": c})),
        "file_change" => item.get("changes").cloned(),
        "mcp_tool_call" | "collab_tool_call" => item.get("arguments").cloned(),
        _ => None,
    };

    vec![AgentEvent {
        schema_version: SCHEMA_VERSION,
        ts,
        payload: EventPayload::ToolCall(ToolCallEvent { tool, path, args }),
    }]
}

fn process_item_completed(
    value: &Value,
    item_map: &mut HashMap<String, PendingItem>,
    ts: f64,
) -> Vec<AgentEvent> {
    let item = match value.get("item") {
        Some(i) => i,
        None => return vec![],
    };

    let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match item_type {
        "command_execution" | "file_change" => emit_tool_result(item_map, item, item_type, ts),
        "mcp_tool_call" | "collab_tool_call" => emit_tool_result(item_map, item, item_type, ts),
        "agent_message" => emit_agent_message(item, ts),
        "error" => emit_error_item(item, ts),
        _ => vec![], // reasoning, web_search, todo_list — skip
    }
}

/// Emit a `ToolResult` for a completed tool-like item.
fn emit_tool_result(
    item_map: &mut HashMap<String, PendingItem>,
    item: &Value,
    item_type: &str,
    ts: f64,
) -> Vec<AgentEvent> {
    let key = item_key(item);
    let pending = item_map.remove(&key);
    let tool = pending
        .as_ref()
        .map(|p| p.tool.clone())
        .unwrap_or_else(|| item_type.to_owned());

    let success = match item_type {
        "command_execution" => {
            // Codex uses exit_code 0 for success.
            item.get("exit_code").and_then(|c| c.as_i64()).unwrap_or(-1) == 0
        }
        "file_change" => {
            // file_change items don't carry an explicit success flag;
            // treat presence of changes as success.
            item.get("changes")
                .and_then(|c| c.as_array())
                .map_or(true, |a| !a.is_empty())
        }
        _ => {
            // mcp_tool_call / collab_tool_call — check for an error field.
            item.get("error").is_none()
        }
    };

    let output = extract_item_output(item, item_type);

    vec![AgentEvent {
        schema_version: SCHEMA_VERSION,
        ts,
        payload: EventPayload::ToolResult(ToolResultEvent {
            tool,
            success,
            output,
        }),
    }]
}

/// Extract a short human-readable output summary from a completed item.
fn extract_item_output(item: &Value, item_type: &str) -> Option<String> {
    match item_type {
        "command_execution" => item
            .get("output")
            .and_then(|o| o.as_str())
            .map(|s| truncate(s.trim(), 500))
            .filter(|s| !s.is_empty()),
        "file_change" => item.get("changes").and_then(|c| c.as_array()).map(|arr| {
            let paths: Vec<String> = arr
                .iter()
                .filter_map(|c| c.get("path").and_then(|p| p.as_str()).map(str::to_owned))
                .collect();
            if paths.is_empty() {
                "no changes".to_owned()
            } else {
                truncate(&paths.join(", "), 500)
            }
        }),
        "mcp_tool_call" | "collab_tool_call" => {
            // Check for error first.
            if let Some(err) = item.get("error") {
                err.as_str()
                    .or_else(|| err.get("message").and_then(|m| m.as_str()))
                    .map(|s| truncate(s.trim(), 500))
                    .filter(|s| !s.is_empty())
            } else {
                item.get("output")
                    .and_then(|o| o.as_str())
                    .map(|s| truncate(s.trim(), 500))
                    .filter(|s| !s.is_empty())
            }
        }
        _ => None,
    }
}

/// Extract the file path from an item, if applicable.
fn extract_item_path(item_type: &str, item: &Value) -> Option<String> {
    match item_type {
        "file_change" => item
            .get("changes")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("path"))
            .and_then(|p| p.as_str())
            .map(str::to_owned),
        _ => None,
    }
}

/// Emit an `AgentMessage` for a completed agent_message item.
fn emit_agent_message(item: &Value, ts: f64) -> Vec<AgentEvent> {
    let content = item.get("content").and_then(|c| c.as_str()).unwrap_or("");

    if content.is_empty() {
        return vec![];
    }

    vec![AgentEvent {
        schema_version: SCHEMA_VERSION,
        ts,
        payload: EventPayload::AgentMessage(AgentMessageEvent {
            role: MessageRole::Assistant,
            content: content.to_owned(),
        }),
    }]
}

/// Emit an `Error` for a completed error item.
fn emit_error_item(item: &Value, ts: f64) -> Vec<AgentEvent> {
    let message = item
        .get("message")
        .and_then(|m| m.as_str())
        .or_else(|| item.get("error").and_then(|e| e.as_str()))
        .unwrap_or("unknown item error")
        .to_owned();

    let code = item.get("code").and_then(|c| c.as_str()).map(str::to_owned);

    vec![AgentEvent {
        schema_version: SCHEMA_VERSION,
        ts,
        payload: EventPayload::Error(ErrorEvent {
            message,
            recoverable: true, // item-level errors may be recoverable
            code,
        }),
    }]
}

/// Emit an `Error` for a top-level error event.
fn process_error(value: &Value, ts: f64) -> Vec<AgentEvent> {
    let message = value
        .get("message")
        .and_then(|m| m.as_str())
        .or_else(|| {
            value.get("error").and_then(|e| {
                e.as_str()
                    .or_else(|| e.get("message").and_then(|m| m.as_str()))
            })
        })
        .unwrap_or("unknown error")
        .to_owned();

    let code = value
        .get("code")
        .and_then(|c| c.as_str())
        .map(str::to_owned);

    vec![AgentEvent {
        schema_version: SCHEMA_VERSION,
        ts,
        payload: EventPayload::Error(ErrorEvent {
            message,
            recoverable: false,
            code,
        }),
    }]
}

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Tracks in-flight items (started but not yet completed).
    let mut item_map: HashMap<String, PendingItem> = HashMap::new();

    for line_result in stdin.lock().lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                eprintln!("needle-transform-codex: stdin read error: {e}");
                break;
            }
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let ts = now_ts();
        let events = process_line(line, &mut item_map, ts);

        for event in &events {
            match serde_json::to_string(event) {
                Ok(json) => {
                    if writeln!(out, "{json}").is_err() {
                        // stdout closed (downstream terminated) — exit cleanly.
                        return;
                    }
                }
                Err(e) => {
                    eprintln!("needle-transform-codex: serialization error (bug): {e}");
                }
            }
        }

        // Flush after each batch so downstream sees events in real time.
        if !events.is_empty() && out.flush().is_err() {
            return;
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Unit tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use needle::agent_event::{EventPayload, MessageRole};

    // ── process_line: item.started command_execution ─────────────────────────

    #[test]
    fn item_started_command_execution_emits_tool_call() {
        let mut map = HashMap::new();
        let line = r#"{"type":"item.started","item":{"type":"command_execution","id":"item-1","command":"ls -la"}}"#;
        let events = process_line(line, &mut map, 1711100400.0);
        assert_eq!(events.len(), 1);

        match &events[0].payload {
            EventPayload::ToolCall(tc) => {
                assert_eq!(tc.tool, "shell");
                assert!(tc.path.is_none());
                assert_eq!(tc.args.as_ref().unwrap()["command"], "ls -la");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        // Item should be tracked.
        assert!(map.contains_key("item-1:command_execution"));
    }

    #[test]
    fn item_started_file_change_emits_tool_call() {
        let mut map = HashMap::new();
        let line = r#"{"type":"item.started","item":{"type":"file_change","id":"item-2","changes":[{"path":"src/main.rs","diff":"-old\n+new"}]}}"#;
        let events = process_line(line, &mut map, 1711100401.0);
        assert_eq!(events.len(), 1);

        match &events[0].payload {
            EventPayload::ToolCall(tc) => {
                assert_eq!(tc.tool, "file_edit");
                assert_eq!(tc.path, Some("src/main.rs".to_owned()));
                assert!(tc.args.is_some());
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn item_started_mcp_tool_call_emits_tool_call() {
        let mut map = HashMap::new();
        let line = r#"{"type":"item.started","item":{"type":"mcp_tool_call","id":"item-3","name":"filesystem.read","arguments":{"path":"/tmp/foo"}}}"#;
        let events = process_line(line, &mut map, 1711100402.0);
        assert_eq!(events.len(), 1);

        match &events[0].payload {
            EventPayload::ToolCall(tc) => {
                assert_eq!(tc.tool, "filesystem.read");
                assert_eq!(tc.args.as_ref().unwrap()["path"], "/tmp/foo");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn item_started_agent_message_produces_no_events() {
        let mut map = HashMap::new();
        let line = r#"{"type":"item.started","item":{"type":"agent_message","id":"item-4","content":"thinking..."}}"#;
        let events = process_line(line, &mut map, 1711100403.0);
        assert!(events.is_empty());
    }

    // ── process_line: item.completed command_execution ───────────────────────

    #[test]
    fn item_completed_command_execution_success() {
        let mut map = HashMap::new();
        map.insert(
            "item-5:command_execution".to_owned(),
            PendingItem {
                tool: "shell".to_owned(),
            },
        );
        let line = r#"{"type":"item.completed","item":{"type":"command_execution","id":"item-5","command":"ls","exit_code":0,"output":"file1.txt\nfile2.txt\n"}}"#;
        let events = process_line(line, &mut map, 1711100404.0);
        assert_eq!(events.len(), 1);

        match &events[0].payload {
            EventPayload::ToolResult(tr) => {
                assert_eq!(tr.tool, "shell");
                assert!(tr.success);
                assert_eq!(tr.output, Some("file1.txt\nfile2.txt".to_owned()));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
        // Item should be removed from map.
        assert!(!map.contains_key("item-5:command_execution"));
    }

    #[test]
    fn item_completed_command_execution_failure() {
        let mut map = HashMap::new();
        map.insert(
            "item-6:command_execution".to_owned(),
            PendingItem {
                tool: "shell".to_owned(),
            },
        );
        let line = r#"{"type":"item.completed","item":{"type":"command_execution","id":"item-6","command":"false","exit_code":1,"output":""}}"#;
        let events = process_line(line, &mut map, 1711100405.0);

        match &events[0].payload {
            EventPayload::ToolResult(tr) => {
                assert_eq!(tr.tool, "shell");
                assert!(!tr.success);
                assert!(tr.output.is_none());
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    // ── process_line: item.completed file_change ─────────────────────────────

    #[test]
    fn item_completed_file_change() {
        let mut map = HashMap::new();
        map.insert(
            "item-7:file_change".to_owned(),
            PendingItem {
                tool: "file_edit".to_owned(),
            },
        );
        let line = r#"{"type":"item.completed","item":{"type":"file_change","id":"item-7","changes":[{"path":"src/main.rs","diff":"-old\n+new"},{"path":"src/lib.rs","diff":"-x\n+y"}]}}"#;
        let events = process_line(line, &mut map, 1711100406.0);

        match &events[0].payload {
            EventPayload::ToolResult(tr) => {
                assert_eq!(tr.tool, "file_edit");
                assert!(tr.success);
                assert_eq!(tr.output, Some("src/main.rs, src/lib.rs".to_owned()));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    // ── process_line: item.completed mcp_tool_call ───────────────────────────

    #[test]
    fn item_completed_mcp_tool_call_success() {
        let mut map = HashMap::new();
        map.insert(
            "item-8:mcp_tool_call".to_owned(),
            PendingItem {
                tool: "fs.read".to_owned(),
            },
        );
        let line = r#"{"type":"item.completed","item":{"type":"mcp_tool_call","id":"item-8","name":"fs.read","arguments":{"path":"/tmp/foo"},"output":"file contents here"}}"#;
        let events = process_line(line, &mut map, 1711100407.0);

        match &events[0].payload {
            EventPayload::ToolResult(tr) => {
                assert_eq!(tr.tool, "fs.read");
                assert!(tr.success);
                assert_eq!(tr.output, Some("file contents here".to_owned()));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn item_completed_mcp_tool_call_error() {
        let mut map = HashMap::new();
        map.insert(
            "item-9:mcp_tool_call".to_owned(),
            PendingItem {
                tool: "fs.read".to_owned(),
            },
        );
        let line = r#"{"type":"item.completed","item":{"type":"mcp_tool_call","id":"item-9","name":"fs.read","arguments":{"path":"/tmp/foo"},"error":"file not found"}}"#;
        let events = process_line(line, &mut map, 1711100408.0);

        match &events[0].payload {
            EventPayload::ToolResult(tr) => {
                assert_eq!(tr.tool, "fs.read");
                assert!(!tr.success);
                assert_eq!(tr.output, Some("file not found".to_owned()));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    // ── process_line: item.completed agent_message ───────────────────────────

    #[test]
    fn item_completed_agent_message_emits_agent_message() {
        let mut map = HashMap::new();
        let line = r#"{"type":"item.completed","item":{"type":"agent_message","id":"item-10","content":"I've updated the auth flow."}}"#;
        let events = process_line(line, &mut map, 1711100409.0);
        assert_eq!(events.len(), 1);

        match &events[0].payload {
            EventPayload::AgentMessage(am) => {
                assert_eq!(am.role, MessageRole::Assistant);
                assert_eq!(am.content, "I've updated the auth flow.");
            }
            other => panic!("expected AgentMessage, got {other:?}"),
        }
    }

    #[test]
    fn item_completed_agent_message_empty_content_produces_no_events() {
        let mut map = HashMap::new();
        let line = r#"{"type":"item.completed","item":{"type":"agent_message","id":"item-11","content":""}}"#;
        let events = process_line(line, &mut map, 1711100410.0);
        assert!(events.is_empty());
    }

    // ── process_line: item.completed error ───────────────────────────────────

    #[test]
    fn item_completed_error_emits_error() {
        let mut map = HashMap::new();
        let line = r#"{"type":"item.completed","item":{"type":"error","id":"item-12","message":"network timeout","code":"timeout"}}"#;
        let events = process_line(line, &mut map, 1711100411.0);
        assert_eq!(events.len(), 1);

        match &events[0].payload {
            EventPayload::Error(e) => {
                assert_eq!(e.message, "network timeout");
                assert!(e.recoverable);
                assert_eq!(e.code, Some("timeout".to_owned()));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // ── process_line: turn.completed (tokens) ────────────────────────────────

    #[test]
    fn turn_completed_emits_tokens() {
        let mut map = HashMap::new();
        let line = r#"{"type":"turn.completed","usage":{"input_tokens":1200,"output_tokens":450,"cached_input_tokens":800},"model":"o3-pro"}"#;
        let events = process_line(line, &mut map, 1711100412.0);
        assert_eq!(events.len(), 1);

        match &events[0].payload {
            EventPayload::Tokens(t) => {
                assert_eq!(t.input, 1200);
                assert_eq!(t.output, 450);
                assert_eq!(t.model, "o3-pro");
                assert_eq!(t.cache_read, Some(800));
                assert!(t.cache_write.is_none());
            }
            other => panic!("expected Tokens, got {other:?}"),
        }
    }

    #[test]
    fn turn_completed_with_no_usage_produces_no_events() {
        let mut map = HashMap::new();
        let line = r#"{"type":"turn.completed","model":"o3-pro"}"#;
        let events = process_line(line, &mut map, 1711100413.0);
        assert!(events.is_empty());
    }

    // ── process_line: turn.failed ────────────────────────────────────────────

    #[test]
    fn turn_failed_emits_error() {
        let mut map = HashMap::new();
        let line = r#"{"type":"turn.failed","message":"rate limit exceeded","code":"rate_limit"}"#;
        let events = process_line(line, &mut map, 1711100414.0);
        assert_eq!(events.len(), 1);

        match &events[0].payload {
            EventPayload::Error(e) => {
                assert_eq!(e.message, "rate limit exceeded");
                assert!(!e.recoverable);
                assert_eq!(e.code, Some("rate_limit".to_owned()));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // ── process_line: top-level error ────────────────────────────────────────

    #[test]
    fn top_level_error_emits_error() {
        let mut map = HashMap::new();
        let line = r#"{"type":"error","message":"authentication failed","code":"auth_error"}"#;
        let events = process_line(line, &mut map, 1711100415.0);
        assert_eq!(events.len(), 1);

        match &events[0].payload {
            EventPayload::Error(e) => {
                assert_eq!(e.message, "authentication failed");
                assert!(!e.recoverable);
                assert_eq!(e.code, Some("auth_error".to_owned()));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // ── process_line: thread.started / turn.started / item.updated ───────────

    #[test]
    fn thread_started_produces_no_events() {
        let mut map = HashMap::new();
        let line =
            r#"{"type":"thread.started","model":"o3-pro","instructions":"You are helpful."}"#;
        let events = process_line(line, &mut map, 1711100416.0);
        assert!(events.is_empty());
    }

    #[test]
    fn turn_started_produces_no_events() {
        let mut map = HashMap::new();
        let line = r#"{"type":"turn.started","turn_id":"t1"}"#;
        let events = process_line(line, &mut map, 1711100417.0);
        assert!(events.is_empty());
    }

    #[test]
    fn item_updated_produces_no_events() {
        let mut map = HashMap::new();
        let line = r#"{"type":"item.updated","item":{"type":"command_execution","id":"item-20","partial_output":"so far..."}}"#;
        let events = process_line(line, &mut map, 1711100418.0);
        assert!(events.is_empty());
    }

    // ── graceful degradation ─────────────────────────────────────────────────

    #[test]
    fn malformed_json_skipped() {
        let mut map = HashMap::new();
        let events = process_line("not { valid json", &mut map, 1711100419.0);
        assert!(events.is_empty());
    }

    #[test]
    fn unknown_type_skipped() {
        let mut map = HashMap::new();
        let line = r#"{"type":"session.heartbeat","ts":1711100420.0}"#;
        let events = process_line(line, &mut map, 1711100420.0);
        assert!(events.is_empty());
    }

    #[test]
    fn missing_type_field_skipped() {
        let mut map = HashMap::new();
        let events = process_line(r#"{"foo":"bar"}"#, &mut map, 1711100421.0);
        assert!(events.is_empty());
    }

    // ── schema_version is always 1 ───────────────────────────────────────────

    #[test]
    fn all_events_have_schema_version_1() {
        let mut map = HashMap::new();

        // item.started → ToolCall
        let line = r#"{"type":"item.started","item":{"type":"command_execution","id":"sv-1","command":"echo hi"}}"#;
        let events = process_line(line, &mut map, 1711100422.0);
        for event in &events {
            assert_eq!(event.schema_version, 1);
        }

        // item.completed → ToolResult
        let line = r#"{"type":"item.completed","item":{"type":"command_execution","id":"sv-1","command":"echo hi","exit_code":0,"output":"hi\n"}}"#;
        let events = process_line(line, &mut map, 1711100422.0);
        for event in &events {
            assert_eq!(event.schema_version, 1);
        }

        // item.completed → AgentMessage
        let line = r#"{"type":"item.completed","item":{"type":"agent_message","id":"sv-2","content":"hello"}}"#;
        let events = process_line(line, &mut map, 1711100422.0);
        for event in &events {
            assert_eq!(event.schema_version, 1);
        }

        // turn.completed → Tokens
        let line = r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5},"model":"o3-pro"}"#;
        let events = process_line(line, &mut map, 1711100422.0);
        for event in &events {
            assert_eq!(event.schema_version, 1);
        }
    }

    // ── round-trip through AgentEvent ────────────────────────────────────────

    #[test]
    fn emitted_events_are_valid_agent_events() {
        use needle::agent_event::AgentEvent;

        let mut map = HashMap::new();

        let lines = [
            r#"{"type":"item.started","item":{"type":"command_execution","id":"rt-1","command":"cat /etc/hosts"}}"#,
            r#"{"type":"item.completed","item":{"type":"command_execution","id":"rt-1","command":"cat /etc/hosts","exit_code":0,"output":"127.0.0.1 localhost\n"}}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","id":"rt-2","content":"Done checking hosts."}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":500,"output_tokens":100,"cached_input_tokens":200},"model":"o3-pro"}"#,
        ];

        for line in &lines {
            let events = process_line(line, &mut map, 1711100430.0);
            for event in &events {
                let json = serde_json::to_string(event).unwrap();
                let _reparsed: AgentEvent = serde_json::from_str(&json).unwrap();
            }
        }
    }

    // ── missing item field in item.started/completed ─────────────────────────

    #[test]
    fn item_started_without_item_field_skipped() {
        let mut map = HashMap::new();
        let line = r#"{"type":"item.started","foo":"bar"}"#;
        let events = process_line(line, &mut map, 1711100423.0);
        assert!(events.is_empty());
    }

    #[test]
    fn item_completed_without_item_field_skipped() {
        let mut map = HashMap::new();
        let line = r#"{"type":"item.completed","foo":"bar"}"#;
        let events = process_line(line, &mut map, 1711100424.0);
        assert!(events.is_empty());
    }

    // ── tool result with no pending item (orphaned completed) ────────────────

    #[test]
    fn item_completed_without_prior_started_uses_type_as_tool() {
        let mut map = HashMap::new();
        let line = r#"{"type":"item.completed","item":{"type":"command_execution","id":"orphan-1","command":"ls","exit_code":0,"output":"file.txt\n"}}"#;
        let events = process_line(line, &mut map, 1711100425.0);

        match &events[0].payload {
            EventPayload::ToolResult(tr) => {
                // Falls back to item type as tool name.
                assert_eq!(tr.tool, "command_execution");
                assert!(tr.success);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }
}
