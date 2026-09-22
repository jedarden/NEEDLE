//! Focused behavioral contracts for N-T52 (ADR-030 decision 5): one
//! configured state directory for every persistent state writer.
//!
//! The bead's acceptance command is this target. Three behaviors live here:
//!
//! 1. With `NEEDLE_STATE_DIR` set, every listed writer resolves beneath the
//!    override and a spawned fixture writes nothing beneath a read-only fake
//!    home.
//! 2. A spawned binary announcing a test harness without an isolated state
//!    root fails fast with a clear message (`NEEDLE_TEST_HARNESS` guard).
//! 3. Ledger consumers — routing evidence and stats — skip `attempt.resolved`
//!    rows whose worker id ends with `-test-worker` or whose workspace is
//!    `.`.

use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::TempDir;

/// The single process-wide lock guarding the environment this harness swaps.
///
/// Every test takes it for its whole body: the resolver reads `HOME` and
/// `NEEDLE_STATE_DIR` at call time, and one test's leftover override would
/// decide another test's result.
static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn env_lock() -> MutexGuard<'static, ()> {
    let lock = ENV_LOCK.get_or_init(|| Mutex::new(()));
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Restores the swapped variables when dropped, then releases the lock —
/// restore-before-unlock, the order the in-crate `test_env` guard learned the
/// hard way.
struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn take() -> Self {
        let lock = env_lock();
        let saved = [
            "HOME",
            needle::state_dir::STATE_DIR_ENV,
            needle::state_dir::TEST_HARNESS_ENV,
        ]
        .iter()
        .map(|&key| (key, std::env::var_os(key)))
        .collect();
        Self { saved, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn needle_binary() -> std::ffi::OsString {
    std::env::var_os("NEXTEST_BIN_EXE_needle")
        .unwrap_or_else(|| std::ffi::OsString::from(env!("CARGO_BIN_EXE_needle")))
}

/// The real home of this process — the value the harness guard carries so the
/// spawned binary can refuse a state root beneath it.
fn real_home() -> OsString {
    std::env::var_os("HOME").unwrap_or_default()
}

/// Assert `path` is beneath `root` and say both in the failure.
fn assert_beneath(root: &Path, path: PathBuf, writer: &str) {
    assert!(
        path.starts_with(root),
        "{writer} resolved to {} which is not beneath the override {}",
        path.display(),
        root.display()
    );
}

/// `attempt.resolved` ledger row with fixture or real identity.
fn ledger_line(worker: &str, workspace: &str, adapter: &str, outcome: &str) -> String {
    serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event_type": "attempt.resolved",
        "worker_id": worker,
        "session_id": "a1b2c3d4",
        "sequence": 1,
        "data": {
            "worker": worker,
            "workspace": workspace,
            "adapter": adapter,
            "outcome": outcome,
        }
    })
    .to_string()
}

/// Today as the ledger file's `YYYY-MM-DD` suffix, so the reader's date-bound
/// window includes the file.
fn today_suffix() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

// ─── Resolution beneath the override ─────────────────────────────────────────

#[test]
fn every_listed_writer_resolves_beneath_the_override() {
    let _env = EnvGuard::take();
    let fixture = TempDir::new().unwrap();
    let override_root = fixture.path().join("state-root");
    fs::create_dir_all(&override_root).unwrap();
    std::env::set_var(needle::state_dir::STATE_DIR_ENV, &override_root);

    let workspace = TempDir::new().unwrap();
    let cases: Vec<(String, PathBuf)> = vec![
        (
            "provider_health::state_file_path".into(),
            needle::provider_health::state_file_path("adapter-key"),
        ),
        (
            "gate_health::state_file_path".into(),
            needle::gate_health::state_file_path(workspace.path()).unwrap(),
        ),
        (
            "experiments::default_state_dir".into(),
            needle::experiments::default_state_dir(),
        ),
        (
            "evidence_routing::default_state_dir".into(),
            needle::evidence_routing::default_state_dir(),
        ),
        ("state_dir::logs_dir".into(), needle::state_dir::logs_dir()),
        (
            "state_dir::heartbeats_dir".into(),
            needle::state_dir::heartbeats_dir(),
        ),
        (
            "state_dir::registry_dir".into(),
            needle::state_dir::registry_dir(),
        ),
        (
            "state_dir::gate_health_dir".into(),
            needle::state_dir::gate_health_dir(),
        ),
        (
            "state_dir::provider_health_dir".into(),
            needle::state_dir::provider_health_dir(),
        ),
        (
            "state_dir::experiments_dir".into(),
            needle::state_dir::experiments_dir(),
        ),
        (
            "state_dir::evidence_routing_dir".into(),
            needle::state_dir::evidence_routing_dir(),
        ),
        (
            "state_dir::spool_dir_under_override".into(),
            needle::state_dir::spool_dir_under_override().unwrap(),
        ),
        (
            "state_dir::attempt_journals_under_override".into(),
            needle::state_dir::attempt_journals_under_override().unwrap(),
        ),
    ];
    for (writer, path) in cases {
        assert_beneath(&override_root, path, &writer);
    }

    // Attempt journals relocate per workspace beneath the override.
    let bead: needle::types::BeadId = "needle-e8f408e1".into();
    assert_beneath(
        &override_root,
        needle::attempt_history::history_path(workspace.path(), &bead),
        "attempt_history::history_path",
    );
    assert_beneath(
        &override_root,
        needle::attempt_history::lessons_path(workspace.path(), &bead),
        "attempt_history::lessons_path",
    );
    assert_beneath(
        &override_root,
        needle::validation::predispatch::snapshot_path(workspace.path(), &bead),
        "validation::predispatch::snapshot_path",
    );
}

#[test]
fn override_precedence_is_env_then_config_then_home() {
    let _env = EnvGuard::take();
    let fixture = TempDir::new().unwrap();
    let env_root = fixture.path().join("env-root");
    let configured_root = fixture.path().join("configured-root");
    let fake_home = fixture.path().join("home");
    fs::create_dir_all(&env_root).unwrap();
    fs::create_dir_all(&fake_home).unwrap();
    std::env::set_var("HOME", &fake_home);
    std::env::remove_var(needle::state_dir::STATE_DIR_ENV);
    needle::state_dir::set_configured(None);

    // Default: $HOME/.needle.
    assert_eq!(
        needle::state_dir::state_root(),
        fake_home.join(".needle"),
        "without an override the root is $HOME/.needle"
    );

    // Configured beats the default. (Published at config load; set directly
    // here because the harness has no ConfigLoader to do it.)
    needle::state_dir::set_configured(Some(configured_root.clone()));
    assert_eq!(
        needle::state_dir::state_root(),
        configured_root,
        "paths.state_dir beats the $HOME/.needle default"
    );

    // The environment beats the configuration.
    std::env::set_var(needle::state_dir::STATE_DIR_ENV, &env_root);
    assert_eq!(
        needle::state_dir::state_root(),
        env_root,
        "NEEDLE_STATE_DIR beats paths.state_dir"
    );

    needle::state_dir::set_configured(None);
}

#[test]
fn journal_roundtrip_under_the_override_stays_inside_it() {
    let _env = EnvGuard::take();
    let fixture = TempDir::new().unwrap();
    let override_root = fixture.path().join("state-root");
    fs::create_dir_all(&override_root).unwrap();
    std::env::set_var(needle::state_dir::STATE_DIR_ENV, &override_root);

    // A workspace outside the override entirely: the journal must still land
    // beneath the override, and the symmetric loader must read it back.
    let workspace = Path::new("/opt/some-real-workspace");
    let bead: needle::types::BeadId = "needle-e8f408e1".into();
    let record = needle::attempt_history::AttemptRecord {
        schema_version: needle::attempt_history::SCHEMA_VERSION,
        attempt_id: "attempt-1".to_string(),
        recorded_at: chrono::Utc::now().to_rfc3339(),
        worker: "harness-worker".to_string(),
        adapter: "fixture".to_string(),
        model: None,
        outcome: "verified_success".to_string(),
        terminal_reason: None,
        exit_code: 0,
        requested_action: "Released".to_string(),
        commits: Vec::new(),
        duration_ms: 1_000,
        failure_summary: None,
        failure_evidence: None,
        wip_patch: None,
    };
    needle::attempt_history::append_local(workspace, &bead, &record).unwrap();
    assert_beneath(
        &override_root,
        needle::attempt_history::history_path(workspace, &bead),
        "attempt journal write",
    );
    let loaded = needle::attempt_history::load_local(workspace, &bead).unwrap();
    assert_eq!(
        loaded.len(),
        1,
        "the journal round-trips beneath the override"
    );
    assert_eq!(loaded[0].attempt_id, "attempt-1");
}

// ─── Spawned fixture vs a read-only fake home ────────────────────────────────

#[test]
fn spawned_fixture_writes_nothing_under_a_read_only_fake_home() {
    let _env = EnvGuard::take();
    let fixture = TempDir::new().unwrap();
    let fake_home = fixture.path().join("home");
    let override_root = fixture.path().join("state-root");
    fs::create_dir_all(&fake_home).unwrap();
    fs::create_dir_all(&override_root).unwrap();

    // A read-only fake home is only a hard refusal for a non-root runner; as
    // root every chmod is advisory. Either way the post-run emptiness
    // assertion below catches a writer that resolved to the fake home.
    fs::set_permissions(&fake_home, fs::Permissions::from_mode(0o555)).ok();

    let status = Command::new(needle_binary())
        .arg("status")
        .arg("--format")
        .arg("json")
        .current_dir(fixture.path())
        .env("HOME", &fake_home)
        .env("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT", fixture.path())
        .env(needle::state_dir::STATE_DIR_ENV, &override_root)
        .env(needle::state_dir::TEST_HARNESS_ENV, real_home())
        .output()
        .expect("spawn needle status");

    assert!(
        status.status.success(),
        "needle status under the override should succeed; stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let leaked: Vec<PathBuf> = fs::read_dir(&fake_home)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .map(|e| e.path())
        .collect();
    assert!(
        leaked.is_empty(),
        "the read-only fake home received writes: {leaked:?}"
    );

    // Writable again so the TempDir teardown can remove the fixture.
    fs::set_permissions(&fake_home, fs::Permissions::from_mode(0o755)).ok();

    // `status` is read-only when the fixture has no workers. The writer
    // resolution contract is covered above; this assertion is deliberately
    // only about the forbidden fake HOME receiving no files.
}

// ─── The harness guard ───────────────────────────────────────────────────────

#[test]
fn spawned_test_without_the_override_fails_fast_with_a_clear_message() {
    let _env = EnvGuard::take();
    let fixture = TempDir::new().unwrap();

    let started = std::time::Instant::now();
    let output = Command::new(needle_binary())
        .arg("--version")
        .current_dir(fixture.path())
        .env("HOME", fixture.path())
        .env("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT", fixture.path())
        .env(needle::state_dir::TEST_HARNESS_ENV, real_home())
        .env_remove(needle::state_dir::STATE_DIR_ENV)
        .output()
        .expect("spawn needle without a state override");

    assert!(
        !output.status.success(),
        "a spawned test without NEEDLE_STATE_DIR must fail"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the refusal must be fast, not a slow discovery; took {:?}",
        started.elapsed()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(needle::state_dir::STATE_DIR_ENV),
        "the refusal should name the missing variable; stderr: {stderr}"
    );
}

#[test]
fn spawned_test_beneath_the_real_home_is_refused() {
    let _env = EnvGuard::take();
    let fixture = TempDir::new().unwrap();
    let home = real_home();
    assert!(
        !home.is_empty(),
        "this harness needs a HOME to capture as the real home"
    );

    let output = Command::new(needle_binary())
        .arg("--version")
        .current_dir(fixture.path())
        .env("HOME", fixture.path())
        .env("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT", fixture.path())
        .env(needle::state_dir::TEST_HARNESS_ENV, &home)
        .env(
            needle::state_dir::STATE_DIR_ENV,
            PathBuf::from(&home).join(".needle").join("state"),
        )
        .output()
        .expect("spawn needle with a state override beneath the real home");

    assert!(
        !output.status.success(),
        "a state root beneath the real home must be refused under the guard"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("beneath the real home"),
        "the refusal should say why; stderr: {stderr}"
    );
}

#[test]
fn harness_guard_contract_matches_the_spawned_behavior() {
    let _env = EnvGuard::take();
    let fixture = TempDir::new().unwrap();

    // No guard: production, always fine.
    std::env::remove_var(needle::state_dir::TEST_HARNESS_ENV);
    std::env::remove_var(needle::state_dir::STATE_DIR_ENV);
    assert!(needle::state_dir::ensure_harness_isolation().is_ok());

    // Guard without an override: refused, naming the variable.
    std::env::set_var(needle::state_dir::TEST_HARNESS_ENV, "real-home-value");
    let error = needle::state_dir::ensure_harness_isolation().unwrap_err();
    assert!(error.contains(needle::state_dir::STATE_DIR_ENV), "{error}");

    // Guard with an override beneath the guarded home: refused.
    let guarded_home = fixture.path().join("outer");
    std::fs::create_dir_all(&guarded_home).unwrap();
    std::env::set_var(needle::state_dir::TEST_HARNESS_ENV, &guarded_home);
    std::env::set_var(
        needle::state_dir::STATE_DIR_ENV,
        guarded_home.join("state-root"),
    );
    let error = needle::state_dir::ensure_harness_isolation().unwrap_err();
    assert!(error.contains("beneath the real home"), "{error}");

    // Guard with an override outside the guarded home: accepted.
    std::env::set_var(
        needle::state_dir::STATE_DIR_ENV,
        fixture.path().join("state-root"),
    );
    assert!(needle::state_dir::ensure_harness_isolation().is_ok());

    std::env::remove_var(needle::state_dir::TEST_HARNESS_ENV);
    std::env::remove_var(needle::state_dir::STATE_DIR_ENV);
}

// ─── Fixture rows out of the ledger consumers ────────────────────────────────

#[test]
fn routing_evidence_skips_fixture_rows() {
    let _env = EnvGuard::take();
    let fixture = TempDir::new().unwrap();
    let log_dir = fixture.path().join("logs");
    fs::create_dir_all(&log_dir).unwrap();

    fs::write(
        log_dir.join(format!("alpha-ab12cd34-{}.jsonl", today_suffix())),
        format!(
            "{}\n{}\n{}\n",
            ledger_line("echo-test-test-worker", ".", "codex", "verified_success"),
            ledger_line("needle-alpha", ".", "flash", "verified_success"),
            ledger_line(
                "needle-alpha",
                "/repos/commitgraph",
                "codex",
                "work_failure"
            ),
        ),
    )
    .unwrap();

    let rows = needle::evidence_routing::timestamped_ledger_rows(&log_dir, 30);
    assert_eq!(
        rows.len(),
        1,
        "only the real row survives; got {:?}",
        rows.iter().map(|r| r.data.clone()).collect::<Vec<_>>()
    );
    assert_eq!(
        rows[0].data.get("adapter").and_then(|v| v.as_str()),
        Some("codex"),
        "the surviving row is the real attempt"
    );
}

#[test]
fn stats_skip_fixture_rows() {
    let _env = EnvGuard::take();
    let fixture = TempDir::new().unwrap();
    let log_dir = fixture.path().join("logs");
    fs::create_dir_all(&log_dir).unwrap();

    fs::write(
        log_dir.join(format!("alpha-ab12cd34-{}.jsonl", today_suffix())),
        format!(
            "{}\n{}\n",
            ledger_line("echo-test-test-worker", ".", "codex", "verified_success"),
            ledger_line("needle-alpha", "/repos/pdftract", "flash", "work_failure"),
        ),
    )
    .unwrap();

    let events =
        needle::telemetry::read_logs(&log_dir, None, None, None).expect("parse the fixture ledger");
    assert_eq!(
        events.len(),
        2,
        "the file holds both rows; consumers filter"
    );

    let rows =
        needle::stats::compute_attempt_stats(&events, needle::stats::StatsDimension::Adapter);
    assert_eq!(rows.len(), 1, "fixture rows are not aggregated: {rows:?}");
    assert_eq!(rows[0].key, "flash");
    assert_eq!(rows[0].fail, 1);
}

#[test]
fn fixture_row_predicate_matches_the_contaminated_shapes() {
    // Pure predicate coverage, no environment: both shapes the 2026-09-12..14
    // ledger actually held.
    assert!(needle::state_dir::is_fixture_row(
        "echo-test-test-worker",
        "."
    ));
    assert!(needle::state_dir::is_fixture_row("needle-alpha", "."));
    assert!(needle::state_dir::is_fixture_row(
        "anything-at-all-test-worker",
        "/repos/anything"
    ));
    assert!(!needle::state_dir::is_fixture_row(
        "needle-alpha",
        "/repos/commitgraph"
    ));
    assert!(needle::state_dir::is_fixture_row("echo-test", "."));
    assert!(!needle::state_dir::is_fixture_row(
        "needle-alpha",
        "./nested"
    ));
}
