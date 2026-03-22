//! `needle-transform-claude` — reference output transform for Claude Code.
//!
//! Reads Claude Code's `--output-format json` JSONL stream from stdin and
//! emits the normalized agent event JSONL schema (v1) to stdout.
//!
//! Each line of Claude's output is a JSON object.  Malformed or unrecognised
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

/// Extract the canonical file-path argument from a tool's `input` object.
///
/// - `Read`, `Edit`, `Write` → `input.file_path`
/// - `Grep`, `Glob` → `input.path`
/// - All other tools → `None`
pub fn extract_path(tool: &str, input: &Value) -> Option<String> {
    match tool {
        "Read" | "Edit" | "Write" => input.get("file_path")?.as_str().map(str::to_owned),
        "Grep" | "Glob" => input.get("path")?.as_str().map(str::to_owned),
        _ => None,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Line processor (pub for unit tests)
// ──────────────────────────────────────────────────────────────────────────────

/// Parse one line of Claude's `--output-format json` output and return any
/// normalized [`AgentEvent`]s it produces.
///
/// `tool_name_map` is updated whenever a `tool_use` block is seen so that the
/// corresponding `tool_result` (which arrives later) can look up the tool name.
pub fn process_line(
    line: &str,
    tool_name_map: &mut HashMap<String, String>,
    ts: f64,
) -> Vec<AgentEvent> {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("needle-transform-claude: skipping malformed JSON ({e}): {line}");
            return vec![];
        }
    };

    match value.get("type").and_then(|v| v.as_str()) {
        Some("assistant") => process_assistant(&value, tool_name_map, ts),
        Some("user") => process_user(&value, tool_name_map, ts),
        Some("result") => process_result(&value, ts),
        Some("error") => process_error(&value, ts),
        Some(_) => vec![], // system, etc. — skip silently
        None => {
            eprintln!("needle-transform-claude: skipping line without `type` field");
            vec![]
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Per-type handlers
// ──────────────────────────────────────────────────────────────────────────────

fn process_assistant(
    value: &Value,
    tool_name_map: &mut HashMap<String, String>,
    ts: f64,
) -> Vec<AgentEvent> {
    let mut events: Vec<AgentEvent> = vec![];

    let message = match value.get("message") {
        Some(m) => m,
        None => return events,
    };

    let model = message
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_owned();

    // Process content blocks in order.
    if let Some(blocks) = message.get("content").and_then(|c| c.as_array()) {
        for block in blocks {
            let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match block_type {
                "tool_use" => {
                    let tool = block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown")
                        .to_owned();
                    let id = block
                        .get("id")
                        .and_then(|id| id.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let input = block.get("input").cloned();

                    // Store mapping so the matching tool_result can find the name.
                    if !id.is_empty() {
                        tool_name_map.insert(id, tool.clone());
                    }

                    let path = input.as_ref().and_then(|inp| extract_path(&tool, inp));

                    events.push(AgentEvent {
                        schema_version: SCHEMA_VERSION,
                        ts,
                        payload: EventPayload::ToolCall(ToolCallEvent {
                            tool,
                            path,
                            args: input,
                        }),
                    });
                }
                "text" => {
                    let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    if !text.is_empty() {
                        events.push(AgentEvent {
                            schema_version: SCHEMA_VERSION,
                            ts,
                            payload: EventPayload::AgentMessage(AgentMessageEvent {
                                role: MessageRole::Assistant,
                                content: text.to_owned(),
                            }),
                        });
                    }
                }
                _ => {} // skip unknown block types
            }
        }
    }

    // Emit token usage from this message turn.
    if let Some(usage) = message.get("usage") {
        let input_tokens = usage
            .get("input_tokens")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|t| t.as_u64());
        let cache_write = usage
            .get("cache_creation_input_tokens")
            .and_then(|t| t.as_u64());

        if input_tokens > 0 || output_tokens > 0 {
            events.push(AgentEvent {
                schema_version: SCHEMA_VERSION,
                ts,
                payload: EventPayload::Tokens(TokensEvent {
                    input: input_tokens,
                    output: output_tokens,
                    model,
                    cache_read: cache_read.filter(|&v| v > 0),
                    cache_write: cache_write.filter(|&v| v > 0),
                }),
            });
        }
    }

    events
}

fn process_user(
    value: &Value,
    tool_name_map: &HashMap<String, String>,
    ts: f64,
) -> Vec<AgentEvent> {
    let mut events: Vec<AgentEvent> = vec![];

    let message = match value.get("message") {
        Some(m) => m,
        None => return events,
    };

    let blocks = match message.get("content").and_then(|c| c.as_array()) {
        Some(b) => b,
        None => return events,
    };

    for block in blocks {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
            continue;
        }

        let tool_use_id = block
            .get("tool_use_id")
            .and_then(|id| id.as_str())
            .unwrap_or("");
        let tool = tool_name_map
            .get(tool_use_id)
            .cloned()
            .unwrap_or_else(|| "unknown".to_owned());
        let is_error = block
            .get("is_error")
            .and_then(|e| e.as_bool())
            .unwrap_or(false);
        let output = extract_tool_result_output(block);

        events.push(AgentEvent {
            schema_version: SCHEMA_VERSION,
            ts,
            payload: EventPayload::ToolResult(ToolResultEvent {
                tool,
                success: !is_error,
                output,
            }),
        });
    }

    events
}

/// Extract a short human-readable summary from a tool result content block.
/// Truncates at 500 characters.
fn extract_tool_result_output(block: &Value) -> Option<String> {
    let content = block.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };

    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(500).collect())
    }
}

fn process_result(value: &Value, ts: f64) -> Vec<AgentEvent> {
    let subtype = value.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
    let is_error = value
        .get("is_error")
        .and_then(|e| e.as_bool())
        .unwrap_or(false);

    if !is_error && !subtype.starts_with("error") {
        return vec![];
    }

    let message = value
        .get("error_message")
        .and_then(|m| m.as_str())
        .unwrap_or(subtype)
        .to_owned();

    let code = if subtype.is_empty() {
        None
    } else {
        Some(subtype.to_owned())
    };

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

fn process_error(value: &Value, ts: f64) -> Vec<AgentEvent> {
    let message = value
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .or_else(|| value.get("message").and_then(|m| m.as_str()))
        .unwrap_or("unknown error")
        .to_owned();

    let code = value
        .get("error")
        .and_then(|e| e.get("code"))
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

    // tool_use_id → tool_name, populated as tool_use blocks are seen.
    let mut tool_name_map: HashMap<String, String> = HashMap::new();

    for line_result in stdin.lock().lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                eprintln!("needle-transform-claude: stdin read error: {e}");
                break;
            }
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let ts = now_ts();
        let events = process_line(line, &mut tool_name_map, ts);

        for event in &events {
            match serde_json::to_string(event) {
                Ok(json) => {
                    if writeln!(out, "{json}").is_err() {
                        // stdout closed (downstream terminated) — exit cleanly.
                        return;
                    }
                }
                Err(e) => {
                    eprintln!("needle-transform-claude: serialization error (bug): {e}");
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

    // ── extract_path ──────────────────────────────────────────────────────────

    #[test]
    fn extract_path_read() {
        let input = serde_json::json!({"file_path": "src/main.rs"});
        assert_eq!(extract_path("Read", &input), Some("src/main.rs".to_owned()));
    }

    #[test]
    fn extract_path_edit() {
        let input =
            serde_json::json!({"file_path": "src/auth.ts", "old_string": "x", "new_string": "y"});
        assert_eq!(extract_path("Edit", &input), Some("src/auth.ts".to_owned()));
    }

    #[test]
    fn extract_path_write() {
        let input = serde_json::json!({"file_path": "out.txt", "content": "hello"});
        assert_eq!(extract_path("Write", &input), Some("out.txt".to_owned()));
    }

    #[test]
    fn extract_path_grep() {
        let input = serde_json::json!({"pattern": "fn main", "path": "src/"});
        assert_eq!(extract_path("Grep", &input), Some("src/".to_owned()));
    }

    #[test]
    fn extract_path_glob() {
        let input = serde_json::json!({"pattern": "**/*.rs", "path": "src/"});
        assert_eq!(extract_path("Glob", &input), Some("src/".to_owned()));
    }

    #[test]
    fn extract_path_bash_is_none() {
        let input = serde_json::json!({"command": "ls -la"});
        assert_eq!(extract_path("Bash", &input), None);
    }

    // ── process_line: assistant with tool_use ─────────────────────────────────

    #[test]
    fn assistant_tool_use_emits_tool_call_and_tokens() {
        let mut map = HashMap::new();
        let line = r#"{"type":"assistant","message":{"id":"msg_01","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_01","name":"Read","input":{"file_path":"src/main.rs"}}],"model":"claude-sonnet-4-6","stop_reason":"tool_use","usage":{"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;

        let events = process_line(line, &mut map, 1711100400.0);
        assert_eq!(events.len(), 2, "expected tool_call + tokens");

        match &events[0].payload {
            EventPayload::ToolCall(tc) => {
                assert_eq!(tc.tool, "Read");
                assert_eq!(tc.path, Some("src/main.rs".to_owned()));
                assert_eq!(tc.args.as_ref().unwrap()["file_path"], "src/main.rs");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        match &events[1].payload {
            EventPayload::Tokens(t) => {
                assert_eq!(t.input, 100);
                assert_eq!(t.output, 20);
                assert_eq!(t.model, "claude-sonnet-4-6");
                assert!(t.cache_read.is_none());
            }
            other => panic!("expected Tokens, got {other:?}"),
        }
        // tool_use_id must be stored for subsequent tool_result lookup.
        assert_eq!(map.get("toolu_01"), Some(&"Read".to_owned()));
    }

    #[test]
    fn assistant_tool_use_with_cache_tokens() {
        let mut map = HashMap::new();
        let line = r#"{"type":"assistant","message":{"id":"msg_02","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_02","name":"Grep","input":{"pattern":"fn main","path":"src/"}}],"model":"claude-sonnet-4-6","stop_reason":"tool_use","usage":{"input_tokens":200,"output_tokens":30,"cache_creation_input_tokens":50,"cache_read_input_tokens":800}}}"#;

        let events = process_line(line, &mut map, 1711100401.0);
        match &events[1].payload {
            EventPayload::Tokens(t) => {
                assert_eq!(t.cache_read, Some(800));
                assert_eq!(t.cache_write, Some(50));
            }
            other => panic!("expected Tokens, got {other:?}"),
        }
        // Grep path extraction
        match &events[0].payload {
            EventPayload::ToolCall(tc) => {
                assert_eq!(tc.tool, "Grep");
                assert_eq!(tc.path, Some("src/".to_owned()));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    // ── process_line: user with tool_result ───────────────────────────────────

    #[test]
    fn user_tool_result_success() {
        let mut map = HashMap::new();
        map.insert("toolu_01".to_owned(), "Read".to_owned());

        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"fn main() {}\n","is_error":false}]}}"#;
        let events = process_line(line, &mut map, 1711100402.0);

        assert_eq!(events.len(), 1);
        match &events[0].payload {
            EventPayload::ToolResult(tr) => {
                assert_eq!(tr.tool, "Read");
                assert!(tr.success);
                assert_eq!(tr.output, Some("fn main() {}".to_owned()));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn user_tool_result_error() {
        let mut map = HashMap::new();
        map.insert("toolu_02".to_owned(), "Edit".to_owned());

        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_02","content":"file not found","is_error":true}]}}"#;
        let events = process_line(line, &mut map, 1711100403.0);

        match &events[0].payload {
            EventPayload::ToolResult(tr) => {
                assert!(!tr.success);
                assert_eq!(tr.output, Some("file not found".to_owned()));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn user_tool_result_unknown_id_falls_back_to_unknown() {
        let map = HashMap::new();
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_missing","content":"ok","is_error":false}]}}"#;
        let events = process_line(line, &mut { map }, 1711100404.0);

        match &events[0].payload {
            EventPayload::ToolResult(tr) => assert_eq!(tr.tool, "unknown"),
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    // ── process_line: assistant text block ────────────────────────────────────

    #[test]
    fn assistant_text_emits_agent_message_and_tokens() {
        let mut map = HashMap::new();
        let line = r#"{"type":"assistant","message":{"id":"msg_03","type":"message","role":"assistant","content":[{"type":"text","text":"Done."}],"model":"claude-sonnet-4-6","stop_reason":"end_turn","usage":{"input_tokens":300,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;

        let events = process_line(line, &mut map, 1711100405.0);
        assert_eq!(events.len(), 2);

        match &events[0].payload {
            EventPayload::AgentMessage(am) => {
                assert_eq!(am.role, MessageRole::Assistant);
                assert_eq!(am.content, "Done.");
            }
            other => panic!("expected AgentMessage, got {other:?}"),
        }
    }

    // ── process_line: mixed content (text + tool_use) ─────────────────────────

    #[test]
    fn assistant_mixed_content_preserves_order() {
        let mut map = HashMap::new();
        let line = r#"{"type":"assistant","message":{"id":"msg_04","type":"message","role":"assistant","content":[{"type":"text","text":"Let me check."},{"type":"tool_use","id":"toolu_03","name":"Glob","input":{"pattern":"**/*.rs","path":"src/"}}],"model":"claude-sonnet-4-6","stop_reason":"tool_use","usage":{"input_tokens":50,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;

        let events = process_line(line, &mut map, 1711100406.0);
        // Expect: agent_message, tool_call, tokens
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0].payload, EventPayload::AgentMessage(_)));
        assert!(matches!(events[1].payload, EventPayload::ToolCall(_)));
        assert!(matches!(events[2].payload, EventPayload::Tokens(_)));

        match &events[1].payload {
            EventPayload::ToolCall(tc) => {
                assert_eq!(tc.tool, "Glob");
                assert_eq!(tc.path, Some("src/".to_owned()));
            }
            _ => panic!(),
        }
    }

    // ── process_line: result ──────────────────────────────────────────────────

    #[test]
    fn result_success_produces_no_events() {
        let mut map = HashMap::new();
        let line = r#"{"type":"result","subtype":"success","cost_usd":0.001,"is_error":false,"session_id":"s1","num_turns":2}"#;
        let events = process_line(line, &mut map, 1711100407.0);
        assert!(events.is_empty());
    }

    #[test]
    fn result_error_max_turns() {
        let mut map = HashMap::new();
        let line = r#"{"type":"result","subtype":"error_max_turns","cost_usd":0.05,"is_error":true,"session_id":"s1","num_turns":10}"#;
        let events = process_line(line, &mut map, 1711100408.0);
        assert_eq!(events.len(), 1);
        match &events[0].payload {
            EventPayload::Error(e) => {
                assert!(!e.recoverable);
                assert_eq!(e.code, Some("error_max_turns".to_owned()));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // ── process_line: top-level error type ───────────────────────────────────

    #[test]
    fn top_level_error_type() {
        let mut map = HashMap::new();
        let line =
            r#"{"type":"error","error":{"message":"rate limit exceeded","code":"rate_limit"}}"#;
        let events = process_line(line, &mut map, 1711100409.0);
        assert_eq!(events.len(), 1);
        match &events[0].payload {
            EventPayload::Error(e) => {
                assert_eq!(e.message, "rate limit exceeded");
                assert_eq!(e.code, Some("rate_limit".to_owned()));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // ── process_line: graceful degradation ───────────────────────────────────

    #[test]
    fn malformed_json_skipped() {
        let mut map = HashMap::new();
        let events = process_line("not { valid json", &mut map, 1711100410.0);
        assert!(events.is_empty());
    }

    #[test]
    fn unknown_type_skipped() {
        let mut map = HashMap::new();
        let line =
            r#"{"type":"system","subtype":"init","session_id":"s1","model":"claude-sonnet-4-6"}"#;
        let events = process_line(line, &mut map, 1711100411.0);
        assert!(events.is_empty());
    }

    #[test]
    fn missing_type_field_skipped() {
        let mut map = HashMap::new();
        let events = process_line(r#"{"foo":"bar"}"#, &mut map, 1711100412.0);
        assert!(events.is_empty());
    }

    // ── schema_version is always 1 ────────────────────────────────────────────

    #[test]
    fn all_events_have_schema_version_1() {
        let mut map = HashMap::new();
        let line = r#"{"type":"assistant","message":{"id":"msg_05","type":"message","role":"assistant","content":[{"type":"text","text":"hi"}],"model":"claude-sonnet-4-6","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
        let events = process_line(line, &mut map, 1711100413.0);
        for event in &events {
            assert_eq!(event.schema_version, 1);
        }
    }

    // ── output is valid JSONL (round-trips through AgentEvent) ────────────────

    #[test]
    fn emitted_events_are_valid_agent_events() {
        use needle::agent_event::AgentEvent;

        let mut map = HashMap::new();
        let line = r#"{"type":"assistant","message":{"id":"msg_06","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_06","name":"Write","input":{"file_path":"out.txt","content":"hello"}}],"model":"claude-sonnet-4-6","stop_reason":"tool_use","usage":{"input_tokens":5,"output_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
        let events = process_line(line, &mut map, 1711100414.0);
        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let _reparsed: AgentEvent = serde_json::from_str(&json).unwrap();
        }
    }
}
