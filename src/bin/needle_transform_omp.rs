//! `needle-transform-omp` — output transform for the oh-my-pi (`omp`) CLI.
//!
//! Reads the JSONL stream produced by `omp -p --mode json` on stdin and emits
//! the normalized agent event JSONL schema (v1) on stdout.
//!
//! # omp event format
//!
//! Each line is a JSON object with a `type` discriminant at the top level (no
//! shared envelope — timestamps live inside `message`, not beside `type`).
//! Observed types, verified against omp 18.1.14:
//!
//! - `session` / `agent_start` / `agent_end` / `turn_start` — lifecycle; dropped.
//! - `message_start` — a message begins, with usage still zeroed; dropped.
//! - `message_update` — streaming deltas. By far the bulk of the stream (506 of
//!   524 lines in the reference session); dropped, since the completed message
//!   arrives in full at `message_end`.
//! - `message_end` — a completed message: `role`, `content[]`, `model`,
//!   `provider`, and a populated `usage`.
//! - `tool_execution_start` / `_update` / `_end` — a tool call and its result.
//! - `turn_end` — carries the SAME message object as the preceding
//!   `message_end`. Dropped, because handling both would double-count every
//!   `tokens` event.
//!
//! # Two ways omp reports a tool call, and why only one is used
//!
//! A call appears both as `tool_execution_start`/`_end` events *and* as a
//! `toolCall` part inside the assistant's `message_end` content. Only the
//! dedicated events are translated: they carry `isError` and the structured
//! result, which the inline part does not. `toolCall` parts are therefore
//! skipped when walking message content, or every call would emit twice.
//!
//! # Token accounting differs from opencode — do not copy that logic here
//!
//! omp's `usage.output` **already includes** `usage.reasoningTokens`: the
//! reference session reports input 15792, output 1022, reasoningTokens 960 and
//! totalTokens 16814, and 15792 + 1022 == 16814 exactly. Adding reasoning to
//! output — which is correct for `needle-transform-opencode`, where opencode
//! reports them as disjoint addends — would inflate output here by roughly 2x
//! on a reasoning-heavy turn. So reasoning is deliberately *not* added.
//!
//! `thinking` content parts are dropped for the same reason they are in the
//! opencode transform: the v1 schema has no reasoning event, and folding
//! private chain-of-thought into `agent_message` would make it
//! indistinguishable from the agent's actual reply.
//!
//! Malformed lines are skipped with a warning on stderr; the process never
//! crashes on bad input.

use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use needle::agent_event::{
    AgentEvent, AgentMessageEvent, ErrorEvent, EventPayload, MessageRole, TokensEvent,
    ToolCallEvent, ToolResultEvent, SCHEMA_VERSION,
};
use serde_json::Value;

/// Cap on the `output` summary carried by a `tool_result`.
///
/// omp inlines full file contents and command output into the tool result, so
/// an unbounded copy would push tens of kilobytes per call downstream.
const MAX_OUTPUT_CHARS: usize = 2000;

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

fn now_ts() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Truncate to at most `max` **characters** — the input is arbitrary UTF-8 and
/// slicing by byte offset would panic mid-codepoint.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars().take(max).collect()
    }
}

fn event(ts: f64, payload: EventPayload) -> AgentEvent {
    AgentEvent {
        schema_version: SCHEMA_VERSION,
        ts,
        payload,
    }
}

/// `message.timestamp` is epoch milliseconds; absent on tool events.
fn message_ts(message: &Value) -> f64 {
    message
        .get("timestamp")
        .and_then(Value::as_f64)
        .map(|ms| ms / 1000.0)
        .unwrap_or_else(now_ts)
}

/// Fully-qualified model name, matching the `provider/model` form the adapter
/// passes to `--model`, so telemetry lines up with what was dispatched.
fn qualified_model(message: &Value) -> String {
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match message.get("provider").and_then(Value::as_str) {
        Some(p) if !p.is_empty() => format!("{p}/{model}"),
        _ => model.to_string(),
    }
}

/// Best-effort file path for a tool call, across omp's tool argument spellings.
fn extract_path(args: &Value) -> Option<String> {
    for key in ["path", "filePath", "file_path", "resolvedPath"] {
        if let Some(p) = args.get(key).and_then(Value::as_str) {
            return Some(p.to_string());
        }
    }
    None
}

/// Flatten omp's `result.content[]` blocks into one plain-text summary.
fn flatten_result(result: &Value) -> Option<String> {
    if let Some(s) = result.as_str() {
        return Some(s.to_string());
    }
    let parts = result.get("content")?.as_array()?;
    let text: Vec<&str> = parts
        .iter()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect();
    if text.is_empty() {
        None
    } else {
        Some(text.join("\n"))
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Line processor
// ──────────────────────────────────────────────────────────────────────────────

/// Parse one line of omp `--mode json` output into normalized events.
pub(crate) fn process_line(line: &str) -> Vec<AgentEvent> {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("needle-transform-omp: skipping malformed JSON ({e}): {line}");
            return vec![];
        }
    };

    match value.get("type").and_then(Value::as_str) {
        Some("tool_execution_start") => {
            let tool = value
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let args = value.get("args").cloned();
            let path = args.as_ref().and_then(extract_path);
            vec![event(
                now_ts(),
                EventPayload::ToolCall(ToolCallEvent { tool, path, args }),
            )]
        }

        Some("tool_execution_end") => {
            let tool = value
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let is_error = value
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let output = value
                .get("result")
                .and_then(flatten_result)
                .map(|s| truncate(&s, MAX_OUTPUT_CHARS));
            vec![event(
                now_ts(),
                EventPayload::ToolResult(ToolResultEvent {
                    tool,
                    success: !is_error,
                    output,
                }),
            )]
        }

        Some("message_end") => {
            let Some(message) = value.get("message") else {
                return vec![];
            };
            // Only the assistant's own turns are translated: `user` echoes the
            // dispatched prompt back, and `toolResult` duplicates what
            // tool_execution_end already reported.
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                return vec![];
            }

            let ts = message_ts(message);
            let mut events = Vec::new();

            if let Some(parts) = message.get("content").and_then(Value::as_array) {
                for part in parts {
                    // `thinking` is private reasoning; `toolCall` is already
                    // covered by the tool_execution_* events. See module docs.
                    if part.get("type").and_then(Value::as_str) != Some("text") {
                        continue;
                    }
                    let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                    if text.trim().is_empty() {
                        continue;
                    }
                    events.push(event(
                        ts,
                        EventPayload::AgentMessage(AgentMessageEvent {
                            role: MessageRole::Assistant,
                            content: text.to_string(),
                        }),
                    ));
                }
            }

            if let Some(usage) = message.get("usage") {
                let get = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
                let input = get("input");
                // NOT input+output+reasoning: omp's `output` already contains
                // `reasoningTokens`. See module docs.
                let output = get("output");
                if input > 0 || output > 0 {
                    events.push(event(
                        ts,
                        EventPayload::Tokens(TokensEvent {
                            input,
                            output,
                            model: qualified_model(message),
                            cache_read: usage.get("cacheRead").and_then(Value::as_u64),
                            cache_write: usage.get("cacheWrite").and_then(Value::as_u64),
                        }),
                    ));
                }
            }

            events
        }

        Some("error") => {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| value.pointer("/error/message").and_then(Value::as_str))
                .unwrap_or("unknown omp error")
                .to_string();
            vec![event(
                now_ts(),
                EventPayload::Error(ErrorEvent {
                    message,
                    recoverable: false,
                    code: None,
                }),
            )]
        }

        // session, agent_start/end, turn_start/end, message_start,
        // message_update, tool_execution_update, and anything omp adds later.
        _ => vec![],
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        for ev in process_line(&line) {
            match serde_json::to_string(&ev) {
                Ok(json) => writeln!(out, "{json}")?,
                Err(e) => eprintln!("needle-transform-omp: failed to serialize event: {e}"),
            }
        }
        out.flush()?;
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_execution_start_becomes_tool_call() {
        let line = r#"{"type":"tool_execution_start","toolCallId":"c1","toolName":"write","args":{"path":"omp-check.txt","content":"WIRED"},"intent":"Create it"}"#;
        let events = process_line(line);
        assert_eq!(events.len(), 1);
        match &events[0].payload {
            EventPayload::ToolCall(c) => {
                assert_eq!(c.tool, "write");
                assert_eq!(c.path.as_deref(), Some("omp-check.txt"));
                assert!(c.args.is_some());
            }
            other => panic!("expected tool_call, got {other:?}"),
        }
    }

    #[test]
    fn tool_execution_end_flattens_result_content() {
        let line = r#"{"type":"tool_execution_end","toolCallId":"c1","toolName":"write","result":{"content":[{"type":"text","text":"Successfully wrote 5 bytes"}],"details":{}},"isError":false}"#;
        let events = process_line(line);
        assert_eq!(events.len(), 1);
        match &events[0].payload {
            EventPayload::ToolResult(r) => {
                assert_eq!(r.tool, "write");
                assert!(r.success);
                assert_eq!(r.output.as_deref(), Some("Successfully wrote 5 bytes"));
            }
            other => panic!("expected tool_result, got {other:?}"),
        }
    }

    #[test]
    fn errored_tool_reports_failure() {
        let line = r#"{"type":"tool_execution_end","toolName":"bash","result":{"content":[{"type":"text","text":"exit 1"}]},"isError":true}"#;
        match &process_line(line)[0].payload {
            EventPayload::ToolResult(r) => assert!(!r.success),
            other => panic!("expected tool_result, got {other:?}"),
        }
    }

    #[test]
    fn message_end_emits_text_and_tokens_but_not_thinking() {
        let line = r#"{"type":"message_end","message":{"role":"assistant","timestamp":1788807596469,"model":"gpt-5-mini","provider":"ardenone-openai","content":[{"type":"thinking","thinking":"secret reasoning"},{"type":"toolCall","name":"write","arguments":{}},{"type":"text","text":"Wrote the file."}],"usage":{"input":15792,"output":1022,"cacheRead":0,"cacheWrite":0,"totalTokens":16814,"reasoningTokens":960}}}"#;
        let events = process_line(line);
        assert_eq!(
            events.len(),
            2,
            "one agent_message + one tokens: {events:?}"
        );

        match &events[0].payload {
            EventPayload::AgentMessage(m) => {
                assert_eq!(m.content, "Wrote the file.");
                assert!(!m.content.contains("secret"), "thinking must not leak");
            }
            other => panic!("expected agent_message, got {other:?}"),
        }
        match &events[1].payload {
            EventPayload::Tokens(t) => {
                assert_eq!(t.input, 15792);
                // 1022, NOT 1022+960 — omp's output already includes reasoning.
                assert_eq!(t.output, 1022, "reasoning must not be added to output");
                assert_eq!(t.model, "ardenone-openai/gpt-5-mini");
            }
            other => panic!("expected tokens, got {other:?}"),
        }
        assert_eq!(events[1].ts, 1788807596.469);
    }

    #[test]
    fn turn_end_is_dropped_so_tokens_are_not_double_counted() {
        // turn_end carries the same message object as the preceding message_end.
        let line = r#"{"type":"turn_end","message":{"role":"assistant","model":"gpt-5-mini","usage":{"input":10,"output":2,"totalTokens":12}},"toolResults":[]}"#;
        assert!(process_line(line).is_empty());
    }

    #[test]
    fn non_assistant_messages_are_dropped() {
        let user = r#"{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"the dispatched prompt"}]}}"#;
        assert!(process_line(user).is_empty());
        let tr = r#"{"type":"message_end","message":{"role":"toolResult","content":[{"type":"text","text":"result"}]}}"#;
        assert!(process_line(tr).is_empty());
    }

    #[test]
    fn zero_usage_message_emits_no_tokens_event() {
        // message_start-shaped usage (all zeros) must not produce a tokens event.
        let line = r#"{"type":"message_end","message":{"role":"assistant","model":"m","usage":{"input":0,"output":0,"totalTokens":0},"content":[]}}"#;
        assert!(process_line(line).is_empty());
    }

    #[test]
    fn streaming_and_lifecycle_events_are_dropped() {
        for line in [
            r#"{"type":"message_update","assistantMessageEvent":{}}"#,
            r#"{"type":"session","version":3,"id":"x","cwd":"/tmp"}"#,
            r#"{"type":"agent_start"}"#,
            r#"{"type":"turn_start"}"#,
            r#"{"type":"tool_execution_update","toolName":"write","partialResult":{}}"#,
        ] {
            assert!(process_line(line).is_empty(), "should drop: {line}");
        }
    }

    #[test]
    fn model_without_provider_is_reported_bare() {
        let line = r#"{"type":"message_end","message":{"role":"assistant","model":"solo","usage":{"input":1,"output":1},"content":[]}}"#;
        match &process_line(line)[0].payload {
            EventPayload::Tokens(t) => assert_eq!(t.model, "solo"),
            other => panic!("expected tokens, got {other:?}"),
        }
    }

    #[test]
    fn malformed_line_does_not_panic() {
        assert!(process_line("not json at all").is_empty());
    }

    #[test]
    fn truncate_is_codepoint_safe() {
        assert_eq!(truncate("→→→→→", 2), "→→");
        assert_eq!(truncate("→→→→→", 99), "→→→→→");
    }
}
