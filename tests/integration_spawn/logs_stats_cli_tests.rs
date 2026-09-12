//! End-to-end validation for the `needle logs` and `needle stats` CLI commands.
//!
//! These tests spawn the compiled `needle` binary with `HOME` pointed at an
//! isolated temp directory (required for every subprocess test — see
//! `docs/testing-isolation-patterns.md`), seed `$HOME/.needle/logs/*.jsonl`
//! with telemetry fixtures, and assert on the commands' stdout.
//!
//! The library internals (`telemetry::read_logs`, `LogsFilter`,
//! `stats::compute_stats`) already have unit and integration coverage; these
//! tests pin the CLI wiring that nothing else covers: flag parsing, log-dir
//! resolution from the global config, `--since`/`--until` plumbing, and both
//! output formats.

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output};

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use tempfile::TempDir;

// ──────────────────────────────────────────────────────────────────────────────
// Fixtures
// ──────────────────────────────────────────────────────────────────────────────

/// An isolated `$HOME` with `$HOME/.needle/logs` created and ready to seed.
struct IsolatedHome(TempDir);

impl IsolatedHome {
    fn new() -> Self {
        let dir = TempDir::new().expect("failed to create isolated temp home");
        fs::create_dir_all(default_log_dir(dir.path())).expect("failed to create isolated log dir");
        IsolatedHome(dir)
    }

    /// Run the compiled `needle` binary with `HOME` isolated to this temp dir.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_needle"))
            .args(args)
            .env("HOME", self.0.path())
            .output()
            .expect("failed to spawn needle binary")
    }
}

/// Default telemetry log dir: `workspace.home` defaults to `$HOME/.needle`,
/// and the file sink's log dir defaults to `workspace.home/logs`.
fn default_log_dir(home: &std::path::Path) -> PathBuf {
    home.join(".needle").join("logs")
}

/// Write one `.jsonl` log fixture into the isolated log dir.
fn seed_log(home: &IsolatedHome, filename: &str, events: &[Value]) {
    let path = default_log_dir(home.0.path()).join(filename);
    let mut file = fs::File::create(&path).expect("failed to create log fixture");
    for event in events {
        let line = serde_json::to_string(event).expect("failed to serialize fixture event");
        writeln!(file, "{line}").expect("failed to write fixture event");
    }
}

/// A minimal well-formed telemetry event (all non-optional fields present).
fn base_event(worker: &str, event_type: &str, at: DateTime<Utc>, data: Value) -> Value {
    json!({
        "timestamp": at.to_rfc3339(),
        "event_type": event_type,
        "worker_id": worker,
        "session_id": "sess0001",
        "sequence": 1,
        "bead_id": null,
        "workspace": "/test/workspace",
        "data": data,
    })
}

/// An `agent.dispatched` event, the row-source for `needle stats`.
fn dispatch_event(worker: &str, bead: &str, template_version: &str, at: DateTime<Utc>) -> Value {
    let mut event = base_event(
        worker,
        "agent.dispatched",
        at,
        json!({"template_name": "pluck", "template_version": template_version}),
    );
    event["bead_id"] = json!(bead);
    event
}

/// An `outcome.classified` event correlating a bead to its outcome.
fn outcome_event(worker: &str, bead: &str, outcome: &str, at: DateTime<Utc>) -> Value {
    let mut event = base_event(
        worker,
        "outcome.classified",
        at,
        json!({"outcome": outcome}),
    );
    event["bead_id"] = json!(bead);
    event
}

/// An `effort.recorded` event carrying token and cost totals for a bead.
fn effort_event(
    worker: &str,
    bead: &str,
    tokens_in: u64,
    tokens_out: u64,
    cost_usd: f64,
    at: DateTime<Utc>,
) -> Value {
    let mut event = base_event(
        worker,
        "effort.recorded",
        at,
        json!({
            "tokens_in": tokens_in,
            "tokens_out": tokens_out,
            "estimated_cost_usd": cost_usd,
        }),
    );
    event["bead_id"] = json!(bead);
    event
}

// ──────────────────────────────────────────────────────────────────────────────
// Assertion helpers
// ──────────────────────────────────────────────────────────────────────────────

fn assert_succeeded(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context}: needle exited with {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Parse `--format json` (JSON Lines) stdout into a list of events.
fn stdout_jsonl(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("stdout line was not valid JSON"))
        .collect()
}

fn stdout_string(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// ──────────────────────────────────────────────────────────────────────────────
// `needle logs`
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_logs_json_outputs_all_seeded_events() {
    let home = IsolatedHome::new();
    let now = Utc::now();
    seed_log(
        &home,
        "alpha-sess0001-2026-09-03.jsonl",
        &[
            base_event("alpha", "worker.started", now, json!({})),
            base_event("alpha", "heartbeat", now, json!({})),
        ],
    );
    seed_log(
        &home,
        "bravo-sess0002-2026-09-03.jsonl",
        &[base_event("bravo", "worker.started", now, json!({}))],
    );

    let output = home.run(&["logs", "--format", "json"]);
    assert_succeeded(&output, "logs --format json");

    let events = stdout_jsonl(&output);
    assert_eq!(events.len(), 3, "expected every seeded event on stdout");
    let mut types: Vec<&str> = events
        .iter()
        .map(|e| e["event_type"].as_str().expect("event_type is a string"))
        .collect();
    types.sort();
    assert_eq!(types, vec!["heartbeat", "worker.started", "worker.started"]);
}

#[test]
fn test_logs_table_mode_prints_one_line_per_event() {
    let home = IsolatedHome::new();
    let now = Utc::now();
    seed_log(
        &home,
        "alpha-sess0001-2026-09-03.jsonl",
        &[
            {
                let mut e = base_event("alpha", "worker.started", now, json!({}));
                e["bead_id"] = json!("needle-abc123");
                e
            },
            base_event("alpha", "heartbeat", now, json!({})),
        ],
    );

    let output = home.run(&["logs"]);
    assert_succeeded(&output, "logs (default table format)");

    let stdout = stdout_string(&output);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "one human-readable line per event");
    // The Normal-format line carries the bead context next to the event.
    assert!(
        stdout.contains("needle-abc123"),
        "table output should include the bead id; got: {stdout}"
    );
}

#[test]
fn test_logs_exact_field_filter() {
    let home = IsolatedHome::new();
    let now = Utc::now();
    seed_log(
        &home,
        "workers-2026-09-03.jsonl",
        &[
            base_event("alpha", "worker.started", now, json!({})),
            base_event("alpha", "heartbeat", now, json!({})),
            base_event("bravo", "worker.started", now, json!({})),
        ],
    );

    let output = home.run(&[
        "logs",
        "--filter",
        "event_type=worker.started",
        "--format",
        "json",
    ]);
    assert_succeeded(&output, "logs --filter event_type=worker.started");

    let events = stdout_jsonl(&output);
    assert_eq!(events.len(), 2, "only the exact-matching events survive");
    assert!(events.iter().all(|e| e["event_type"] == "worker.started"));
}

#[test]
fn test_logs_glob_and_predicate_filters_are_anded() {
    let home = IsolatedHome::new();
    let now = Utc::now();
    seed_log(
        &home,
        "workers-2026-09-03.jsonl",
        &[
            {
                let mut e = base_event("alpha", "agent.dispatched", now, json!({}));
                e["bead_id"] = json!("needle-1");
                e
            },
            {
                let mut e = base_event("alpha", "outcome.classified", now, json!({}));
                e["bead_id"] = json!("needle-1");
                e
            },
            {
                let mut e = base_event("bravo", "agent.dispatched", now, json!({}));
                e["bead_id"] = json!("needle-2");
                e
            },
        ],
    );

    // Glob on event_type plus an exact-match predicate: both must hold.
    let output = home.run(&[
        "logs",
        "--filter",
        "agent.*",
        "--filter",
        "worker_id=alpha",
        "--format",
        "json",
    ]);
    assert_succeeded(&output, "logs with glob + exact filter");

    let events = stdout_jsonl(&output);
    assert_eq!(events.len(), 1, "predicates must be ANDed");
    assert_eq!(events[0]["event_type"], "agent.dispatched");
    assert_eq!(events[0]["worker_id"], "alpha");
}

#[test]
fn test_logs_since_and_until_bound_the_window() {
    let home = IsolatedHome::new();
    let now = Utc::now();
    let old = now - Duration::hours(48);
    seed_log(
        &home,
        "alpha-sess0001-2026-09-01.jsonl",
        &[base_event("alpha", "worker.started", old, json!({}))],
    );
    seed_log(
        &home,
        "alpha-sess0002-2026-09-03.jsonl",
        &[base_event("alpha", "heartbeat", now, json!({}))],
    );

    // Relative --until is anchored backwards too: `--until 24h` means
    // "up to 24h ago", so only the 48h-old event survives this window.
    let output = home.run(&[
        "logs", "--since", "72h", "--until", "24h", "--format", "json",
    ]);
    assert_succeeded(&output, "logs --since 72h --until 24h");
    let events = stdout_jsonl(&output);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "worker.started");

    // Only the recent event is inside `--since 1h`.
    let output = home.run(&["logs", "--since", "1h", "--format", "json"]);
    assert_succeeded(&output, "logs --since 1h");
    let events = stdout_jsonl(&output);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "heartbeat");
}

#[test]
fn test_logs_since_accepts_absolute_timestamps() {
    let home = IsolatedHome::new();
    let now = Utc::now();
    seed_log(
        &home,
        "alpha-sess0001-2026-09-03.jsonl",
        &[
            base_event(
                "alpha",
                "worker.started",
                now - Duration::hours(48),
                json!({}),
            ),
            base_event("alpha", "heartbeat", now, json!({})),
        ],
    );

    // Absolute RFC 3339 timestamps bound the same window the relative
    // forms do — only the recent event is inside `--since` an hour ago.
    let cutoff = (now - Duration::hours(1)).to_rfc3339();
    let output = home.run(&["logs", "--since", &cutoff, "--format", "json"]);
    assert_succeeded(&output, "logs --since <rfc3339>");
    let events = stdout_jsonl(&output);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "heartbeat");
}

#[test]
fn test_logs_empty_log_dir_reports_no_matches() {
    let home = IsolatedHome::new();

    let output = home.run(&["logs"]);
    assert_succeeded(&output, "logs against an empty log dir");
    assert!(
        stdout_string(&output).contains("No matching events found."),
        "expected the empty-result message; got: {}",
        stdout_string(&output)
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// `needle stats`
// ──────────────────────────────────────────────────────────────────────────────

/// Seed a two-worker store: alpha has 1 pass + 1 fail with effort data,
/// bravo has a single timeout.
fn seed_stats_store(home: &IsolatedHome) {
    let now = Utc::now();
    seed_log(
        home,
        "stats-fixture-2026-09-03.jsonl",
        &[
            dispatch_event("alpha", "needle-1", "pluck-v2", now),
            dispatch_event("alpha", "needle-2", "pluck-v2", now),
            outcome_event("alpha", "needle-1", "Success", now),
            outcome_event("alpha", "needle-2", "Failure", now),
            effort_event("alpha", "needle-1", 100, 50, 0.5, now),
            effort_event("alpha", "needle-2", 10, 5, 0.02, now),
            dispatch_event("bravo", "needle-3", "pluck-v1", now),
            outcome_event("bravo", "needle-3", "Timeout", now),
            effort_event("bravo", "needle-3", 200, 100, 1.0, now),
        ],
    );
}

#[test]
fn test_stats_by_worker_correlates_dispatch_outcome_and_effort() {
    let home = IsolatedHome::new();
    seed_stats_store(&home);

    let output = home.run(&["stats", "--by", "worker", "--format", "json"]);
    assert_succeeded(&output, "stats --by worker --format json");

    let rows: Vec<Value> =
        serde_json::from_str(&stdout_string(&output)).expect("stats JSON was not an array");
    assert_eq!(rows.len(), 2, "one row per worker");

    let alpha = rows
        .iter()
        .find(|r| r["key"] == "alpha")
        .expect("alpha row");
    assert_eq!(alpha["beads"], 2);
    assert_eq!(alpha["pass"], 1);
    assert_eq!(alpha["fail"], 1);
    assert_eq!(alpha["timeout"], 0);
    assert!((alpha["pass_rate"].as_f64().expect("pass_rate") - 0.5).abs() < 1e-9);
    assert!((alpha["avg_tokens"].as_f64().expect("avg_tokens") - 82.5).abs() < 1e-9);
    assert!((alpha["avg_cost_usd"].as_f64().expect("avg_cost_usd") - 0.26).abs() < 1e-9);

    let bravo = rows
        .iter()
        .find(|r| r["key"] == "bravo")
        .expect("bravo row");
    assert_eq!(bravo["beads"], 1);
    assert_eq!(bravo["timeout"], 1);
    assert_eq!(bravo["pass"], 0);
    assert!((bravo["avg_tokens"].as_f64().expect("avg_tokens") - 300.0).abs() < 1e-9);
}

#[test]
fn test_stats_by_template_version_groups_on_dispatch_data() {
    let home = IsolatedHome::new();
    seed_stats_store(&home);

    let output = home.run(&["stats", "--by", "template_version", "--format", "json"]);
    assert_succeeded(&output, "stats --by template_version --format json");

    let rows: Vec<Value> =
        serde_json::from_str(&stdout_string(&output)).expect("stats JSON was not an array");
    assert_eq!(rows.len(), 2, "one row per template version");

    let v2 = rows
        .iter()
        .find(|r| r["key"] == "pluck-v2")
        .expect("pluck-v2 row");
    assert_eq!(v2["beads"], 2);
    let v1 = rows
        .iter()
        .find(|r| r["key"] == "pluck-v1")
        .expect("pluck-v1 row");
    assert_eq!(v1["beads"], 1);
    assert_eq!(v1["timeout"], 1);
}

#[test]
fn test_stats_by_task_type_groups_on_template_name() {
    let home = IsolatedHome::new();
    seed_stats_store(&home);

    let output = home.run(&["stats", "--by", "task_type", "--format", "json"]);
    assert_succeeded(&output, "stats --by task_type --format json");

    // Every fixture dispatch carries template_name "pluck", so the whole
    // store collapses into a single row whose counts are the union of the
    // per-worker rows.
    let rows: Vec<Value> =
        serde_json::from_str(&stdout_string(&output)).expect("stats JSON was not an array");
    assert_eq!(rows.len(), 1, "one row for the single template_name");

    let pluck = &rows[0];
    assert_eq!(pluck["key"], "pluck");
    assert_eq!(pluck["beads"], 3);
    assert_eq!(pluck["pass"], 1);
    assert_eq!(pluck["fail"], 1);
    assert_eq!(pluck["timeout"], 1);
    assert!((pluck["pass_rate"].as_f64().expect("pass_rate") - (1.0 / 3.0)).abs() < 1e-9);
}

#[test]
fn test_stats_table_mode_renders_dimension_rows() {
    let home = IsolatedHome::new();
    seed_stats_store(&home);

    let output = home.run(&["stats", "--by", "worker"]);
    assert_succeeded(&output, "stats --by worker (default table format)");

    let stdout = stdout_string(&output);
    assert!(stdout.contains("WORKER"), "missing header: {stdout}");
    assert!(stdout.contains("BEADS"), "missing column header: {stdout}");
    assert!(stdout.contains("alpha"), "missing alpha row: {stdout}");
    assert!(stdout.contains("bravo"), "missing bravo row: {stdout}");
}

#[test]
fn test_stats_since_excludes_out_of_window_beads() {
    let home = IsolatedHome::new();
    let two_days_ago = Utc::now() - Duration::hours(48);
    seed_log(
        &home,
        "stats-stale-2026-09-01.jsonl",
        &[
            dispatch_event("alpha", "needle-old", "pluck-v2", two_days_ago),
            outcome_event("alpha", "needle-old", "Success", two_days_ago),
        ],
    );

    // The only bead dispatched 48h ago falls outside `--since 24h`, so no
    // dispatch remains to anchor a row.
    let output = home.run(&["stats", "--by", "worker", "--since", "24h"]);
    assert_succeeded(&output, "stats --since 24h");
    assert!(
        stdout_string(&output).contains("No telemetry data found."),
        "expected the empty-result message; got: {}",
        stdout_string(&output)
    );

    // Widening the window to cover the dispatch makes the row appear.
    let output = home.run(&[
        "stats", "--by", "worker", "--since", "72h", "--format", "json",
    ]);
    assert_succeeded(&output, "stats --since 72h --format json");
    let rows: Vec<Value> =
        serde_json::from_str(&stdout_string(&output)).expect("stats JSON was not an array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["key"], "alpha");
    assert_eq!(rows[0]["pass"], 1);
}

#[test]
fn test_stats_empty_store_reports_no_data() {
    let home = IsolatedHome::new();

    let output = home.run(&["stats", "--by", "worker"]);
    assert_succeeded(&output, "stats against an empty log dir");
    assert!(
        stdout_string(&output).contains("No telemetry data found."),
        "expected the empty-result message; got: {}",
        stdout_string(&output)
    );
}
