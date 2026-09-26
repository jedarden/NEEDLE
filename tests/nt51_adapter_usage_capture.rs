//! Focused N-T51 acceptance fixture.
//!
//! The fixtures are the raw JSONL emitted by each adapter. The test exercises
//! the same resolution function used by the worker, including the pricing
//! table, then verifies that a resolved Codex row is visible to the CLI's
//! adapter statistics without touching the real home or state directory.

use std::path::PathBuf;
use std::process::Command;

use needle::adapter_usage::{resolve_usage, stream_usage};
use needle::cost::default_pricing;
use needle::dispatch::{AgentAdapter, TokenUsage, UsageFormat};
use tempfile::tempdir;

const CODEX: &str = include_str!("fixtures/adapter-usage/codex.jsonl");
const OPENCODE: &str = include_str!("fixtures/adapter-usage/opencode.jsonl");
const OMP: &str = include_str!("fixtures/adapter-usage/omp.jsonl");
// The adapter smoke matrix and this usage replay share the same recorded
// summary, so invocation and accounting cannot silently drift apart.
const AIDER: &str = include_str!("fixtures/adapter-usage/aider-summary.txt");

fn resolved(format: UsageFormat, output: &str) -> needle::attempt_accounting::AttemptUsage {
    resolve_usage(
        format,
        &TokenUsage::default(),
        None,
        output,
        "gpt-4",
        &default_pricing(),
    )
}

#[test]
fn each_adapter_fixture_extracts_tokens_and_estimates_cost() {
    let cases = [
        (UsageFormat::CodexJsonl, CODEX, 600, 500),
        (UsageFormat::OpencodeJsonl, OPENCODE, 1500, 200),
        (UsageFormat::OmpJsonl, OMP, 1500, 140),
        // First line of the fixture: the cached form every Anthropic-model
        // aider session prints, with abbreviated counts.
        (UsageFormat::AiderSummary, AIDER, 12_500, 4_300),
    ];

    for (format, output, input, expected_output) in cases {
        let usage = stream_usage(format, output).expect("fixture should carry usage");
        assert_eq!(usage.input, input, "input tokens for {format:?}");
        assert_eq!(
            usage.output, expected_output,
            "output tokens for {format:?}"
        );

        let resolved = resolved(format, output);
        assert_eq!(resolved.tokens_in, Some(input));
        assert_eq!(resolved.tokens_out, Some(expected_output));
        assert!(resolved.costed, "{format:?} fixture should be costed");
        assert!(resolved.estimated_cost_usd.unwrap() > 0.0);
    }
}

#[test]
fn unknown_usage_format_is_not_costed() {
    let resolved = resolved(UsageFormat::Unknown, CODEX);
    assert_eq!(resolved.tokens_in, None);
    assert_eq!(resolved.tokens_out, None);
    assert_eq!(resolved.estimated_cost_usd, None);
    assert!(!resolved.costed);
}

#[test]
fn adapter_definition_selects_declared_usage_format() {
    let adapter: AgentAdapter = serde_yaml::from_str(
        "name: codex-fixture\nagent_cli: codex\ninvoke_template: codex\nusage_format: codex_jsonl\n",
    )
    .expect("parse adapter definition");
    assert_eq!(adapter.usage_format, Some(UsageFormat::CodexJsonl));
    assert_eq!(adapter.effective_usage_format(), UsageFormat::CodexJsonl);

    let unknown: AgentAdapter = serde_yaml::from_str(
        "name: unknown-fixture\nagent_cli: custom\ninvoke_template: custom\nusage_format: unknown\n",
    )
    .expect("parse unknown adapter definition");
    assert_eq!(unknown.effective_usage_format(), UsageFormat::Unknown);
}

#[test]
fn stats_by_adapter_reads_costed_codex_fixture_row() {
    let root = tempdir().expect("temporary fixture root");
    let home = root.path().join("home");
    let state = root.path().join("state");
    let discovery = root.path().join("discovery");
    let logs = state.join("logs");
    std::fs::create_dir_all(&logs).expect("create isolated log directory");
    std::fs::create_dir_all(&discovery).expect("create isolated discovery root");

    let event = serde_json::json!({
        "timestamp": "2026-09-17T12:00:00Z",
        "event_type": "attempt.resolved",
        "worker_id": "nt51-acceptance-worker",
        "session_id": "nt51-session",
        "sequence": 1,
        "bead_id": "needle-nt51-fixture",
        "workspace": "/tmp/nt51-fixture-workspace",
        "data": {
            "schema_version": 2,
            "attempt_id": "nt51-attempt",
            "provisional": false,
            "bead_id": "needle-nt51-fixture",
            "workspace": "/tmp/nt51-fixture-workspace",
            "worker": "nt51-acceptance-worker",
            "adapter": "codex-gpt-5.6-luna-xhigh",
            "model": "gpt-4",
            "provider": "openai",
            "outcome": "verified_success",
            "requested_action": "none",
            "tokens_in": 600,
            "tokens_out": 500,
            "estimated_cost_usd": 0.0507,
            "costed": true,
            "commits": [],
            "duration_ms": 1,
            "exit_code": 0
        },
        "duration_ms": 1,
        "trace_id": null,
        "span_id": null,
        "attempt_id": "nt51-attempt"
    });
    std::fs::write(
        logs.join("nt51-acceptance-worker-session-2026-09-17.jsonl"),
        format!("{}\n", serde_json::to_string(&event).unwrap()),
    )
    .expect("write fixture ledger");

    let output = Command::new(needle_binary())
        .args(["stats", "--by", "adapter", "--format", "json"])
        .env("HOME", &home)
        .env("NEEDLE_STATE_DIR", &state)
        .env("NEEDLE_STRANDS__EXPLORE__ENABLED", "false")
        .env("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT", &discovery)
        .output()
        .expect("run isolated stats command");
    assert!(
        output.status.success(),
        "stats failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).expect("stats JSON");
    let codex = rows
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["key"] == "codex-gpt-5.6-luna-xhigh")
        })
        .expect("codex adapter row");
    assert_eq!(codex["avg_cost_usd"], serde_json::json!(0.0507));
}

/// The `needle` binary under test. Nextest archive runs relocate it, so the
/// compile-time `CARGO_BIN_EXE_needle` path does not exist there.
fn needle_binary() -> PathBuf {
    std::env::var_os("NEXTEST_BIN_EXE_needle")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_needle")))
}
