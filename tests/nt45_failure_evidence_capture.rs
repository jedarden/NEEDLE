//! Focused behavioral contracts for N-T45 failure evidence capture.

use std::collections::HashMap;

use needle::attempt_history::{
    append_local, capture_failure_evidence, capture_failure_evidence_blocked,
    capture_failure_evidence_with_limit, render, AttemptRecord, FailureEvidence, GateDiagnostic,
    HistoryLimits, ToolErrorEvidence, MAX_FAILURE_EVIDENCE_BYTES, SANITIZER_BLOCKED_MARKER,
    SCHEMA_VERSION,
};
use needle::config::Config;
use needle::sanitize::{CustomPattern, Sanitizer};
use needle::types::BeadId;
use needle::validation::{GateReport, GateResult};

fn gate_report() -> GateReport {
    let mut results = HashMap::new();
    results.insert(
        "cargo-test".to_string(),
        GateResult::Fail("cargo test\n\ntest result: FAILED. 1 passed; 1 failed\n".to_string()),
    );
    GateReport::new(results)
}

fn sanitizer() -> Sanitizer {
    Sanitizer::new(&[CustomPattern {
        id: "fixture-secret".to_string(),
        pattern: r"secret=([A-Za-z0-9_-]+)".to_string(),
        entropy: None,
    }])
    .expect("fixture sanitizer")
}

fn fixture_transcript() -> &'static str {
    concat!(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"call-1","name":"Bash","input":{"command":"cargo check"}}]}}"#,
        "\n",
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call-1","content":"error[E0308]: mismatched types; secret=do-not-leak","is_error":true}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"call-2","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        "\n",
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call-2","content":"test result: FAILED. 1 passed; 1 failed","is_error":true}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"The final patch was not complete."}]}}"#,
        "\n",
    )
}

fn record_with(evidence: FailureEvidence) -> AttemptRecord {
    AttemptRecord {
        schema_version: SCHEMA_VERSION,
        attempt_id: "att-nt45".to_string(),
        recorded_at: "2026-09-16T10:20:30.000Z".to_string(),
        worker: "fixture-worker".to_string(),
        adapter: "fixture-agent".to_string(),
        model: None,
        outcome: "work_failure".to_string(),
        terminal_reason: Some("gate:tests".to_string()),
        exit_code: 1,
        requested_action: "released:dispatch_failed".to_string(),
        commits: Vec::new(),
        duration_ms: 10,
        failure_summary: None,
        failure_evidence: Some(evidence),
    }
}

#[test]
fn fixture_transcript_captures_tools_final_message_and_gate_diagnostic() {
    let evidence = capture_failure_evidence(
        fixture_transcript(),
        Some(&gate_report()),
        Some(&sanitizer()),
    )
    .expect("failure evidence");
    let record = record_with(evidence.clone());

    assert_eq!(evidence.tool_errors.len(), 2);
    assert_eq!(evidence.tool_errors[0].tool_name, "Bash");
    assert!(
        evidence.tool_errors[0].signature.contains("error[E<n>]")
            || evidence.tool_errors[0].signature.contains("error[E0308]")
    );
    assert!(evidence.tool_errors[0]
        .excerpt
        .contains("[REDACTED:fixture-secret]"));
    assert!(evidence.tool_errors[1]
        .excerpt
        .contains("test result: FAILED"));
    assert_eq!(
        evidence.final_message.as_deref(),
        Some("The final patch was not complete.")
    );
    assert_eq!(evidence.gate_diagnostics[0].gate_name, "cargo-test");
    assert!(evidence.gate_diagnostics[0]
        .error_block
        .contains("test result: FAILED"));
    assert!(evidence.serialized_bytes() <= MAX_FAILURE_EVIDENCE_BYTES);

    let dir = tempfile::tempdir().expect("tempdir");
    let bead_id = BeadId::from("needle-nt45-fixture");
    append_local(dir.path(), &bead_id, &record).expect("append record");
    let loaded = needle::attempt_history::load_local(dir.path(), &bead_id).expect("load record");
    assert_eq!(loaded[0].failure_evidence, Some(evidence));
}

#[test]
fn timed_out_partial_transcript_keeps_its_last_tool_error() {
    let partial = concat!(
        r#"{"schema_version":1,"ts":1.0,"type":"tool_result","tool":"shell","success":true,"output":"ok"}"#,
        "\n",
        r#"{"schema_version":1,"ts":2.0,"type":"tool_result","tool":"shell","success":false,"output":"error: compiler stopped before producing an artifact"}"#,
        "\n{malformed trailing line"
    );
    let evidence = capture_failure_evidence(partial, None, Some(&sanitizer()))
        .expect("partial failure evidence");

    assert_eq!(evidence.tool_errors.len(), 1);
    assert_eq!(evidence.tool_errors[0].tool_name, "shell");
    assert!(evidence.tool_errors[0].excerpt.contains("compiler stopped"));
}

#[test]
fn sanitizer_blocked_content_is_marked_and_bounded() {
    let evidence = capture_failure_evidence_blocked(fixture_transcript(), Some(&gate_report()))
        .expect("blocked failure evidence");

    assert!(evidence
        .final_message
        .as_deref()
        .unwrap_or_default()
        .contains(SANITIZER_BLOCKED_MARKER));
    assert!(evidence
        .tool_errors
        .iter()
        .all(|error| error.excerpt.contains(SANITIZER_BLOCKED_MARKER)));
    assert!(evidence
        .gate_diagnostics
        .iter()
        .all(|diagnostic| diagnostic.error_block.contains(SANITIZER_BLOCKED_MARKER)));
    assert!(evidence.serialized_bytes() <= MAX_FAILURE_EVIDENCE_BYTES);

    let tiny = capture_failure_evidence_with_limit(
        fixture_transcript(),
        Some(&gate_report()),
        None,
        512,
        true,
    )
    .expect("tiny blocked failure evidence");
    assert!(tiny.serialized_bytes() <= 512);
}

#[test]
fn evidence_is_off_by_default_and_rendering_is_byte_stable() {
    assert!(
        !Config::default()
            .strands
            .learning
            .failure_history
            .evidence
            .enabled
    );

    let evidence = FailureEvidence {
        final_message: Some("Final patch was not complete.".to_string()),
        tool_errors: vec![ToolErrorEvidence {
            tool_name: "Bash".to_string(),
            signature: "sig-test".to_string(),
            excerpt: "error[E0308]: mismatched types".to_string(),
        }],
        gate_diagnostics: vec![GateDiagnostic {
            gate_name: "cargo-test".to_string(),
            error_block: "test result: FAILED".to_string(),
        }],
    };
    let rendered = render(
        &[record_with(evidence)],
        HistoryLimits {
            max_attempts: 1,
            max_bytes: 4000,
        },
    );
    let expected = concat!(
        "## Previous attempts on this bead (newest first)\n\n",
        "This bead has been attempted 1 time before (1 without a verified success). ",
        "Read the failures below before you start. Do NOT repeat an approach that already ",
        "failed the same way. If a verification gate failed, make that gate pass first — ",
        "the gate is what decides whether your work is accepted.\n\n",
        "### Attempt 1 — 2026-09-16T10:20Z — fixture-agent — outcome: work_failure (gate:tests)\n",
        "Failure evidence:\n",
        "Final assistant message:\n```\n",
        "Final patch was not complete.\n```\n",
        "Tool error — Bash — signature: sig-test\n```\n",
        "error[E0308]: mismatched types\n```\n",
        "Gate diagnostic — cargo-test:\n```\n",
        "test result: FAILED\n```"
    );
    assert_eq!(rendered, expected);
}
