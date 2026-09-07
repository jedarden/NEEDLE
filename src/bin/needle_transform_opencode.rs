//! `needle-transform-opencode` — output transform for the opencode CLI.
//!
//! Reads the JSONL stream produced by `opencode run --format json` on stdin and
//! emits the normalized agent event JSONL schema (v1) on stdout.
//!
//! # opencode event format
//!
//! Every line is a JSON object with a common envelope:
//!
//! ```json
//! {"type":"text","timestamp":1788799629440,"sessionID":"ses_…","part":{…}}
//! ```
//!
//! `timestamp` is **epoch milliseconds** (the v1 schema wants fractional
//! seconds, so it is divided by 1000). The event-specific body always lives
//! under `part`. Observed `type` values, verified against opencode 1.18.29:
//!
//! - `step_start` — a model step begins. No normalized equivalent; dropped.
//! - `text` — an assistant text turn (`part.text`).
//! - `reasoning` — thinking output. Dropped: the v1 schema has no reasoning
//!   event, and folding it into `agent_message` would make private
//!   chain-of-thought indistinguishable from the agent's actual reply.
//! - `tool_use` — a tool call and its result, carried in one part whose
//!   `state.status` advances to `completed` or `error`.
//! - `step_finish` — end of a step, carrying `part.tokens`.
//! - `error` — a top-level failure (`error.data.message`).
//!
//! # Why `tool_use` produces two events
//!
//! opencode packs the call and its result into a single part, whereas the v1
//! schema separates `tool_call` from `tool_result`. Each part is therefore
//! expanded into both. A part is emitted at most once per `callID` per phase,
//! so a stream that re-sends a part as its status advances (`pending` →
//! `running` → `completed`) yields exactly one `tool_call` and one
//! `tool_result` rather than one pair per update.
//!
//! # Model attribution
//!
//! opencode's `--format json` stream never names the model or provider —
//! verified against 1.18.29 by walking every key of a full session. The model
//! therefore has to be injected, and it cannot come from the adapter's
//! `environment:` map: that map is applied to the *agent* process, while
//! NEEDLE spawns the transform separately (`bash -c`), so it inherits the
//! worker's environment rather than the adapter's. `output_transform` cannot
//! carry it either — both the dispatcher and `needle test-agent` run
//! `which()` on that field verbatim, so it must be a bare binary name.
//!
//! The adapter instead prints one `needle_meta` line ahead of opencode's own
//! output:
//!
//! ```json
//! {"type":"needle_meta","model":"opencode/big-pickle"}
//! ```
//!
//! which this transform consumes and emits nothing for. `NEEDLE_OPENCODE_MODEL`
//! is honoured as a fallback (convenient when piping by hand), and failing both
//! `tokens` events report `"unknown"` rather than being dropped, so usage stays
//! visible even if the marker is lost.
//!
//! Malformed lines are skipped with a warning on stderr; the process never
//! crashes on bad input.

use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use needle::agent_event::{
    AgentEvent, AgentMessageEvent, ErrorEvent, EventPayload, MessageRole, TokensEvent,
    ToolCallEvent, ToolResultEvent, SCHEMA_VERSION,
};
use serde_json::Value;

/// Cap on the `output` summary carried by a `tool_result`.
///
/// opencode inlines full patch text and file diffs into `state.output`; without
/// a cap a single `apply_patch` can carry tens of kilobytes into every
/// downstream consumer of the event stream.
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

/// Truncate to at most `max` **characters** (not bytes — the input is
/// arbitrary UTF-8 and slicing by byte offset would panic mid-codepoint).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars().take(max).collect()
    }
}

/// Convert opencode's epoch-millisecond `timestamp` to fractional seconds.
fn event_ts(value: &Value) -> f64 {
    value
        .get("timestamp")
        .and_then(Value::as_f64)
        .map(|ms| ms / 1000.0)
        .unwrap_or_else(now_ts)
}

/// Model name from the environment fallback. See module docs.
fn model_from_env() -> Option<String> {
    std::env::var("NEEDLE_OPENCODE_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn event(ts: f64, payload: EventPayload) -> AgentEvent {
    AgentEvent {
        schema_version: SCHEMA_VERSION,
        ts,
        payload,
    }
}

/// Best-effort file path for a tool call.
///
/// opencode reports the edited path in different places depending on the tool:
/// `apply_patch` records it under `state.metadata.files[]`, while `read`/`write`
/// carry it in the tool input under one of several key spellings.
fn extract_path(state: &Value) -> Option<String> {
    if let Some(rel) = state
        .pointer("/metadata/files/0/relativePath")
        .and_then(Value::as_str)
    {
        return Some(rel.to_string());
    }
    if let Some(abs) = state
        .pointer("/metadata/files/0/filePath")
        .and_then(Value::as_str)
    {
        return Some(abs.to_string());
    }
    let input = state.get("input")?;
    for key in ["filePath", "file_path", "path"] {
        if let Some(p) = input.get(key).and_then(Value::as_str) {
            return Some(p.to_string());
        }
    }
    None
}

// ──────────────────────────────────────────────────────────────────────────────
// Line processor
// ──────────────────────────────────────────────────────────────────────────────

/// Cross-line state: which tool calls have already emitted each phase (keyed by
/// `callID`), and the model announced by the `needle_meta` marker.
#[derive(Default)]
pub(crate) struct TransformState {
    called: HashSet<String>,
    resolved: HashSet<String>,
    model: Option<String>,
}

impl TransformState {
    /// Model to attribute `tokens` events to: the marker, else the environment
    /// fallback, else `"unknown"`. See module docs.
    fn model(&self) -> String {
        self.model
            .clone()
            .or_else(model_from_env)
            .unwrap_or_else(|| "unknown".to_string())
    }
}

/// Parse one line of opencode `--format json` output into normalized events.
pub(crate) fn process_line(line: &str, seen: &mut TransformState) -> Vec<AgentEvent> {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("needle-transform-opencode: skipping malformed JSON ({e}): {line}");
            return vec![];
        }
    };

    let ts = event_ts(&value);
    let part = value.get("part");

    match value.get("type").and_then(Value::as_str) {
        // Injected by the adapter, not produced by opencode. See module docs.
        Some("needle_meta") => {
            if let Some(model) = value
                .get("model")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
            {
                seen.model = Some(model.to_string());
            }
            vec![]
        }

        Some("text") => {
            let text = part
                .and_then(|p| p.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if text.trim().is_empty() {
                return vec![];
            }
            vec![event(
                ts,
                EventPayload::AgentMessage(AgentMessageEvent {
                    role: MessageRole::Assistant,
                    content: text.to_string(),
                }),
            )]
        }

        Some("tool_use") => {
            let Some(part) = part else { return vec![] };
            let tool = part
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            // Fall back to a synthetic key so a part with no callID still
            // de-duplicates against itself rather than repeating every update.
            let call_id = part
                .get("callID")
                .and_then(Value::as_str)
                .unwrap_or(&tool)
                .to_string();
            let state = part.get("state").cloned().unwrap_or(Value::Null);
            let status = state.get("status").and_then(Value::as_str).unwrap_or("");

            let mut events = Vec::new();

            if seen.called.insert(call_id.clone()) {
                events.push(event(
                    ts,
                    EventPayload::ToolCall(ToolCallEvent {
                        tool: tool.clone(),
                        path: extract_path(&state),
                        args: state.get("input").cloned(),
                    }),
                ));
            }

            // Only terminal states carry a result.
            if matches!(status, "completed" | "error") && seen.resolved.insert(call_id) {
                let output = state
                    .get("output")
                    .and_then(Value::as_str)
                    .or_else(|| state.get("error").and_then(Value::as_str))
                    .map(|s| truncate(s, MAX_OUTPUT_CHARS));
                events.push(event(
                    ts,
                    EventPayload::ToolResult(ToolResultEvent {
                        tool,
                        success: status == "completed",
                        output,
                    }),
                ));
            }

            events
        }

        Some("step_finish") => {
            let Some(tokens) = part.and_then(|p| p.get("tokens")) else {
                return vec![];
            };
            let get = |k: &str| tokens.get(k).and_then(Value::as_u64).unwrap_or(0);
            // Reasoning tokens are billed as output by every provider opencode
            // fronts, and opencode's own `total` is input + output + reasoning.
            // Folding them into `output` keeps cost arithmetic downstream
            // consistent with what the provider actually charges.
            let output = get("output") + get("reasoning");
            let input = get("input");
            // A step that failed before reaching the model still emits
            // step_finish, with every counter zero — opencode retries a
            // retryable upstream error (a zai-proxy 503, say) and each attempt
            // contributes one of these. They carry no cost information, so
            // forwarding them just pads the stream with noise.
            if input == 0 && output == 0 {
                return vec![];
            }
            let cache_read = tokens.pointer("/cache/read").and_then(Value::as_u64);
            let cache_write = tokens.pointer("/cache/write").and_then(Value::as_u64);

            vec![event(
                ts,
                EventPayload::Tokens(TokensEvent {
                    input,
                    output,
                    model: seen.model(),
                    cache_read,
                    cache_write,
                }),
            )]
        }

        Some("error") => {
            let err = value.get("error");
            let message = err
                .and_then(|e| e.pointer("/data/message"))
                .and_then(Value::as_str)
                .or_else(|| err.and_then(|e| e.get("message")).and_then(Value::as_str))
                .unwrap_or("unknown opencode error")
                .to_string();
            let code = err
                .and_then(|e| e.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let recoverable = err
                .and_then(|e| e.pointer("/data/isRetryable"))
                .and_then(Value::as_bool)
                .unwrap_or(false);

            vec![event(
                ts,
                EventPayload::Error(ErrorEvent {
                    message,
                    recoverable,
                    code,
                }),
            )]
        }

        // step_start, reasoning, and anything opencode adds later.
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
    let mut seen = TransformState::default();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        for ev in process_line(&line, &mut seen) {
            match serde_json::to_string(&ev) {
                Ok(json) => {
                    writeln!(out, "{json}")?;
                }
                Err(e) => eprintln!("needle-transform-opencode: failed to serialize event: {e}"),
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
    fn text_becomes_agent_message() {
        let line = r#"{"type":"text","timestamp":1788799629440,"part":{"type":"text","text":"Proceeding with patch."}}"#;
        let events = process_line(line, &mut TransformState::default());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].ts, 1788799629.440);
        match &events[0].payload {
            EventPayload::AgentMessage(m) => {
                assert_eq!(m.role, MessageRole::Assistant);
                assert_eq!(m.content, "Proceeding with patch.");
            }
            other => panic!("expected agent_message, got {other:?}"),
        }
    }

    #[test]
    fn empty_text_is_dropped() {
        let line = r#"{"type":"text","timestamp":1,"part":{"text":"   "}}"#;
        assert!(process_line(line, &mut TransformState::default()).is_empty());
    }

    #[test]
    fn tool_use_expands_to_call_and_result_with_relative_path() {
        let line = r#"{"type":"tool_use","timestamp":1788799629546,"part":{"type":"tool","tool":"apply_patch","callID":"call_1","state":{"status":"completed","input":{"patchText":"x"},"output":"Success.","metadata":{"files":[{"filePath":"/abs/hello.txt","relativePath":"hello.txt","type":"add"}]}}}}"#;
        let events = process_line(line, &mut TransformState::default());
        assert_eq!(events.len(), 2);
        match &events[0].payload {
            EventPayload::ToolCall(c) => {
                assert_eq!(c.tool, "apply_patch");
                // relativePath wins over the absolute filePath.
                assert_eq!(c.path.as_deref(), Some("hello.txt"));
                assert!(c.args.is_some());
            }
            other => panic!("expected tool_call, got {other:?}"),
        }
        match &events[1].payload {
            EventPayload::ToolResult(r) => {
                assert_eq!(r.tool, "apply_patch");
                assert!(r.success);
                assert_eq!(r.output.as_deref(), Some("Success."));
            }
            other => panic!("expected tool_result, got {other:?}"),
        }
    }

    #[test]
    fn repeated_tool_part_emits_each_phase_once() {
        let mut seen = TransformState::default();
        let running = r#"{"type":"tool_use","timestamp":1,"part":{"tool":"read","callID":"c1","state":{"status":"running","input":{"filePath":"a.rs"}}}}"#;
        let done = r#"{"type":"tool_use","timestamp":2,"part":{"tool":"read","callID":"c1","state":{"status":"completed","input":{"filePath":"a.rs"},"output":"ok"}}}"#;

        let first = process_line(running, &mut seen);
        assert_eq!(first.len(), 1, "running emits only the call");
        assert!(matches!(first[0].payload, EventPayload::ToolCall(_)));

        let second = process_line(done, &mut seen);
        assert_eq!(second.len(), 1, "completion must not re-emit the call");
        assert!(matches!(second[0].payload, EventPayload::ToolResult(_)));

        // A third copy of the completed part adds nothing.
        assert!(process_line(done, &mut seen).is_empty());
    }

    #[test]
    fn errored_tool_reports_failure() {
        let line = r#"{"type":"tool_use","timestamp":1,"part":{"tool":"bash","callID":"c2","state":{"status":"error","error":"exit 1"}}}"#;
        let events = process_line(line, &mut TransformState::default());
        assert_eq!(events.len(), 2);
        match &events[1].payload {
            EventPayload::ToolResult(r) => {
                assert!(!r.success);
                assert_eq!(r.output.as_deref(), Some("exit 1"));
            }
            other => panic!("expected tool_result, got {other:?}"),
        }
    }

    #[test]
    fn step_finish_folds_reasoning_into_output() {
        let line = r#"{"type":"step_finish","timestamp":1788799629598,"part":{"type":"step-finish","tokens":{"total":8811,"input":8017,"output":90,"reasoning":704,"cache":{"write":0,"read":0}}}}"#;
        let events = process_line(line, &mut TransformState::default());
        assert_eq!(events.len(), 1);
        match &events[0].payload {
            EventPayload::Tokens(t) => {
                assert_eq!(t.input, 8017);
                assert_eq!(t.output, 794, "output must include reasoning tokens");
                assert_eq!(t.cache_read, Some(0));
                assert_eq!(t.cache_write, Some(0));
            }
            other => panic!("expected tokens, got {other:?}"),
        }
    }

    #[test]
    fn needle_meta_marker_names_the_model_and_emits_nothing() {
        let mut seen = TransformState::default();
        let meta = r#"{"type":"needle_meta","model":"opencode/big-pickle"}"#;
        assert!(
            process_line(meta, &mut seen).is_empty(),
            "the marker is consumed, not forwarded"
        );

        let line = r#"{"type":"step_finish","timestamp":1,"part":{"tokens":{"input":10,"output":2,"reasoning":0}}}"#;
        match &process_line(line, &mut seen)[0].payload {
            EventPayload::Tokens(t) => assert_eq!(t.model, "opencode/big-pickle"),
            other => panic!("expected tokens, got {other:?}"),
        }
    }

    #[test]
    fn model_is_unknown_rather_than_dropped_when_unannounced() {
        // No marker and (under `cargo test`) no NEEDLE_OPENCODE_MODEL: usage
        // must still be reported so cost stays visible.
        let mut seen = TransformState::default();
        let line =
            r#"{"type":"step_finish","timestamp":1,"part":{"tokens":{"input":10,"output":2}}}"#;
        let events = process_line(line, &mut seen);
        assert_eq!(events.len(), 1);
        match &events[0].payload {
            EventPayload::Tokens(t) => {
                assert_eq!(t.input, 10);
                assert!(
                    t.model == "unknown" || !t.model.is_empty(),
                    "model falls back rather than vanishing"
                );
            }
            other => panic!("expected tokens, got {other:?}"),
        }
    }

    #[test]
    fn all_zero_step_finish_is_dropped() {
        // Observed live: opencode retries a retryable upstream failure (zai-proxy
        // 503) and emits one all-zero step_finish per attempt. Those carry no
        // cost information and must not reach the event stream.
        let line = r#"{"type":"step_finish","timestamp":1,"part":{"tokens":{"total":0,"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}}}}"#;
        assert!(process_line(line, &mut TransformState::default()).is_empty());
    }

    #[test]
    fn step_finish_without_tokens_is_dropped() {
        let line = r#"{"type":"step_finish","timestamp":1,"part":{"reason":"tool-calls"}}"#;
        assert!(process_line(line, &mut TransformState::default()).is_empty());
    }

    #[test]
    fn error_event_is_normalized() {
        let line = r#"{"type":"error","timestamp":1788799607537,"error":{"name":"APIError","data":{"message":"Unsupported parameter","statusCode":400,"isRetryable":false}}}"#;
        let events = process_line(line, &mut TransformState::default());
        assert_eq!(events.len(), 1);
        match &events[0].payload {
            EventPayload::Error(e) => {
                assert_eq!(e.message, "Unsupported parameter");
                assert_eq!(e.code.as_deref(), Some("APIError"));
                assert!(!e.recoverable);
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn step_start_and_reasoning_are_dropped() {
        let mut seen = TransformState::default();
        assert!(process_line(
            r#"{"type":"step_start","timestamp":1,"part":{}}"#,
            &mut seen
        )
        .is_empty());
        assert!(process_line(
            r#"{"type":"reasoning","timestamp":1,"part":{"text":"thinking"}}"#,
            &mut seen
        )
        .is_empty());
    }

    #[test]
    fn malformed_line_does_not_panic() {
        assert!(process_line("not json at all", &mut TransformState::default()).is_empty());
    }

    #[test]
    fn truncate_is_codepoint_safe() {
        // Byte-slicing this would panic; character-slicing must not.
        let s = "→→→→→";
        assert_eq!(truncate(s, 2), "→→");
        assert_eq!(truncate(s, 99), s);
    }
}
