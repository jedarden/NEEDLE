//! Integration tests for `needle-transform-claude`.
//!
//! Spawns the compiled binary, pipes a captured Claude `--output-format json`
//! session through it, and validates the normalized JSONL output.

use std::io::Write;
use std::process::{Command, Stdio};

use needle::agent_event::{AgentEvent, EventPayload, MessageRole};

// Path to the compiled binary, set by Cargo for integration tests.
const BINARY: &str = env!("CARGO_BIN_EXE_needle-transform-claude");

/// Feed `input` to the binary via stdin, return all output lines.
fn run(input: &str) -> Vec<String> {
    let mut child = Command::new(BINARY)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()) // suppress warnings in test output
        .spawn()
        .expect("failed to spawn needle-transform-claude");

    child
        .stdin
        .take()
        .expect("stdin not available")
        .write_all(input.as_bytes())
        .expect("write to stdin failed");

    let output = child.wait_with_output().expect("wait failed");
    assert!(output.status.success() || output.status.code() == Some(0));

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Parse a JSONL line into an AgentEvent.
fn parse_event(line: &str) -> AgentEvent {
    serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("failed to parse AgentEvent from {line:?}: {e}"))
}

// ──────────────────────────────────────────────────────────────────────────────
// Full session smoke test
// ──────────────────────────────────────────────────────────────────────────────

/// A minimal but realistic Claude `--output-format json` session:
///
/// 1. system init (ignored)
/// 2. assistant: text + tool_use (Read)
/// 3. user: tool_result (Read success)
/// 4. assistant: text only
/// 5. result: success (no event emitted)
const SAMPLE_SESSION: &str = concat!(
    // system — should be silently skipped
    r#"{"type":"system","subtype":"init","cwd":"/repo","session_id":"sess_01","tools":["Read","Edit"],"model":"claude-sonnet-4-6"}"#,
    "\n",
    // assistant turn 1: preamble text + tool_use Read
    r#"{"type":"assistant","message":{"id":"msg_01","type":"message","role":"assistant","content":[{"type":"text","text":"Let me read the file."},{"type":"tool_use","id":"toolu_01","name":"Read","input":{"file_path":"src/main.rs"}}],"model":"claude-sonnet-4-6","stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":1234,"output_tokens":56,"cache_creation_input_tokens":0,"cache_read_input_tokens":800}}}"#,
    "\n",
    // user turn 1: tool_result for Read
    r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"fn main() {\n    println!(\"hello\");\n}\n","is_error":false}]}}"#,
    "\n",
    // assistant turn 2: final text
    r#"{"type":"assistant","message":{"id":"msg_02","type":"message","role":"assistant","content":[{"type":"text","text":"The file looks good."}],"model":"claude-sonnet-4-6","stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1500,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
    "\n",
    // result success — no event
    r#"{"type":"result","subtype":"success","cost_usd":0.00123,"is_error":false,"session_id":"sess_01","num_turns":2}"#,
    "\n",
);

#[test]
fn full_session_normalized_output() {
    let lines = run(SAMPLE_SESSION);

    // Expected events (in order):
    //  0  agent_message  "Let me read the file."
    //  1  tool_call      Read  src/main.rs
    //  2  tokens         input=1234 output=56 cache_read=800
    //  3  tool_result    Read  success
    //  4  agent_message  "The file looks good."
    //  5  tokens         input=1500 output=10
    assert_eq!(lines.len(), 6, "expected 6 events, got: {lines:#?}");

    // All events are valid AgentEvents.
    let events: Vec<AgentEvent> = lines.iter().map(|l| parse_event(l)).collect();

    // 0 — agent_message
    match &events[0].payload {
        EventPayload::AgentMessage(am) => {
            assert_eq!(am.role, MessageRole::Assistant);
            assert_eq!(am.content, "Let me read the file.");
        }
        other => panic!("event[0]: expected AgentMessage, got {other:?}"),
    }

    // 1 — tool_call
    match &events[1].payload {
        EventPayload::ToolCall(tc) => {
            assert_eq!(tc.tool, "Read");
            assert_eq!(tc.path.as_deref(), Some("src/main.rs"));
        }
        other => panic!("event[1]: expected ToolCall, got {other:?}"),
    }

    // 2 — tokens (with cache_read)
    match &events[2].payload {
        EventPayload::Tokens(t) => {
            assert_eq!(t.input, 1234);
            assert_eq!(t.output, 56);
            assert_eq!(t.model, "claude-sonnet-4-6");
            assert_eq!(t.cache_read, Some(800));
            assert!(t.cache_write.is_none());
        }
        other => panic!("event[2]: expected Tokens, got {other:?}"),
    }

    // 3 — tool_result
    match &events[3].payload {
        EventPayload::ToolResult(tr) => {
            assert_eq!(tr.tool, "Read");
            assert!(tr.success);
            // output is truncated to 500 chars; first 12 chars should match
            assert!(tr.output.as_deref().unwrap_or("").starts_with("fn main()"));
        }
        other => panic!("event[3]: expected ToolResult, got {other:?}"),
    }

    // 4 — agent_message
    match &events[4].payload {
        EventPayload::AgentMessage(am) => {
            assert_eq!(am.content, "The file looks good.");
        }
        other => panic!("event[4]: expected AgentMessage, got {other:?}"),
    }

    // 5 — tokens (no cache)
    match &events[5].payload {
        EventPayload::Tokens(t) => {
            assert_eq!(t.input, 1500);
            assert_eq!(t.output, 10);
            assert!(t.cache_read.is_none());
        }
        other => panic!("event[5]: expected Tokens, got {other:?}"),
    }

    // schema_version = 1 on all events
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev.schema_version, 1, "event[{i}] schema_version != 1");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Error session
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn error_result_emits_error_event() {
    let input = concat!(
        r#"{"type":"result","subtype":"error_max_turns","cost_usd":0.05,"is_error":true,"session_id":"s1","num_turns":10}"#,
        "\n",
    );
    let lines = run(input);
    assert_eq!(lines.len(), 1);
    let event = parse_event(&lines[0]);
    match &event.payload {
        EventPayload::Error(e) => {
            assert!(!e.recoverable);
            assert_eq!(e.code.as_deref(), Some("error_max_turns"));
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Robustness: malformed lines mixed with valid lines
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn malformed_lines_skipped_valid_lines_pass_through() {
    let input = concat!(
        "this is not json\n",
        r#"{"type":"assistant","message":{"id":"m1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"model":"claude-sonnet-4-6","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        "\n",
        "{broken json\n",
        "\n", // empty line — skipped
    );

    let lines = run(input);
    // Only the valid assistant turn produces events: agent_message + tokens
    assert_eq!(lines.len(), 2, "got: {lines:#?}");
    assert!(matches!(
        parse_event(&lines[0]).payload,
        EventPayload::AgentMessage(_)
    ));
    assert!(matches!(
        parse_event(&lines[1]).payload,
        EventPayload::Tokens(_)
    ));
}

// ──────────────────────────────────────────────────────────────────────────────
// Edit tool: path extraction and error result
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn edit_tool_use_and_failed_result() {
    let input = concat!(
        r#"{"type":"assistant","message":{"id":"m2","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_edit","name":"Edit","input":{"file_path":"src/lib.rs","old_string":"foo","new_string":"bar"}}],"model":"claude-sonnet-4-6","stop_reason":"tool_use","usage":{"input_tokens":50,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_edit","content":"old_string not found","is_error":true}]}}"#,
        "\n",
    );

    let lines = run(input);
    // tool_call, tokens, tool_result
    assert_eq!(lines.len(), 3, "got: {lines:#?}");

    match &parse_event(&lines[0]).payload {
        EventPayload::ToolCall(tc) => {
            assert_eq!(tc.tool, "Edit");
            assert_eq!(tc.path.as_deref(), Some("src/lib.rs"));
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }

    match &parse_event(&lines[2]).payload {
        EventPayload::ToolResult(tr) => {
            assert_eq!(tr.tool, "Edit");
            assert!(!tr.success);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}
