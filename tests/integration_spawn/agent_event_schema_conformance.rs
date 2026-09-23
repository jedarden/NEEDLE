//! Agent-event schema conformance: `docs/agent-event-schema.md` as an
//! executable contract.
//!
//! The schema doc is the authoritative transform↔FABRIC contract, but until
//! now nothing executed it — the doc's examples could drift from the canonical
//! types in `src/agent_event.rs`, and only `needle-transform-claude` was ever
//! exercised at the binary level. These tests close both gaps:
//!
//! 1. **Doc conformance** — every JSON example line in the schema doc must
//!    parse and round-trip as an [`AgentEvent`], the doc's `### ` heading
//!    vocabulary must equal the v1 discriminant set, each field the doc marks
//!    Required must actually be enforced by the deserializer (and each
//!    optional field must be omissible), unknown fields must be ignored (the
//!    doc's additive-change rule), and an unknown `type` must fail closed.
//! 2. **Adapter conformance** — every shipped output transform
//!    (`needle-transform-{claude,codex,opencode,omp}`) is spawned as the real
//!    binary and driven with the upstream-session fixtures under
//!    `tests/fixtures/agent-events/`. Every line each binary emits must
//!    deserialize against the schema with a v1 `schema_version`, a
//!    plausible epoch-seconds `ts` (milliseconds leak is a contract break),
//!    and a payload inside the documented vocabulary. Each fixture carries
//!    malformed and unknown-type upstream lines, so exit-status 0 also proves
//!    the transforms degrade gracefully rather than crashing.
//!
//! If this suite fails, either the doc or an implementation moved without the
//! other — exactly the drift the bead that added it was about.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use needle::agent_event::{AgentEvent, EventPayload, SCHEMA_VERSION};
use serde_json::Value;

// ──────────────────────────────────────────────────────────────────────────────
// The documented contract, expressed as data
// ──────────────────────────────────────────────────────────────────────────────

/// The v1 discriminant set — the doc's `## Event Types` headings and the
/// `EventPayload` variants' serde renames must both equal this.
const DOCUMENTED_EVENT_TYPES: &[&str] = &[
    "tool_call",
    "tool_result",
    "agent_message",
    "tokens",
    "error",
];

/// Envelope fields the doc's table marks Required for every event.
const DOCUMENTED_ENVELOPE_REQUIRED: &[&str] = &["schema_version", "type", "ts"];

/// Per-type payload fields the doc marks Required.
const DOCUMENTED_REQUIRED_FIELDS: &[(&str, &[&str])] = &[
    ("tool_call", &["tool"]),
    ("tool_result", &["tool", "success"]),
    ("agent_message", &["role", "content"]),
    ("tokens", &["input", "output", "model"]),
    ("error", &["message", "recoverable"]),
];

/// Per-type payload fields the doc marks optional (Required = no).
const DOCUMENTED_OPTIONAL_FIELDS: &[(&str, &[&str])] = &[
    ("tool_call", &["path", "args"]),
    ("tool_result", &["output"]),
    ("agent_message", &[]),
    ("tokens", &["cache_read", "cache_write"]),
    ("error", &["code"]),
];

/// (transform binary, upstream fixture) — one entry per shipped transform.
const ADAPTER_MATRIX: &[(&str, &str)] = &[
    ("needle-transform-claude", "claude-session.jsonl"),
    ("needle-transform-codex", "codex-session.jsonl"),
    ("needle-transform-opencode", "opencode-session.jsonl"),
    ("needle-transform-omp", "omp-session.jsonl"),
];

/// Epoch-seconds upper sanity bound (~year 2096). A transform that forgets to
/// divide opencode/omp epoch milliseconds by 1000 emits ~1.8e12 and lands
/// here.
const TS_EPOCH_SECONDS_MAX: f64 = 4_000_000_000.0;

// ──────────────────────────────────────────────────────────────────────────────
// Doc and fixture loading
// ──────────────────────────────────────────────────────────────────────────────

fn repo_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn schema_doc() -> String {
    fs::read_to_string(repo_file("docs/agent-event-schema.md"))
        .expect("docs/agent-event-schema.md must exist alongside the tests")
}

/// Every non-empty line inside a ```json / ```jsonl fenced block of the doc.
fn fenced_json_lines(doc: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut in_block = false;
    for raw in doc.lines() {
        let trimmed = raw.trim();
        if in_block {
            if trimmed == "```" {
                in_block = false;
            } else if !trimmed.is_empty() {
                lines.push(trimmed);
            }
        } else if trimmed.starts_with("```json") {
            in_block = true;
        }
    }
    lines
}

/// The doc's event-type headings, parsed from the `## Event Types` section.
fn documented_event_type_headings(doc: &str) -> Vec<String> {
    let mut types = Vec::new();
    let mut in_section = false;
    for raw in doc.lines() {
        match raw.trim() {
            "## Event Types" => in_section = true,
            "## Full Example Stream" => break,
            trimmed if in_section && trimmed.starts_with("### `") => {
                let name = trimmed
                    .trim_start_matches("### `")
                    .split('`')
                    .next()
                    .unwrap_or("");
                if !name.is_empty() {
                    types.push(name.to_owned());
                }
            }
            _ => {}
        }
    }
    types
}

/// One documented example per event type (first occurrence wins).
fn examples_by_type() -> Vec<(String, serde_json::Value)> {
    let mut by_type: Vec<(String, serde_json::Value)> = Vec::new();
    for line in fenced_json_lines(&schema_doc()) {
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("doc example line {line:?} is not valid JSON: {e}"));
        let event_type = value
            .get("type")
            .and_then(|t| t.as_str())
            .expect("every doc example carries a type discriminant")
            .to_owned();
        if !by_type.iter().any(|(t, _)| *t == event_type) {
            by_type.push((event_type, value));
        }
    }
    by_type
}

fn example_for<'a>(
    examples: &'a [(String, serde_json::Value)],
    event_type: &str,
) -> &'a serde_json::Value {
    &examples
        .iter()
        .find(|(t, _)| t == event_type)
        .unwrap_or_else(|| panic!("doc carries no example for `{event_type}`"))
        .1
}

fn fixture_path(fixture: &str) -> PathBuf {
    repo_file(&format!("tests/fixtures/agent-events/{fixture}"))
}

/// Keep the negative cases in every adapter fixture executable.  A fixture
/// that accidentally loses its malformed, unknown-type, or missing-type input
/// would otherwise let the adapter's graceful-degradation contract go
/// untested while all emitted lines still conform.
fn assert_bad_fixture_inputs_are_skipped(transform: &str, input: &str, stdout: &str) {
    let mut malformed = 0;
    let mut unknown_type = false;
    let mut missing_type = false;

    for line in input.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(value) => match value.get("type").and_then(Value::as_str) {
                Some("needle_future_unknown") => unknown_type = true,
                Some(_) => {}
                None => missing_type = true,
            },
            Err(_) => malformed += 1,
        }
    }

    assert!(
        malformed > 0,
        "{transform} fixture lost its malformed JSON input"
    );
    assert!(
        unknown_type,
        "{transform} fixture lost its unknown event-type input"
    );
    assert!(
        missing_type,
        "{transform} fixture lost its event without a type input"
    );
    assert!(
        !stdout.contains("needle_future_unknown"),
        "{transform} emitted output for an unknown upstream event type"
    );
    assert!(
        !stdout.contains("MALFORMED_SENTINEL"),
        "{transform} emitted output for malformed upstream input"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Adapter execution and stream validation
// ──────────────────────────────────────────────────────────────────────────────

/// Runtime-relocatable path for a transform binary — mirrors
/// `isolation::needle_transform_claude_binary_path`: nextest remaps the
/// compile-time `CARGO_BIN_EXE_*` paths, so prefer the runtime variable.
fn transform_binary_path(transform: &str) -> OsString {
    let nextest_name = format!("NEXTEST_BIN_EXE_{}", transform.replace('-', "_"));
    std::env::var_os(nextest_name).unwrap_or_else(|| match transform {
        "needle-transform-claude" => env!("CARGO_BIN_EXE_needle-transform-claude").into(),
        "needle-transform-codex" => env!("CARGO_BIN_EXE_needle-transform-codex").into(),
        "needle-transform-opencode" => env!("CARGO_BIN_EXE_needle-transform-opencode").into(),
        "needle-transform-omp" => env!("CARGO_BIN_EXE_needle-transform-omp").into(),
        other => panic!("unknown transform binary: {other}"),
    })
}

/// Pipe `fixture` through the built transform, validate every emitted line
/// against the schema, and return the parsed events.
fn conformant_events_from_fixture(transform: &str, fixture: &Path) -> Vec<AgentEvent> {
    let input = fs::read_to_string(fixture)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", fixture.display()));

    let mut child = Command::new(transform_binary_path(transform))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()) // malformed-line warnings are expected noise here
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {transform}: {e}"));

    child
        .stdin
        .take()
        .expect("transform stdin is piped")
        .write_all(input.as_bytes())
        .expect("write fixture to transform stdin");

    let output = child.wait_with_output().expect("wait for transform");
    assert!(
        output.status.success(),
        "{transform} exited {:?} — transforms must skip malformed and unknown \
         upstream lines and exit 0, never crash",
        output.status.code()
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_bad_fixture_inputs_are_skipped(transform, &input, &stdout);

    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| assert_conformant_line(transform, line))
        .collect()
}

/// Validate one serialized event line against the documented schema.
fn assert_conformant_line(transform: &str, line: &str) -> AgentEvent {
    let event: AgentEvent = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("{transform} emitted a non-conforming line {line:?}: {e}"));

    assert_eq!(
        event.schema_version, SCHEMA_VERSION,
        "{transform} emitted schema_version {} — the v1 contract pins 1",
        event.schema_version
    );
    assert!(
        event.ts.is_finite() && event.ts > 0.0 && event.ts < TS_EPOCH_SECONDS_MAX,
        "{transform} emitted ts {} — not plausible epoch seconds \
         (milliseconds leaking into the envelope?)",
        event.ts
    );

    // `#[non_exhaustive]` forces this arm outside the defining crate; a future
    // variant must extend the documented vocabulary, the fixtures, and the
    // coverage assertion together — exactly the drift this suite guards.
    match &event.payload {
        EventPayload::ToolCall(payload) => {
            assert!(!payload.tool.is_empty(), "{transform}: empty tool name");
        }
        EventPayload::ToolResult(payload) => {
            assert!(!payload.tool.is_empty(), "{transform}: empty tool name");
        }
        EventPayload::AgentMessage(payload) => {
            assert!(!payload.content.is_empty(), "{transform}: empty message");
        }
        EventPayload::Tokens(payload) => {
            assert!(!payload.model.is_empty(), "{transform}: empty model");
        }
        EventPayload::Error(payload) => {
            assert!(!payload.message.is_empty(), "{transform}: empty error");
        }
        other => panic!("{transform} emitted a payload outside the v1 vocabulary: {other:?}"),
    }

    event
}

/// A fixture that stopped exercising one of the five documented types would
/// let the per-line assertions pass vacuously — refuse that silently.
fn assert_covers_documented_vocabulary(transform: &str, events: &[AgentEvent]) {
    let mut covered = [false; DOCUMENTED_EVENT_TYPES.len()];
    for event in events {
        match &event.payload {
            EventPayload::ToolCall(_) => covered[0] = true,
            EventPayload::ToolResult(_) => covered[1] = true,
            EventPayload::AgentMessage(_) => covered[2] = true,
            EventPayload::Tokens(_) => covered[3] = true,
            EventPayload::Error(_) => covered[4] = true,
            other => panic!("{transform} emitted an undocumented payload: {other:?}"),
        }
    }
    assert!(
        covered.iter().all(|&seen| seen),
        "{transform} fixture no longer exercises every documented event type \
         (covered: tool_call={}, tool_result={}, agent_message={}, tokens={}, \
         error={}) — the conformance run is vacuous without all five",
        covered[0],
        covered[1],
        covered[2],
        covered[3],
        covered[4]
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Doc conformance
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn documented_examples_parse_and_round_trip() {
    assert_eq!(SCHEMA_VERSION, 1, "the doc's current version is 1");

    let doc = schema_doc();
    let lines = fenced_json_lines(&doc);
    assert!(
        lines.len() >= DOCUMENTED_EVENT_TYPES.len(),
        "the schema doc lost its JSON examples — only {} fenced lines remain",
        lines.len()
    );

    for line in lines {
        let event: AgentEvent = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("doc example {line:?} violates the schema: {e}"));
        assert_eq!(
            event.schema_version, SCHEMA_VERSION,
            "doc example {line:?} pins schema_version {}",
            event.schema_version
        );
        assert!(
            event.ts.is_finite() && event.ts >= 0.0,
            "doc example {line:?} carries a non-finite or negative ts"
        );

        // Serialization must stay on the wire the doc documents: a re-parse of
        // what the types emit is byte-equivalent at the value level.
        let serialized = serde_json::to_string(&event)
            .unwrap_or_else(|e| panic!("doc example failed to serialize: {e}"));
        let reparsed: AgentEvent = serde_json::from_str(&serialized)
            .unwrap_or_else(|e| panic!("serialized doc example {serialized:?} re-parse: {e}"));
        assert_eq!(event, reparsed, "doc example does not round-trip");
    }
}

#[test]
fn documented_event_type_headings_match_the_v1_vocabulary() {
    let mut headings = documented_event_type_headings(&schema_doc());
    headings.sort();

    let mut expected = DOCUMENTED_EVENT_TYPES.to_vec();
    expected.sort();

    assert_eq!(
        headings, expected,
        "the doc's `## Event Types` headings and the v1 discriminant set \
         disagree — update the doc, `EventPayload`, and this table together"
    );
}

#[test]
fn documented_required_fields_are_enforced() {
    let examples = examples_by_type();

    for (event_type, required) in DOCUMENTED_REQUIRED_FIELDS {
        for &field in *required {
            let mut stripped = example_for(&examples, event_type).clone();
            stripped
                .as_object_mut()
                .expect("doc examples are JSON objects")
                .remove(field);
            let result: Result<AgentEvent, _> = serde_json::from_value(stripped);
            assert!(
                result.is_err(),
                "`{event_type}` without its documented-required `{field}` still \
                 deserializes — the doc's Required column and serde disagree"
            );
        }
    }

    // The envelope's required fields, exercised on the first example.
    for &field in DOCUMENTED_ENVELOPE_REQUIRED {
        let mut stripped = examples[0].1.clone();
        stripped
            .as_object_mut()
            .expect("doc examples are JSON objects")
            .remove(field);
        let result: Result<AgentEvent, _> = serde_json::from_value(stripped);
        assert!(
            result.is_err(),
            "an event without its documented-required envelope field `{field}` \
             still deserializes"
        );
    }
}

#[test]
fn documented_optional_fields_may_be_omitted() {
    let examples = examples_by_type();

    for (event_type, optional) in DOCUMENTED_OPTIONAL_FIELDS {
        for &field in *optional {
            let mut stripped = example_for(&examples, event_type).clone();
            stripped
                .as_object_mut()
                .expect("doc examples are JSON objects")
                .remove(field);
            let result: Result<AgentEvent, _> = serde_json::from_value(stripped);
            assert!(
                result.is_ok(),
                "`{event_type}` without its documented-optional `{field}` failed \
                 to deserialize: {result:?} — the doc promises it is omittable"
            );
        }
    }
}

#[test]
fn unknown_fields_are_ignored_for_additive_compatibility() {
    let examples = examples_by_type();

    for (event_type, example) in &examples {
        let baseline: AgentEvent = serde_json::from_value(example.clone())
            .unwrap_or_else(|e| panic!("baseline `{event_type}` example: {e}"));

        let mut extended = example.clone();
        extended
            .as_object_mut()
            .expect("doc examples are JSON objects")
            .insert(
                "fabricated_future_field".to_owned(),
                serde_json::json!({"nested": [1, 2, 3]}),
            );
        let result: Result<AgentEvent, _> = serde_json::from_value(extended);
        let with_unknown = result.unwrap_or_else(|e| {
            panic!(
                "`{event_type}` with an additive unknown field failed to \
                 deserialize: {e} — the doc requires consumers to ignore them"
            )
        });
        assert_eq!(
            baseline, with_unknown,
            "`{event_type}`: an unknown field changed the parsed event — \
             additive fields must be dropped, not folded into the payload"
        );
    }
}

#[test]
fn unknown_event_types_fail_closed() {
    let examples = examples_by_type();
    let mut unknown = examples[0].1.clone();
    unknown
        .as_object_mut()
        .expect("doc examples are JSON objects")
        .insert("type".to_owned(), serde_json::json!("quantum_flux"));

    let result: Result<AgentEvent, _> = serde_json::from_value(unknown);
    assert!(
        result.is_err(),
        "an unknown `type` discriminant deserialized successfully — the v1 \
         deserializer is fail-closed; FABRIC-level tolerance of new types is \
         that consumer's concern, not this schema's"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Per-adapter conformance over the upstream fixtures
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn claude_fixture_stream_conforms_to_schema() {
    let (transform, fixture) = ADAPTER_MATRIX[0];
    let events = conformant_events_from_fixture(transform, &fixture_path(fixture));
    assert_covers_documented_vocabulary(transform, &events);

    // The fixture's failed Edit result must arrive with its tool name
    // resolved through the transform's tool_use_id correlation, not "unknown".
    let failed = events.iter().any(|event| match &event.payload {
        EventPayload::ToolResult(result) => !result.success && result.tool == "Edit",
        _ => false,
    });
    assert!(
        failed,
        "claude fixture: the errored Edit tool_result lost its correlated \
         tool name or success flag"
    );
}

#[test]
fn codex_fixture_stream_conforms_to_schema() {
    let (transform, fixture) = ADAPTER_MATRIX[1];
    let events = conformant_events_from_fixture(transform, &fixture_path(fixture));
    assert_covers_documented_vocabulary(transform, &events);

    let reported_usage = events.iter().any(|event| match &event.payload {
        EventPayload::Tokens(tokens) => {
            tokens.model == "gpt-5-codex" && tokens.cache_read == Some(2048)
        }
        _ => false,
    });
    assert!(
        reported_usage,
        "codex fixture: turn.completed usage did not surface as a tokens \
         event with model and cached_input_tokens"
    );
}

#[test]
fn opencode_fixture_stream_conforms_to_schema() {
    let (transform, fixture) = ADAPTER_MATRIX[2];
    let events = conformant_events_from_fixture(transform, &fixture_path(fixture));
    assert_covers_documented_vocabulary(transform, &events);

    // Reasoning tokens are billed as output: 640 + 220 from the fixture's
    // step_finish, under the model injected by the needle_meta line.
    let usage = events.iter().any(|event| match &event.payload {
        EventPayload::Tokens(tokens) => {
            tokens.output == 860 && tokens.model == "opencode/glm-5.3-flash"
        }
        _ => false,
    });
    assert!(
        usage,
        "opencode fixture: step_finish tokens (with reasoning folded into \
         output, model from needle_meta) did not surface"
    );
}

#[test]
fn omp_fixture_stream_conforms_to_schema() {
    let (transform, fixture) = ADAPTER_MATRIX[3];
    let events = conformant_events_from_fixture(transform, &fixture_path(fixture));
    assert_covers_documented_vocabulary(transform, &events);

    // usage.output already includes reasoningTokens here — it must be
    // forwarded as-is (1022), not re-inflated, under provider/model.
    let usage = events.iter().any(|event| match &event.payload {
        EventPayload::Tokens(tokens) => {
            tokens.output == 1022 && tokens.model == "zai/glm-5.3-flash"
        }
        _ => false,
    });
    assert!(
        usage,
        "omp fixture: message_end usage did not surface with the \
         provider-qualified model and omp's own output figure"
    );
}
