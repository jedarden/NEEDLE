//! One configured state root for every persistent state writer (ADR-030
//! decision 5, N-T52).
//!
//! Before N-T52 each state writer resolved `$HOME/.needle/...` on its own:
//! provider health read `HOME` in `provider_health.rs`, gate health in
//! `gate_health.rs`, the ledger in `telemetry/file_sink.rs`, and so on. A test
//! that spawned NEEDLE with an isolated `HOME` was safe, but any writer
//! reached in-process — or any spawned test that forgot the `HOME` override —
//! landed fixture rows and fixture state files in the live fleet's state
//! directory. That is exactly what ADR-030 decision 5 forbids: provider-health
//! and gate-health fixtures were found under the real `~/.needle/state` on
//! 2026-09-13, and the live ledger holds rows with worker
//! `echo-test-test-worker` and workspace `.`.
//!
//! The contract now is one resolution, one override:
//!
//! 1. `NEEDLE_STATE_DIR` (the test harness override, and the operator's
//!    escape hatch), then
//! 2. the configured `paths.state_dir` value (published by
//!    [`crate::config::ConfigLoader`] at load; restart-required), then
//! 3. the historical default `$HOME/.needle`.
//!
//! Every writer resolves beneath this root by joining its established
//! relative path (`state/gate-health`, `state/provider-health`,
//! `state/experiments`, `state/evidence_routing`, `state/heartbeats`,
//! `state/workers.json`, `logs`), so production layouts are byte-identical
//! and an override relocates all of them at once. Writers whose root is a
//! configuration value rather than a default — the attempt-archive spool
//! (`attempt_archive.spool_dir`) and the attempt journals
//! (`attempt_history::history_path`, workspace-rooted because the retry
//! prompt and the bead-rs data mirror read them back from the workspace) —
//! redirect beneath [`override_root`] only while an override is in force, so
//! a fixture suite cannot touch them and an operator's explicit paths keep
//! working.
//!
//! Two more pieces share this module because they enforce the same decision:
//!
//! * [`is_fixture_row`] — ledger consumers (stats, routing evidence, and the
//!   proposal generator once N-T53 lands) skip `attempt.resolved` rows whose
//!   worker id ends with `-test-worker` or whose workspace is `.`, so rows
//!   already written before the override existed age out of every reading.
//! * [`ensure_harness_isolation`] — a spawned binary that announces a test
//!   harness (`NEEDLE_TEST_HARNESS`, set by
//!   `tests/integration_spawn/isolation.rs`) must carry an explicit
//!   `NEEDLE_STATE_DIR`, and one pointing beneath the harness's captured real
//!   home is refused at startup. The failure is deliberately loud and early:
//!   an unisolated spawn used to write anyway and be discovered days later in
//!   the live ledger.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Environment override for the state root (ADR-030 decision 5).
pub const STATE_DIR_ENV: &str = "NEEDLE_STATE_DIR";

/// Environment marker set by a test harness that spawns the NEEDLE binary.
///
/// The value, when non-empty, is the real home directory captured by the
/// harness before it overrode the child's environment; [`ensure_harness_isolation`]
/// refuses a state root beneath it.
pub const TEST_HARNESS_ENV: &str = "NEEDLE_TEST_HARNESS";

/// The `paths.state_dir` value published by [`crate::config::ConfigLoader`].
///
/// A mutex rather than a `OnceLock` because config loads are repeatable
/// (process start, and the restart-required reload path re-reads the file);
/// the value is a whole-path replacement, so last load wins.
static CONFIGURED: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Publish the configured `paths.state_dir` value.
///
/// Called by [`crate::config::ConfigLoader`] after a config load. `None`
/// clears a previous value; the environment override always wins over
/// whatever is published here.
pub fn set_configured(state_dir: Option<PathBuf>) {
    let mut configured = CONFIGURED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *configured = state_dir;
}

/// The configured `paths.state_dir` value, if one was published.
pub fn configured() -> Option<PathBuf> {
    CONFIGURED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Expand a leading `~` using the current `HOME`, and normalize an empty
/// override to `None`.
fn expanded(raw: std::ffi::OsString) -> Option<PathBuf> {
    let trimmed = raw.to_string_lossy().trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(crate::util::expand_tilde(&trimmed)))
}

/// The state-root override in force for this process, when there is one:
/// `NEEDLE_STATE_DIR`, else the configured `paths.state_dir`.
///
/// `Some` only when an override exists, so workspace-rooted writers (the
/// attempt journals) and configuration-rooted writers (the spool) can detect
/// "a harness or operator redirected the state root" without re-deriving the
/// precedence here.
pub fn override_root() -> Option<PathBuf> {
    std::env::var_os(STATE_DIR_ENV)
        .and_then(expanded)
        .or_else(configured)
}

/// The historical default root: `$HOME/.needle` (temp dir when `HOME` is
/// unset, matching `dirs_or_home` elsewhere in the config layer).
fn default_root() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join(".needle"),
        _ => std::env::temp_dir().join(".needle"),
    }
}

/// The single state root every persistent writer resolves beneath.
///
/// Precedence: `NEEDLE_STATE_DIR`, then the configured `paths.state_dir`,
/// then `$HOME/.needle`.
pub fn state_root() -> PathBuf {
    override_root().unwrap_or_else(default_root)
}

/// Resolve a state root for a component that historically accepted a custom
/// `workspace.home` fallback. An explicit state-root override wins; without
/// one, retaining the caller's fallback preserves the pre-N-T52 behavior for
/// in-process callers and custom operator homes.
pub fn root_for(fallback: &Path) -> PathBuf {
    override_root().unwrap_or_else(|| fallback.to_path_buf())
}

/// Resolve the telemetry/log-writer directory while honoring an explicit
/// state-root override. A configured legacy log directory remains effective
/// only when no central state root is active.
pub fn logs_dir_for(configured: Option<&Path>, fallback_root: &Path) -> PathBuf {
    if override_root().is_some() {
        logs_dir()
    } else {
        configured
            .map(Path::to_path_buf)
            .unwrap_or_else(|| fallback_root.join("logs"))
    }
}

/// The ledger's log directory: `<root>/logs`.
pub fn logs_dir() -> PathBuf {
    state_root().join("logs")
}

/// The worker registry directory: `<root>/state`.
///
/// [`crate::registry::Registry::new`] joins `workers.json`.
pub fn registry_dir() -> PathBuf {
    state_root().join("state")
}

/// The heartbeat directory: `<root>/state/heartbeats`.
pub fn heartbeats_dir() -> PathBuf {
    state_root().join("state").join("heartbeats")
}

/// The gate-health state directory: `<root>/state/gate-health`.
pub fn gate_health_dir() -> PathBuf {
    state_root().join("state").join("gate-health")
}

/// The provider-health state directory: `<root>/state/provider-health`.
pub fn provider_health_dir() -> PathBuf {
    state_root().join("state").join("provider-health")
}

/// The experiment receipt directory: `<root>/state/experiments`.
pub fn experiments_dir() -> PathBuf {
    state_root().join("state").join("experiments")
}

/// The routing-evidence receipt directory: `<root>/state/evidence_routing`.
pub fn evidence_routing_dir() -> PathBuf {
    state_root().join("state").join("evidence_routing")
}

/// The spool root an override redirects to: `<root>/spool`.
///
/// Without an override the attempt archive keeps its own configured
/// `attempt_archive.spool_dir`; only an explicit state-root override moves it.
pub fn spool_dir_under_override() -> Option<PathBuf> {
    override_root().map(|root| root.join("spool"))
}

/// The attempt-journal root an override redirects to: `<root>/attempt-journals`.
///
/// Without an override the journals stay in their bead workspace (`.beads/traces`),
/// which is where the retry prompt and the bead-rs data mirror read them.
pub fn attempt_journals_under_override() -> Option<PathBuf> {
    override_root().map(|root| root.join("attempt-journals"))
}

/// Whether an `attempt.resolved` ledger row is a fixture row (ADR-030
/// decision 5).
///
/// Consumers — stats, routing evidence, and the proposal generator — skip
/// rows whose worker id ends with `-test-worker` or whose workspace is the
/// relative root `.`. Both shapes came from tests that resolved state from
/// the real home before the override existed; the rows age out of the files,
/// and every reader excludes them in the meantime.
pub fn is_fixture_row(worker: &str, workspace: &str) -> bool {
    worker.ends_with("-test-worker") || workspace == "."
}

/// Fail fast when a spawned test binary lacks an isolated state root.
///
/// `NEEDLE_TEST_HARNESS` is set by the shared spawn harness
/// (`tests/integration_spawn/isolation.rs`). Under that marker the binary
/// requires:
///
/// * an explicit `NEEDLE_STATE_DIR` — a harness that overrode nothing gets a
///   clear refusal instead of silent writes beneath the real home, and
/// * a state root outside the real home captured by the harness (the guard
///   variable's value), so even an override naming `~/.needle` is refused.
///
/// `Ok` when the marker is absent: production workers never set it.
pub fn ensure_harness_isolation() -> Result<(), String> {
    let Some(raw_guard) = std::env::var_os(TEST_HARNESS_ENV) else {
        return Ok(());
    };

    let Some(raw_state) = std::env::var_os(STATE_DIR_ENV).and_then(|v| {
        let lossy = v.to_string_lossy().trim().to_string();
        if lossy.is_empty() {
            None
        } else {
            Some(v)
        }
    }) else {
        return Err(format!(
            "{TEST_HARNESS_ENV} is set but {STATE_DIR_ENV} is not: a spawned test must override \
             the state directory to an isolated path (see \
             tests/integration_spawn/isolation.rs) — refusing to write beneath the real home"
        ));
    };

    let real_home = PathBuf::from(&raw_guard);
    if !real_home.as_os_str().is_empty() {
        let root = expanded(raw_state).unwrap_or_default();
        if root.starts_with(&real_home) {
            return Err(format!(
                "{STATE_DIR_ENV} ({}) is beneath the real home ({}) while {TEST_HARNESS_ENV} is \
                 set — refusing to write beneath the real home",
                root.display(),
                real_home.display(),
            ));
        }
    }

    Ok(())
}
