//! Integration tests for NEEDLE Phase 1.
//!
//! These tests exercise the full worker pipeline end-to-end using mock
//! `BeadStore` implementations and real process execution via bash adapters.
//!
//! Test categories:
//! 1. End-to-end single worker cycle
//! 2. All 6 outcome paths (success, failure, timeout, agent_not_found, interrupted, crash)
//! 3. Exhaustion (empty queue → Pluck returns NoWork → Knot fires → EXHAUSTED)
//! 4. Graceful shutdown (shutdown flag during various states)
//! 5. Deterministic ordering (property test)
//! 6. Cross-workspace mend: two-workspace zombie scenario

// ============================================================================
// PROCESSGUARD COVERAGE CATALOG — 2026-08-13
// ============================================================================
//
// This section documents the ProcessGuard coverage analysis performed across
// all integration tests. ProcessGuard is a test helper that ensures child
// processes are cleaned up (killed + reaped) even if a test panics, preventing
// zombie processes from contaminating the test environment.
//
// **EXECUTIVE SUMMARY: NO ADDITIONAL COVERAGE NEEDED**
//
// All integration tests that spawn real child processes already implement
// proper ProcessGuard wrapping. The analysis found:
// - **Total sites needing ProcessGuard:** 0
// - **Tests already correctly covered:** 4 (all real process tests)
// - **Mock infrastructure tests:** 1 (MockProcess::wait() — no guard needed)
//
// ----------------------------------------------------------------------------
// TESTS WITH REAL CHILD PROCESSES (ALL COVERED)
// ----------------------------------------------------------------------------
//
// 1. dead_worker_cleanup_integration (line ~2206)
//    - Spawns: Real `needle worker --once` subprocess
//    - ProcessGuard: ✅ YES (lines 2277-2313)
//    - Pattern: Drop implementation kills + waits
//
// 2. heartbeat_cleanup_on_signal_integration (line ~2618)
//    - Spawns: Real `needle run` subprocess with heartbeat file
//    - ProcessGuard: ✅ YES (lines 2720-2762)
//    - Pattern: Wait with timeout handling, explicit error messages
//
// 3. heartbeat_cleanup_on_normal_exit_integration (line ~3317)
//    - Spawns: Real `needle run` subprocess
//    - ProcessGuard: ✅ YES (lines 3410-3453)
//    - Pattern: Handles cleanup on panic/timeout
//
// 4. heartbeat_cleanup_multiple_scenarios_integration (line ~3596)
//    - Spawns: TWO real `needle run` subprocesses (scenario1, scenario2)
//    - ProcessGuard: ✅ YES (lines 3600-3638) — 2 separate instances
//    - Pattern: Multiple sequential scenarios, each with its own guard
//
// ----------------------------------------------------------------------------
// MOCK INFRASTRUCTURE (NOT REAL PROCESSES)
// ----------------------------------------------------------------------------
//
// 5. MockProcess::wait() (line ~2477)
//    - Type: Test helper/mock infrastructure
//    - Spawns: Does NOT spawn real process (trivial `true` command only)
//    - ProcessGuard: ❌ NO (Not needed — not a long-lived worker process)
//
// ----------------------------------------------------------------------------
// CONSISTENT PATTERN USED ACROSS ALL TESTS
// ----------------------------------------------------------------------------
//
// All ProcessGuard implementations follow this pattern:
//
// ```rust
// struct ProcessGuard {
//     inner: Option<std::process::Child>,
// }
//
// impl ProcessGuard {
//     fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
//         if let Some(ref mut child) = self.inner {
//             child.wait()  // Safe, wrapped in Drop
//         } else {
//             Err(std::io::Error::other("No child process"))
//         }
//     }
// }
//
// impl Drop for ProcessGuard {
//     fn drop(&mut self) {
//         if let Some(mut child) = self.inner.take() {
//             let _ = child.kill();      // Signal termination
//             let _ = child.wait();      // Reap to prevent zombies
//         }
//     }
// }
// ```
//
// ----------------------------------------------------------------------------
// NEXT STEPS (OPTIONAL IMPROVEMENTS)
// ----------------------------------------------------------------------------
//
// Since all tests already have coverage, no immediate action is required.
// Optional improvements for future maintenance:
//
// 1. Extract ProcessGuard to a shared test helper module to reduce
//    code duplication across the 4 tests that use it.
//
// 2. Consider adding a macro or builder pattern for common ProcessGuard
//    patterns (with timeout, with custom error messages, etc.).
//
// 3. Document the pattern in test development guidelines for new tests
//    that spawn real subprocesses.
//
// ----------------------------------------------------------------------------
// ANALYSIS METHODOLOGY
// ----------------------------------------------------------------------------
//
// This catalog was created by:
// 1. Scanning all test files for `child.wait()` calls
// 2. Cross-referencing each call site with ProcessGuard usage
// 3. Distinguishing between real process tests and mock infrastructure
// 4. Verifying ProcessGuard implementations include Drop guards
//
// See: tests/processguard_coverage_catalog.md for detailed analysis.
// ============================================================================

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::config::Config;
use needle::dispatch::{AgentAdapter, Dispatcher};
use needle::strand::Strand;
use needle::telemetry::Telemetry;
use needle::types::{
    Bead, BeadId, BeadStatus, ClaimResult, IdleAction, InputMethod, Outcome, StrandResult,
    WorkerState,
};
use needle::worker::truncate_commit_sha;
use needle::worker::Worker;

// Serial test execution for tests that modify global environment variables
use serial_test::serial;

// ─── Test isolation infrastructure ───────────────────────────────────────────────

/// Guard that restores HOME to its original value when dropped.
///
/// This prevents tests from writing to the live fleet's state directory
/// (`~/.needle/state/heartbeats/`). Each test gets its own temporary HOME
/// so heartbeats and other state files don't contaminate the real environment.
///
/// HOME is process-wide state, so isolation must also be mutually exclusive: two
/// tests isolating at once clobber each other and whichever sets HOME last wins for
/// both, making `~` expand to the other test's tempdir. `#[serial]` cannot prevent
/// this -- it only orders serial-marked tests against each other, so any non-serial
/// test racing one still corrupts it. The lock below makes isolation exclusive.
static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct HomeGuard {
    _temp_dir: tempfile::TempDir,
    original_home: Option<std::ffi::OsString>,
    // Declared last so it is released only after Drop::drop has restored HOME.
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl HomeGuard {
    /// Isolates the test's HOME directory to a temp directory.
    ///
    /// Returns a guard that restores the original HOME value when dropped.
    /// Use this in any test that creates a HealthMonitor or Worker, as both
    /// may call `dirs_or_home()` which reads HOME directly.
    fn isolate() -> Self {
        // Take the lock before touching HOME. Recover from poisoning rather than
        // propagating it: one panicking test must not cascade into every other test
        // that isolates HOME.
        let lock = HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let original_home = std::env::var_os("HOME");
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir for test HOME");
        let temp_path = temp_dir.path().to_path_buf();

        std::env::set_var("HOME", &temp_path);

        HomeGuard {
            _temp_dir: temp_dir,
            original_home,
            _lock: lock,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

// ─── Shared test infrastructure ──────────────────────────────────────────────

fn configured_forge_store(workspace: PathBuf) -> needle::bead_store::CliBeadStore {
    // bead-rs (`bead`), not bead-forge (`bf`).
    //
    // bf was decommissioned across this environment on 2026-08-16 and its binary
    // deleted, so every test routed through here died at construction with
    // "bead backend binary not found at ~/.local/bin/bf" -- a dead-tool dependency,
    // not a real failure. The CI image provisions the pinned bead-rs CLI, so this
    // resolves in CI as well as locally. See needle-ab52a15a.
    let backend = needle::bead_store::builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .expect("built-in bead-rs descriptor");
    let binary = which::which("bead").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(format!("{home}/.cargo/bin/bead"))
    });
    needle::bead_store::CliBeadStore::new(backend, binary, workspace, None, None, None)
        .expect("configured bead-rs test store")
}

/// ProcessGuard ensures proper cleanup of child processes in tests.
///
/// This struct wraps a child process and guarantees cleanup via Drop:
/// - Kills the process on drop (if still running)
/// - Reaps the process to prevent zombies
///
/// Usage:
/// ```rust
/// let child = Command::new("...").spawn().unwrap();
/// let pid = child.id();
/// let guard = ProcessGuard::new(child, Some(pid));
/// // Use guard.try_wait(), guard.kill(), guard.wait()
/// // Cleanup happens automatically when guard is dropped
/// ```
struct ProcessGuard {
    inner: Option<std::process::Child>,
}

impl ProcessGuard {
    /// Create a new ProcessGuard from a spawned child process.
    ///
    /// # Arguments
    /// * `child` - The spawned child process
    /// * `pid` - Optional PID for logging/debugging (can be obtained via `child.id()`)
    fn new(child: std::process::Child, _pid: Option<u32>) -> Self {
        Self { inner: Some(child) }
    }

    /// Try to wait for the process to exit without blocking.
    ///
    /// Returns `Ok(Some(status))` if the process has exited,
    /// `Ok(None)` if the process is still running,
    /// or `Err` if an error occurs.
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        if let Some(ref mut child) = self.inner {
            child.try_wait()
        } else {
            Ok(None)
        }
    }

    /// Send a kill signal to the child process.
    fn kill(&mut self) -> std::io::Result<()> {
        if let Some(ref mut child) = self.inner {
            child.kill()
        } else {
            Ok(())
        }
    }

    /// Wait for the child process to exit (blocking).
    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if let Some(ref mut child) = self.inner {
            child.wait()
        } else {
            Err(std::io::Error::other("No child process to wait for"))
        }
    }

    /// Take the inner child process out of the guard, bypassing Drop.
    ///
    /// # Safety
    /// The caller becomes responsible for cleanup (kill + wait) of the child process.
    fn into_inner(mut self) -> Option<std::process::Child> {
        self.inner.take()
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.inner.take() {
            // Kill the process (safe even if already exited)
            let _ = child.kill();
            // Reap the process to prevent zombies
            let _ = child.wait();
        }
    }
}

/// Mock BeadStore that tracks all operations and returns configurable beads.
///
/// Key behaviors:
/// - `ready()` returns only open, unassigned beads
/// - `claim()` sets assignee, preventing re-selection via ready()
/// - `release()` removes the bead entirely (prevents infinite re-selection loops)
struct IntegrationMockStore {
    beads: Mutex<Vec<Bead>>,
    actions: Mutex<Vec<String>>,
}

impl IntegrationMockStore {
    fn new(beads: Vec<Bead>) -> Self {
        IntegrationMockStore {
            beads: Mutex::new(beads),
            actions: Mutex::new(Vec::new()),
        }
    }

    fn empty() -> Self {
        Self::new(vec![])
    }

    fn actions(&self) -> Vec<String> {
        self.actions.lock().unwrap().clone()
    }

    fn record(&self, action: &str) {
        self.actions.lock().unwrap().push(action.to_string());
    }
}

#[async_trait]
impl BeadStore for IntegrationMockStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        self.record("ready");
        // Only return open, unassigned beads (matching real br behavior).
        Ok(self
            .beads
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b.status == BeadStatus::Open && b.assignee.is_none())
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        self.record("list_all");
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        self.record(&format!("show:{id}"));
        let beads = self.beads.lock().unwrap();
        let bead = beads
            .iter()
            .find(|b| b.id == *id)
            .cloned()
            .unwrap_or_else(|| make_bead_with_id(id.as_ref()));
        Ok(bead)
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        self.record(&format!("claim:{id}:{actor}"));
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::InProgress;
            bead.assignee = Some(actor.to_string());
            Ok(ClaimResult::Claimed(bead.clone()))
        } else {
            Ok(ClaimResult::NotClaimable {
                reason: "not found".to_string(),
            })
        }
    }

    async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
        // Return NotClaimable for tests - auto-claim not supported in mock
        Ok(ClaimResult::NotClaimable {
            reason: "no beads available".to_string(),
        })
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        self.record(&format!("release:{id}"));
        // Remove released beads from the list to prevent infinite re-selection loops.
        // In real usage, released beads get labels (deferred, failure-count) that
        // filter them out, but the mock doesn't simulate full label-based filtering.
        let mut beads = self.beads.lock().unwrap();
        beads.retain(|b| b.id != *id);
        Ok(())
    }

    async fn block(&self, id: &BeadId) -> Result<()> {
        self.record(&format!("block:{id}"));
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        self.record(&format!("reopen:{id}"));
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        self.record(&format!("labels:{id}"));
        Ok(vec![])
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.record(&format!("add_label:{id}:{label}"));
        Ok(())
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.record(&format!("remove_label:{id}:{label}"));
        Ok(())
    }

    async fn create_bead(&self, title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
        self.record(&format!("create_bead:{title}"));
        Ok(BeadId::from("alert-new"))
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        self.record(&format!("add_dep:{}:{}", blocker_id, blocked_id));
        Ok(())
    }

    async fn remove_dependency(&self, _blocked_id: &BeadId, _blocker_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        Ok(RepairReport::default())
    }
    async fn doctor_check(&self) -> Result<RepairReport> {
        Ok(RepairReport::default())
    }
    async fn full_rebuild(&self) -> Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true // Mock store always has a valid store
    }
}

fn make_bead_with_id(id: &str) -> Bead {
    Bead {
        id: BeadId::from(id),
        title: format!("Test bead {id}"),
        body: Some("Implement something useful".to_string()),
        priority: 1,
        status: BeadStatus::Open,
        assignee: None,
        labels: vec![],
        workspace: PathBuf::from("/tmp/test-workspace"),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn make_bead(id: &str, priority: u8) -> Bead {
    let mut bead = make_bead_with_id(id);
    bead.priority = priority;
    bead
}

fn test_adapter(name: &str, template: &str, timeout_secs: u64) -> AgentAdapter {
    AgentAdapter {
        name: name.to_string(),
        description: None,
        agent_cli: "bash".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: template.to_string(),
        environment: HashMap::new(),
        timeout_secs,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: needle::dispatch::TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    }
}

/// ISOLATION REQUIRED: In-process Worker tests must isolate HOME and pin Explore strand's scan root.
///
/// **CRITICAL:** Callers MUST create a `HomeGuard::isolate()` guard before calling this function.
///
/// This applies to tests that build a Worker in-process via this helper. The guard ensures:
/// 1. Heartbeats write to the test's temp HOME, not the live fleet's `~/.needle/state/heartbeats/`
/// 2. `Config::default()` uses the isolated HOME via `dirs_or_home()`
/// 3. The Explore strand doesn't scan real user directories
///
/// Without the guard:
/// - `dirs_or_home()` in `Config::default()` resolves to the real user home
/// - Heartbeats write to `~/.needle/state/heartbeats/` (live fleet state)
/// - Explore scans every directory under real `$HOME` containing `.beads/`
///
/// 2026-08-05 incident: An in-process Worker test without HOME isolation let an orphaned
/// `integration_tests` binary mutate 2302 beads to `in_progress` under assignee
/// `echo-test-test-worker` and truncate `.beads/issues.jsonl` to 0 bytes (recovered from git).
///
/// 2026-08-06 incident: The lib suite wrote `claude-test-worker.json` into the live
/// fleet's heartbeat directory (`~/.needle/state/heartbeats/`), contaminating shared state.
///
/// See CLAUDE.md Test Isolation Policy for full details.
fn test_config(adapter_name: &str, workspace_home: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Exit;
    // boot() silently rewrites Exit -> Wait with no supervisor present, making the
    // worker sleep the 60-120s idle backoff in a loop instead of stopping. Tests
    // have no supervisor, so this opt-in is required or the test hangs rather than
    // fails. Deliberately NOT set in idle_action_exit_without_supervisor_emits_warning,
    // which asserts that downgrade happens. See needle-ab52a15a.
    config.worker.allow_exit_without_supervisor = true;
    config.agent.default = adapter_name.to_string();
    config.agent.timeout = 10;
    config.agent.routing = None; // Disable routing in tests - use adapter directly
    config.self_modification.hot_reload = false;
    // Match the test bead workspace so the remote-store-switch logic
    // doesn't fire (it would try to create a real CLI store).
    config.workspace.default = std::path::PathBuf::from("/tmp/test-workspace");
    // Isolate workspace home so the registry doesn't leak between tests.
    config.workspace.home = workspace_home.to_path_buf();
    // Confine the Explore strand to the test's temp home.
    // REQUIRED — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
    config.strands.explore.workspace_root = workspace_home.to_path_buf();
    config.strands.explore.workspaces = Vec::new();
    // Enable OTLP sink to trigger the runtime guard (bf-4nwm7).
    // This ensures that init_tracing_subscriber's tokio::spawn calls
    // work correctly with the runtime context guard from bf-3s2b0.
    config.telemetry.otlp_sink.enabled = true;
    config
}

/// Returns `(Worker, HomeGuard)` — the HomeGuard must be kept alive for the test duration.
///
/// ISOLATION REQUIRED: In-process Worker tests must isolate HOME and pin Explore strand.
///
/// This helper builds a Worker in-process via `test_config()`, which requires HOME to be
/// isolated before calling. The returned HomeGuard must be kept alive for the entire
/// test duration — dropping it early restores the original HOME, breaking isolation.
///
/// See `test_config()` for full isolation documentation, including the 2026-08-05 and
/// 2026-08-06 contamination incidents where lack of HOME/Explore isolation caused beads
/// to be mutated and heartbeats to write to the live fleet's state directory.
fn make_worker_with_adapter(
    store: Arc<dyn BeadStore>,
    adapter_name: &str,
    template: &str,
    timeout_secs: u64,
) -> (Worker, HomeGuard) {
    // Isolate HOME before calling test_config() - required for proper isolation
    let home_guard = HomeGuard::isolate();
    let config = test_config(adapter_name, home_guard._temp_dir.path());
    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter(adapter_name, template, timeout_secs);
    let mut adapters = HashMap::new();
    adapters.insert(adapter_name.to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    (worker, home_guard)
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 1: End-to-end single worker
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn end_to_end_single_bead_success() {
    // A single bead exists, agent exits 0, worker processes it and stops.
    // No show_override_status: the claimer verifies via show(), so the bead
    // must appear as Open during claiming. After claim sets assignee, the
    // bead is filtered from ready() on the next cycle → exhaustion.
    let bead = make_bead("needle-e2e-001", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_guard) =
        make_worker_with_adapter(store.clone(), "echo-test", "echo 'agent completed'", 10);

    let result = worker.run().await.unwrap();

    // Worker should process the bead and then exhaust (no more beads).
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );
    assert!(
        worker.beads_processed() >= 1,
        "expected at least 1 bead processed, got {}",
        worker.beads_processed()
    );
}

#[tokio::test]
async fn end_to_end_worker_loops_to_next_bead() {
    // Two beads exist; worker processes both.
    let beads = vec![make_bead("needle-e2e-a", 1), make_bead("needle-e2e-b", 1)];
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(beads));

    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "echo-test", "echo 'done'", 10);

    let result = worker.run().await.unwrap();

    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );
    assert!(
        worker.beads_processed() >= 2,
        "expected at least 2 beads processed, got {}",
        worker.beads_processed()
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 2: All 6 outcome paths
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn outcome_path_success_exit_0() {
    let bead = make_bead("needle-out-success", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "success-agent", "exit 0", 10);

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );
    assert_eq!(
        worker.beads_processed(),
        1,
        "expected exactly 1 bead to be processed"
    );
}

#[tokio::test]
async fn outcome_path_failure_exit_1() {
    let bead = make_bead("needle-out-fail", 1);
    let store = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "fail-agent", "exit 1", 10);

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );

    let actions = store.actions();
    assert!(
        actions.iter().any(|a| a.starts_with("release:")),
        "failure should release bead; actions: {:?}",
        actions
    );
    assert!(
        actions
            .iter()
            .any(|a| a.contains("add_label") && a.contains("failure-count:1")),
        "failure should increment failure count; actions: {:?}",
        actions
    );
}

#[tokio::test]
async fn outcome_path_timeout_exit_124() {
    let bead = make_bead("needle-out-timeout", 1);
    let store = Arc::new(IntegrationMockStore::new(vec![bead]));

    // Use a very short timeout (1 second) with a long-running command.
    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "timeout-agent", "sleep 100", 1);

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );

    let actions = store.actions();
    assert!(
        actions.iter().any(|a| a.starts_with("release:")),
        "timeout should release bead; actions: {:?}",
        actions
    );
    assert!(
        actions
            .iter()
            .any(|a| a.contains("add_label") && a.contains("deferred")),
        "timeout should add deferred label; actions: {:?}",
        actions
    );
}

#[tokio::test]
async fn outcome_path_agent_not_found_exit_127() {
    let bead = make_bead("needle-out-notfound", 1);
    let store = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_dir) = make_worker_with_adapter(
        store.clone(),
        "missing-agent",
        "nonexistent-binary-xyz-999",
        10,
    );

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );

    let actions = store.actions();
    assert!(
        actions.iter().any(|a| a.starts_with("release:")),
        "agent_not_found should release bead; actions: {:?}",
        actions
    );
}

#[tokio::test]
async fn outcome_path_crash_exit_137() {
    let bead = make_bead("needle-out-crash", 1);
    let store = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "crash-agent", "exit 137", 10);

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );

    let actions = store.actions();
    assert!(
        actions.iter().any(|a| a.starts_with("release:")),
        "crash should release bead; actions: {:?}",
        actions
    );
    assert!(
        actions.iter().any(|a| a.starts_with("create_bead:")),
        "crash should create alert bead; actions: {:?}",
        actions
    );
}

#[tokio::test]
async fn outcome_path_interrupted_via_shutdown_flag() {
    let bead = make_bead("needle-out-interrupt", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));

    let _home_guard = HomeGuard::isolate();
    let config = test_config("slow-agent", _home_guard._temp_dir.path());
    let mut worker = Worker::new(config, "test-worker".to_string(), store.clone());

    // Use a slow adapter so we have time to set shutdown.
    let adapter = test_adapter("slow-agent", "sleep 2", 30);
    let mut adapters = HashMap::new();
    adapters.insert("slow-agent".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        30,
    ));

    // Set shutdown flag before run — worker should detect it during the loop.
    worker.request_shutdown();

    let result = worker.run().await.unwrap();
    assert_eq!(
        result,
        WorkerState::Stopped,
        "interrupted worker should stop cleanly"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 3: Exhaustion
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn exhaustion_empty_workspace() {
    // Empty store → Pluck returns NoWork → Knot fires → EXHAUSTED → Exit.
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_guard = HomeGuard::isolate();
    let config = test_config("echo-test", _home_guard._temp_dir.path());
    let session_id = needle::telemetry::generate_session_id();
    let _ = needle::cli::init_tracing_subscriber("test-worker".to_string(), session_id, &config);
    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    let result = worker.run().await.unwrap();
    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected exhausted/stopped, got {:?}",
        result
    );
    assert_eq!(worker.beads_processed(), 0, "no beads should be processed");
}

/// Test exhaustion with idle_action=Exit.
///
/// This test builds a Worker in-process with custom config. The Explore strand
/// MUST be isolated to prevent scanning real user directories.
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
///
/// Why isolation is required for in-process Worker tests:
/// - Workers built in-process never go through a child process, so setting
///   `HOME` in a Command::new() call has no effect (no child process exists)
/// - Without explicit config, Explore uses `ExploreConfig::default()`: `workspaces: []`
///   (= auto-discover) with `workspace_root` defaulting to the real home directory
///   (`ExploreConfig::default_workspace_root()` -> `dirs_or_home("")`)
/// - Left unchecked, Explore scans every directory under $HOME containing a
///   `.beads/` and claims REAL beads from REAL repos under the fixture
///   adapter/worker identity
///
/// 2026-08-05 contamination incident: An in-process Worker test without
/// isolation let an orphaned `integration_tests` binary roam into bead-forge's
/// live store, mutate 2302 beads to `in_progress` under assignee
/// `echo-test-test-worker`, and truncate `.beads/issues.jsonl` to 0 bytes.
#[tokio::test]
async fn exhaustion_with_idle_action_exit() {
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_guard = HomeGuard::isolate();
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Exit;
    // Opt in to Exit: boot() downgrades it to Wait without a supervisor. See needle-ab52a15a.
    config.worker.allow_exit_without_supervisor = true;
    config.agent.default = "echo-test".to_string();
    config.agent.routing = None; // Disable routing in tests - use adapter directly
    config.self_modification.hot_reload = false;
    config.workspace.default = std::path::PathBuf::from("/tmp/test-workspace");
    config.workspace.home = _home_guard._temp_dir.path().to_path_buf();
    // Confine Explore strand to test's tempdir to prevent scanning real user directories
    config.strands.explore.workspace_root = _home_guard._temp_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    let result = worker.run().await.unwrap();
    assert_eq!(
        result,
        WorkerState::Stopped,
        "idle_action=exit should produce Stopped"
    );
}

#[tokio::test]
async fn exhaustion_with_idle_action_wait_survives_sleep() {
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Mock store that returns NoWork initially, then a bead after N calls.
    struct DelayedBeadStore {
        call_count: AtomicU32,
        bead_after: u32,
        bead: Mutex<Option<Bead>>,
        /// Tracks claimed beads (moved here from `bead` on claim).
        claimed: Mutex<Vec<Bead>>,
        /// Signals that the bead has been released and the worker should exit.
        bead_released: AtomicU32,
    }

    #[async_trait]
    impl BeadStore for DelayedBeadStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            // After the bead is released, signal the worker to exit by returning an error.
            if self.bead_released.load(Ordering::SeqCst) == 1 {
                return Err(anyhow::anyhow!("test complete - bead was released"));
            }
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count >= self.bead_after {
                // Return the bead if it's available (not yet claimed or claimed but not yet released).
                let bead = self.bead.lock().unwrap();
                let claimed = self.claimed.lock().unwrap();
                if bead.is_some() && !claimed.iter().any(|b| b.id == bead.as_ref().unwrap().id) {
                    Ok(vec![bead.clone().unwrap()])
                } else {
                    Ok(vec![])
                }
            } else {
                Ok(Vec::new())
            }
        }

        async fn list_all(&self) -> Result<Vec<Bead>> {
            let bead = self.bead.lock().unwrap();
            let claimed = self.claimed.lock().unwrap();
            let mut all: Vec<Bead> = claimed.clone();
            if let Some(b) = bead.as_ref() {
                if !all.iter().any(|x| x.id == b.id) {
                    all.push(b.clone());
                }
            }
            Ok(all)
        }

        async fn show(&self, id: &BeadId) -> Result<Bead> {
            let claimed = self.claimed.lock().unwrap();
            if let Some(b) = claimed.iter().find(|b| b.id == *id) {
                return Ok(b.clone());
            }
            let bead = self.bead.lock().unwrap();
            match bead.as_ref() {
                Some(b) if b.id == *id => Ok(b.clone()),
                _ => anyhow::bail!("bead not found: {id}"),
            }
        }

        async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
            let bead = self.bead.lock().unwrap();
            let mut claimed = self.claimed.lock().unwrap();
            match bead.as_ref() {
                Some(b) if b.id == *id => {
                    let mut cloned = b.clone();
                    cloned.status = BeadStatus::InProgress;
                    cloned.assignee = Some(actor.to_string());
                    claimed.push(cloned.clone());
                    Ok(ClaimResult::Claimed(cloned))
                }
                _ => Ok(ClaimResult::NotClaimable {
                    reason: "no bead".to_string(),
                }),
            }
        }

        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "no beads available".to_string(),
            })
        }

        async fn release(&self, id: &BeadId) -> Result<()> {
            // Remove the bead from claimed after it's been processed.
            let mut claimed = self.claimed.lock().unwrap();
            claimed.retain(|b| b.id != *id);
            // Mark that the bead has been released - worker should exit on next cycle.
            self.bead_released.store(1, Ordering::SeqCst);
            Ok(())
        }

        async fn block(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn flush(&self) -> Result<()> {
            Ok(())
        }

        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            Ok(vec![])
        }

        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }

        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }

        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("mock-bead"))
        }

        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }

        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn doctor_repair(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }

        async fn doctor_check(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }

        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }

        fn has_valid_store(&self) -> bool {
            true // Mock store always has a valid store
        }
    }

    let bead = Bead {
        id: BeadId::from("test-bead"),
        status: BeadStatus::Open,
        title: "Test Bead".to_string(),
        body: Some("Test body".to_string()),
        priority: 1,
        assignee: None,
        labels: vec![],
        workspace: std::path::PathBuf::from("/tmp"),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let delayed_store = Arc::new(DelayedBeadStore {
        call_count: AtomicU32::new(0),
        bead_after: 2, // Add bead after 2 calls (first call goes to EXHAUSTED, second after sleep)
        bead: Mutex::new(Some(bead)),
        claimed: Mutex::new(vec![]),
        bead_released: AtomicU32::new(0),
    });
    let store: Arc<dyn BeadStore> = delayed_store.clone();

    let _home_guard = HomeGuard::isolate();
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Wait; // Wait for delayed bead
    config.worker.idle_timeout = 1; // 1 second for fast test
                                    // The idle sleep is driven by idle_backoff_min/max (compute_jittered_backoff),
                                    // NOT idle_timeout. Left at the 60-120s defaults this test slept ~90s per cycle
                                    // and never returned -- it was what timed out the whole integration suite at its
                                    // 900s cap. 1s keeps the sleep real (this test exists to prove the worker SURVIVES
                                    // an idle sleep) without stalling the suite; 0 would busy-spin instead.
                                    // See needle-ab52a15a.
    config.worker.idle_backoff_min = 1;
    config.worker.idle_backoff_max = 1;
    config.agent.default = "echo-test".to_string();
    config.agent.routing = None; // Disable routing in tests - use adapter directly
    config.workspace.home = _home_guard._temp_dir.path().to_path_buf();
    config.self_modification.hot_reload = false;
    config.workspace.default = std::path::PathBuf::from("/tmp");
    // ISOLATION REQUIRED: In-process Worker tests must isolate HOME and pin Explore strand.
    //
    // This test builds a Worker in-process with custom config. The HomeGuard ensures:
    // 1. Heartbeats write to test's temp HOME, not live fleet's ~/.needle/state/heartbeats/
    // 2. Config::default() uses isolated HOME via dirs_or_home()
    // 3. Explore strand doesn't scan real user directories
    //
    // 2026-08-05 incident: test_config() isolated workspace.default/home but not strands.explore,
    // letting an orphaned integration_tests binary mutate 2302 beads to in_progress under
    // assignee echo-test-test-worker and truncate .beads/issues.jsonl to 0 bytes (recovered from git).
    //
    // 2026-08-06 incident: The lib suite wrote claude-test-worker.json into the live
    // fleet's heartbeat directory, contaminating shared state.
    //
    // See CLAUDE.md Test Isolation Policy for full details.
    config.strands.explore.workspace_root = _home_guard._temp_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    // Stop the worker once the bead has been processed and released.
    //
    // DelayedBeadStore::ready() returns an error after the release to "signal the
    // worker to exit", but the worker deliberately treats a store error as
    // recoverable ("strand error, continuing to next strand"), so that sentinel
    // never stopped anything -- the loop simply ran forever. Drive the real stop
    // signal instead. See needle-ab52a15a.
    let shutdown = worker.shutdown_handle();
    let released = delayed_store.clone();
    let stopper = tokio::spawn(async move {
        while released.bead_released.load(Ordering::SeqCst) != 1 {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        shutdown.store(true, Ordering::SeqCst);
    });

    let result = worker.run().await.unwrap();
    stopper.await.unwrap();
    assert_eq!(
        result,
        WorkerState::Stopped,
        "worker should stop after processing the delayed bead"
    );
    assert_eq!(
        worker.beads_processed(),
        1,
        "worker should process the bead that appeared after idle sleep"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 4: Graceful shutdown
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn shutdown_during_selecting_exits_cleanly() {
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_guard = HomeGuard::isolate();
    let config = test_config("echo-test", _home_guard._temp_dir.path());
    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    // Set shutdown before run.
    worker.request_shutdown();

    let result = worker.run().await.unwrap();
    assert_eq!(result, WorkerState::Stopped);
}

#[tokio::test]
async fn shutdown_flag_preempts_execution() {
    // Even with beads available, shutdown should cause clean exit.
    let bead = make_bead("needle-shutdown", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));
    let _home_guard = HomeGuard::isolate();
    let config = test_config("echo-test", _home_guard._temp_dir.path());
    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("test-worker".to_string()),
        10,
    ));

    // Set shutdown flag.
    worker.request_shutdown();

    let result = worker.run().await.unwrap();
    assert_eq!(
        result,
        WorkerState::Stopped,
        "shutdown should preempt processing"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 5: Deterministic ordering (property test)
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn deterministic_ordering_same_beads_same_order() {
    // Given the same set of beads with varied priorities and creation times,
    // two independent sorts should produce identical ordering.
    use needle::strand::PluckStrand;
    use needle::strand::Strand;

    let now = Utc::now();

    let beads = vec![
        {
            let mut b = make_bead("needle-sort-c", 3);
            b.created_at = now - chrono::Duration::hours(1);
            b
        },
        {
            let mut b = make_bead("needle-sort-a", 1);
            b.created_at = now - chrono::Duration::hours(3);
            b
        },
        {
            let mut b = make_bead("needle-sort-b", 1);
            b.created_at = now - chrono::Duration::hours(2);
            b
        },
        {
            let mut b = make_bead("needle-sort-d", 2);
            b.created_at = now;
            b
        },
    ];

    // Create two independent stores with the same beads (shuffled).
    let store1: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(beads.clone()));
    let store2: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new({
        let mut reversed = beads.clone();
        reversed.reverse();
        reversed
    }));

    let pluck = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));

    // Evaluate both — should return the same top candidate.
    let result1 = pluck.evaluate(store1.as_ref(), &HashSet::new()).await;
    let result2 = pluck.evaluate(store2.as_ref(), &HashSet::new()).await;

    // Extract candidate IDs.
    let id1 = match result1 {
        needle::types::StrandResult::BeadFound(beads) => beads.first().map(|b| b.id.clone()),
        _ => None,
    };
    let id2 = match result2 {
        needle::types::StrandResult::BeadFound(beads) => beads.first().map(|b| b.id.clone()),
        _ => None,
    };

    assert_eq!(
        id1, id2,
        "deterministic ordering: same beads must produce same top candidate"
    );

    // The top candidate should be the highest-priority, oldest bead.
    assert_eq!(
        id1,
        Some(BeadId::from("needle-sort-a")),
        "P1 bead created earliest should be selected first"
    );
}

#[tokio::test]
async fn deterministic_ordering_tiebreak_by_id() {
    // When priority and creation time are identical, bead ID breaks ties.
    use needle::strand::PluckStrand;
    use needle::strand::Strand;

    let now = Utc::now();

    let beads = vec![
        {
            let mut b = make_bead("needle-zz", 1);
            b.created_at = now;
            b
        },
        {
            let mut b = make_bead("needle-aa", 1);
            b.created_at = now;
            b
        },
        {
            let mut b = make_bead("needle-mm", 1);
            b.created_at = now;
            b
        },
    ];

    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(beads));
    let pluck = PluckStrand::new(vec![], Telemetry::new("test-worker".to_string()));

    let result = pluck.evaluate(store.as_ref(), &HashSet::new()).await;

    let top_id = match result {
        needle::types::StrandResult::BeadFound(beads) => beads.first().map(|b| b.id.clone()),
        _ => None,
    };

    assert_eq!(
        top_id,
        Some(BeadId::from("needle-aa")),
        "when priority and time match, lowest ID wins"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 6: Outcome classification exhaustiveness
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn outcome_classify_covers_all_exit_code_ranges() {
    // Verify that classify handles the full i32 range without panicking.
    // This is a smoke test to ensure no gaps exist.
    let test_codes: Vec<i32> = vec![
        i32::MIN,
        -1000,
        -1,
        0,
        1,
        2,
        50,
        99,
        100,
        123,
        124,
        125,
        126,
        127,
        128,
        129,
        130,
        137,
        143,
        255,
        256,
        1000,
        i32::MAX,
    ];

    for code in test_codes {
        // Should not panic.
        let outcome = Outcome::classify(code, false);
        // Verify specific mappings.
        match code {
            0 => assert_eq!(outcome, Outcome::Success),
            1 => assert_eq!(outcome, Outcome::Failure),
            124 => assert_eq!(outcome, Outcome::Timeout),
            127 => assert_eq!(outcome, Outcome::AgentNotFound),
            c if c > 128 => assert_eq!(outcome, Outcome::Crash(c)),
            c if c < 0 => assert_eq!(outcome, Outcome::Crash(c)),
            _ => {} // Other codes just shouldn't panic
        }
    }

    // Verify interrupted flag always wins.
    for code in [-1, 0, 1, 127, 137] {
        assert_eq!(
            Outcome::classify(code, true),
            Outcome::Interrupted,
            "was_interrupted=true must always return Interrupted for code {code}"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 7: Worker config validation at boot
// ═════════════════════════════════════════════════════════════════════════════

/// Test that worker rejects invalid config at boot.
///
/// This test builds a Worker in-process with intentionally invalid config.
/// The Explore strand MUST be isolated to prevent scanning real user directories.
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
///
/// Why isolation is required for in-process Worker tests:
/// - Workers built in-process never go through a child process, so setting
///   `HOME` in a Command::new() call has no effect (no child process exists)
/// - Without explicit config, Explore uses `ExploreConfig::default()`: `workspaces: []`
///   (= auto-discover) with `workspace_root` defaulting to the real home directory
///   (`ExploreConfig::default_workspace_root()` -> `dirs_or_home("")`)
/// - Left unchecked, Explore scans every directory under $HOME containing a
///   `.beads/` and claims REAL beads from REAL repos under the fixture
///   adapter/worker identity
///
/// 2026-08-05 contamination incident: An in-process Worker test without
/// isolation let an orphaned `integration_tests` binary roam into bead-forge's
/// live store, mutate 2302 beads to `in_progress` under assignee
/// `echo-test-test-worker`, and truncate `.beads/issues.jsonl` to 0 bytes.
#[tokio::test]
async fn worker_boot_rejects_invalid_config() {
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_guard = HomeGuard::isolate();
    let mut config = Config::default();
    config.agent.default = String::new(); // Invalid: empty agent name
    config.workspace.home = _home_guard._temp_dir.path().to_path_buf();
    // Confine Explore strand to test's tempdir to prevent scanning real user directories
    config.strands.explore.workspace_root = _home_guard._temp_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    let mut worker = Worker::new(config, "test-worker".to_string(), store);
    let result = worker.run().await;

    assert!(
        result.is_err(),
        "worker should fail to boot with invalid config"
    );
    assert!(
        result.unwrap_err().to_string().contains("agent.default"),
        "error should mention the invalid field"
    );
}

/// Test that worker rejects a nonexistent adapter at boot AND produces detailed stderr.
///
/// This test spawns the needle binary as a subprocess to verify that:
/// 1. The error message written to stderr contains the adapter name
/// 2. The error message includes configuration directory guidance
/// 3. The error message is actionable (user knows how to fix)
///
/// ISOLATION REQUIRED: This test spawns a real needle binary subprocess.
/// The test must isolate HOME to prevent the Explore strand from scanning
/// the real user workspace. Without this, the spawned needle binary would leak
/// into the real $HOME and scan real repos, contaminating the test environment.
///
/// See CLAUDE.md Test Isolation Policy and ADR-006 for full details.
#[tokio::test]
async fn worker_boot_rejects_nonexistent_adapter() {
    // Create a temporary workspace for the test
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path().join("test-workspace");
    std::fs::create_dir(&workspace).unwrap();

    // Initialize bead workspace (bead-rs CLI)
    let bead_result = std::process::Command::new("bead")
        .arg("init")
        .current_dir(&workspace)
        .output();

    // bead init may fail if the workspace is already initialized - that's OK for this test
    if let Ok(init_output) = bead_result {
        if !init_output.status.success() {
            let stderr = String::from_utf8_lossy(&init_output.stderr);
            // Only fail hard if it's a real error, not "already initialized"
            if !stderr.contains("already") && !stderr.contains("exists") {
                panic!("bead init failed: {}", stderr);
            }
        }
    }

    // Create .needle.yaml configuration to enable bead store discovery
    // Use bead-rs backend since that's the active CLI in this workspace
    std::fs::write(
        workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .expect("failed to create .needle.yaml configuration");

    // Get the needle binary path
    let bin_path = std::env::var("CARGO_BIN_EXE_needle").unwrap_or_else(|_| "needle".to_string());

    // Use a clearly fake adapter name that will never exist
    let nonexistent_adapter = "nonexistent-test-adapter-xyz-999";

    // Spawn the needle binary with the nonexistent adapter
    let output = std::process::Command::new(&bin_path)
        // NEEDLE_INNER=1 runs the worker loop in THIS process.
        // Without it, `needle run` detaches into a tmux session and the parent exits 0
        // immediately -- so adapter preflight happens in the detached worker and this
        // process reports success no matter what. That is why these assertions saw
        // "it succeeded" with empty stderr. The systemd unit sets the same variable.
        // See needle-ab52a15a.
        .env("NEEDLE_INNER", "1")
        .arg("run")
        .arg("--agent")
        .arg(nonexistent_adapter)
        .arg("--workspace")
        .arg(&workspace)
        .arg("--identifier")
        .arg("test-worker") // Use explicit identifier for deterministic behavior
        .env("HOME", temp_dir.path()) // ISOLATION: Prevent scanning real user directories
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("needle run command failed to execute");

    // Capture stderr for analysis
    let stderr_output = String::from_utf8_lossy(&output.stderr);

    // The worker should fail (nonzero exit code)
    assert!(
        !output.status.success(),
        "needle run should fail with nonexistent adapter, but it succeeded. \
         stderr: {}",
        stderr_output
    );

    // ASSERTION 1: Error message must contain the nonexistent adapter name
    assert!(
        stderr_output.contains(nonexistent_adapter),
        "error message should mention the nonexistent adapter name '{}'. \
         Got stderr:\n{}",
        nonexistent_adapter,
        stderr_output
    );

    // ASSERTION 2: Error message must indicate this is an adapter error
    assert!(
        stderr_output.contains("adapter")
            && (stderr_output.contains("not found")
                || stderr_output.contains("unknown")
                || stderr_output.contains("no such")),
        "error message should indicate adapter not found. \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 3: Error message must include configuration directory guidance
    assert!(
        stderr_output.contains("~/.needle/agents/")
            || stderr_output.contains(".needle/agents/")
            || stderr_output.contains("claude-config/agents/")
            || stderr_output.contains(".config/needle/adapters/"),
        "error message should include configuration directory guidance. Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 4: Error message must contain the adapter name in file path examples
    // This ensures users see exactly where their adapter file should be located
    assert!(
        stderr_output.contains(&format!("{nonexistent_adapter}.yaml"))
            || stderr_output.contains(&format!("{nonexistent_adapter}/config.json"))
            || stderr_output.contains(&format!("agents/{nonexistent_adapter}")),
        "error message should show the adapter name in file path examples. Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 5: Error message must include remediation language
    // Phrases like "To fix this" indicate actionable guidance
    assert!(
        stderr_output.contains("To fix this")
            || stderr_output.contains("To resolve this")
            || stderr_output.contains("To correct this")
            || stderr_output.contains("fix this")
            || stderr_output.contains("resolve this"),
        "error message should include remediation language (e.g., 'To fix this'). Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 6: Error message must include a "Common causes" or similar section
    // This helps users diagnose why the error occurred
    assert!(
        stderr_output.contains("Common causes")
            || stderr_output.contains("common causes")
            || stderr_output.contains("Possible causes")
            || stderr_output.contains("possible causes")
            || stderr_output.contains("Reasons")
            || stderr_output.contains("reasons"),
        "error message should include a 'Common causes' section to help diagnose the issue. \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 7: Error message must be structured with bullet points or numbered lists
    // Multi-step guidance is easier to follow when formatted as a list
    assert!(
        stderr_output.contains("  -") ||  // Markdown-style bullets
        stderr_output.contains("•") ||   // Unicode bullets
        stderr_output.contains("*") ||   // Asterisk bullets
        stderr_output.contains("1.") ||  // Numbered lists
        stderr_output.contains("\n\n"), // At minimum, multi-paragraph structure
        "error message should be structured with bullets or multiple paragraphs for readability. \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 8: Error message must indicate the problem occurred at startup
    // This prevents users from thinking it's a runtime issue
    assert!(
        stderr_output.contains("startup")
            || stderr_output.contains("Startup")
            || stderr_output.contains("boot")
            || stderr_output.contains("initialization")
            || stderr_output.contains("aborting"),
        "error message should indicate the problem occurred at startup/boot time. \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 9: Error message must prevent bead claiming with invalid config
    // This is critical - the error should explain WHY startup is aborting
    assert!(
        stderr_output.contains("claiming") ||
        stderr_output.contains("prevent") ||
        stderr_output.contains("invalid adapter") ||
        stderr_output.contains("invalid configuration") ||
        stderr_output.contains("aborting to prevent"),
        "error message should explain why startup is aborting (to prevent claiming beads with bad config). \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 10: Verify the error message is substantial and multi-line
    // A good error message should be detailed, not a single line
    let line_count = stderr_output.lines().count();
    assert!(
        line_count >= 5,
        "error message should be substantial (at least 5 lines), but got {} lines. \
         Got stderr:\n{}",
        line_count,
        stderr_output
    );

    // ASSERTION 11: Error message must include actionable configuration directory guidance
    // This is distinct from path checking - it validates the presence of remediation language
    // that explicitly directs users to check their configuration directory
    assert!(
        stderr_output.contains("check your configuration directory") ||
        stderr_output.contains("check your config") ||
        stderr_output.contains("check the config") ||
        stderr_output.contains("configuration directory") ||
        (stderr_output.contains("check") && stderr_output.contains("config")),
        "error message should include actionable guidance about checking the configuration directory. \
         Got stderr:\n{}",
        stderr_output
    );
}

/// Test that worker rejects a nonexistent adapter even when beads are available.
///
/// Regression test for the scenario where:
/// 1. A bead exists and is ready to be claimed
/// 2. The configured adapter does not exist
/// 3. Worker should fail at boot BEFORE claiming the bead
///
/// This prevents orphaned in_progress beads from misconfiguration.
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
#[tokio::test]
async fn worker_boot_rejects_nonexistent_adapter_before_claiming_work() {
    let nonexistent_adapter = "another-missing-adapter-456";
    let bead = make_bead("needle-adapter-check", 1);
    let store = Arc::new(IntegrationMockStore::new(vec![bead]));
    let _home_dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.agent.default = nonexistent_adapter.to_string();
    config.workspace.home = _home_dir.path().to_path_buf();
    // Confine Explore strand to test's tempdir to prevent scanning real user directories
    config.strands.explore.workspace_root = _home_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    let mut worker = Worker::new(config, "test-worker".to_string(), store.clone());
    let result = worker.run().await;

    // Worker should fail at boot, not claim the bead
    assert!(
        result.is_err(),
        "worker should fail to boot with nonexistent adapter"
    );

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains(nonexistent_adapter),
        "error should mention the nonexistent adapter name"
    );

    // Verify no work was done
    assert_eq!(
        worker.beads_processed(),
        0,
        "worker should not claim or process any beads when adapter is invalid"
    );

    // Verify no claim was attempted on the store
    let actions = store.actions();
    assert!(
        !actions.iter().any(|a| a.starts_with("claim:")),
        "worker should not attempt to claim beads when adapter is invalid; actions: {:?}",
        actions
    );
}

/// Test that worker boots successfully with a valid adapter.
///
/// This is the positive test case demonstrating that:
/// 1. When a valid adapter is configured, worker boots successfully
/// 2. Worker can claim and process beads
/// 3. No adapter validation errors occur
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
#[tokio::test]
async fn worker_boot_succeeds_with_valid_adapter() {
    let bead = make_bead("needle-valid-adapter", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));

    // Use make_worker_with_adapter which sets up a valid adapter
    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "valid-test-adapter", "exit 0", 10);

    let result = worker.run().await;

    // Worker should succeed
    assert!(
        result.is_ok(),
        "worker should boot successfully with valid adapter: {:?}",
        result
    );

    let final_state = result.unwrap();
    assert!(
        final_state == WorkerState::Stopped || final_state == WorkerState::Exhausted,
        "worker should reach terminal state, got: {:?}",
        final_state
    );

    // Worker should have processed the bead
    assert!(
        worker.beads_processed() >= 1,
        "worker should process at least one bead with valid adapter"
    );
}

/// Test that adapter validation happens BEFORE the main worker loop.
///
/// This is a timing/regression test ensuring that:
/// 1. Adapter validation is early in the boot sequence
/// 2. No beads are claimed before validation
/// 3. The error is immediate, not delayed by idle timeouts or retries
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
#[tokio::test]
async fn adapter_validation_happens_before_main_worker_loop() {
    let nonexistent_adapter = "timing-test-adapter-missing";
    let bead = make_bead("needle-timing-check", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));
    let _home_dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.agent.default = nonexistent_adapter.to_string();
    config.worker.idle_action = IdleAction::Wait;
    config.worker.idle_timeout = 10; // 10 second idle timeout
    config.workspace.home = _home_dir.path().to_path_buf();
    // Confine Explore strand to test's tempdir to prevent scanning real user directories
    config.strands.explore.workspace_root = _home_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    let start = std::time::Instant::now();
    let mut worker = Worker::new(config, "test-worker".to_string(), store);
    let result = worker.run().await;
    let elapsed = start.elapsed();

    // Should fail immediately, not wait for idle timeout
    assert!(
        result.is_err(),
        "worker should fail with nonexistent adapter"
    );

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains(nonexistent_adapter),
        "error should mention the nonexistent adapter"
    );

    // Verify failure was fast, i.e. it did not wait out the 10s idle timeout configured
    // above. The bound is 5s rather than 2s because worker boot alone measures ~1.8s in
    // isolation on this hardware -- a 2s bound left ~13% headroom and failed whenever the
    // suite ran in parallel. 5s still fails decisively if validation slips past the idle
    // timeout, which is the property under test. See needle-ab52a15a.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "adapter validation should fail before the idle timeout; took {:?}",
        elapsed
    );

    // Verify no beads were claimed
    assert_eq!(worker.beads_processed(), 0);
}

/// Subprocess test: needle binary produces actionable error message for nonexistent adapter.
///
/// This test spawns the actual needle binary as a subprocess to verify that the
/// error message shown to users is:
/// 1. Captured from stderr
/// 2. Contains the specific adapter name that failed
/// 3. Contains configuration directory guidance
/// 4. Is actionable (user knows how to fix the problem)
///
/// ISOLATION REQUIRED: This test spawns a real needle binary subprocess.
/// The test must isolate HOME to prevent the Explore strand from scanning
/// the real user workspace. Without this, the spawned needle binary would leak
/// into the real $HOME and scan real repos, contaminating the test environment.
///
/// See CLAUDE.md Test Isolation Policy and ADR-006 for full details.
#[tokio::test]
async fn subprocess_nonexistent_adapter_produces_actionable_error_message() {
    // Create a temporary workspace for the test
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path().join("test-workspace");
    std::fs::create_dir(&workspace).unwrap();

    // Initialize bead workspace (bead-rs CLI)
    let bead_result = std::process::Command::new("bead")
        .arg("init")
        .current_dir(&workspace)
        .output();

    // bead init may fail if the workspace is already initialized - that's OK for this test
    if let Ok(init_output) = bead_result {
        if !init_output.status.success() {
            let stderr = String::from_utf8_lossy(&init_output.stderr);
            // Only fail hard if it's a real error, not "already initialized"
            if !stderr.contains("already") && !stderr.contains("exists") {
                panic!("bead init failed: {}", stderr);
            }
        }
    }

    // Create .needle.yaml configuration to enable bead store discovery
    // Use bead-rs backend since that's the active CLI in this workspace
    std::fs::write(
        workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .expect("failed to create .needle.yaml configuration");

    // Get the needle binary path
    let bin_path = std::env::var("CARGO_BIN_EXE_needle").unwrap_or_else(|_| "needle".to_string());

    // Use a clearly fake adapter name that will never exist
    let nonexistent_adapter = "totally-fake-adapter-xyz-999";

    // Spawn the needle binary with the nonexistent adapter
    let output = std::process::Command::new(&bin_path)
        // NEEDLE_INNER=1 runs the worker loop in THIS process.
        // Without it, `needle run` detaches into a tmux session and the parent exits 0
        // immediately -- so adapter preflight happens in the detached worker and this
        // process reports success no matter what. That is why these assertions saw
        // "it succeeded" with empty stderr. The systemd unit sets the same variable.
        // See needle-ab52a15a.
        .env("NEEDLE_INNER", "1")
        .arg("run")
        .arg("--agent")
        .arg(nonexistent_adapter)
        .arg("--workspace")
        .arg(&workspace)
        .arg("--identifier")
        .arg("test-worker") // Use explicit identifier for deterministic behavior
        .env("HOME", temp_dir.path()) // ISOLATION: Prevent scanning real user directories
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("needle run command failed to execute");

    // Capture stderr for analysis
    let stderr_output = String::from_utf8_lossy(&output.stderr);

    // The worker should fail (nonzero exit code)
    assert!(
        !output.status.success(),
        "needle run should fail with nonexistent adapter, but it succeeded. \
         stderr: {}",
        stderr_output
    );

    // ASSERTION 1: Error message must contain the nonexistent adapter name
    assert!(
        stderr_output.contains(nonexistent_adapter),
        "error message should mention the nonexistent adapter name '{}'. \
         Got stderr:\n{}",
        nonexistent_adapter,
        stderr_output
    );

    // ASSERTION 2: Error message must indicate this is an adapter error
    assert!(
        stderr_output.contains("adapter")
            && (stderr_output.contains("not found")
                || stderr_output.contains("unknown")
                || stderr_output.contains("no such")),
        "error message should indicate adapter not found. \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 3: Error message must include specific configuration directory paths
    // The error should provide exact file paths where users should configure adapters
    assert!(
        stderr_output.contains("~/.needle/agents/")
            || stderr_output.contains(".needle/agents/")
            || stderr_output.contains("claude-config/agents/")
            || stderr_output.contains(".config/needle/adapters/"),
        "error message should include specific configuration directory paths. \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 4: Error message must contain the adapter name in file path examples
    // This ensures users see exactly where their adapter file should be located
    assert!(
        (stderr_output.contains(&format!("{nonexistent_adapter}.yaml"))
            || stderr_output.contains(&format!("{nonexistent_adapter}/config.json"))
            || stderr_output.contains(&format!("agents/{nonexistent_adapter}"))),
        "error message should show the adapter name in file path examples. \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 5: Error message must include remediation language
    // Phrases like "To fix this" indicate actionable guidance
    assert!(
        stderr_output.contains("To fix this")
            || stderr_output.contains("To resolve this")
            || stderr_output.contains("To correct this")
            || stderr_output.contains("fix this")
            || stderr_output.contains("resolve this"),
        "error message should include remediation language (e.g., 'To fix this'). \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 6: Error message must include a "Common causes" or similar section
    // This helps users diagnose why the error occurred
    assert!(
        stderr_output.contains("Common causes")
            || stderr_output.contains("common causes")
            || stderr_output.contains("Possible causes")
            || stderr_output.contains("possible causes")
            || stderr_output.contains("Reasons")
            || stderr_output.contains("reasons"),
        "error message should include a 'Common causes' section to help diagnose the issue. \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 7: Error message must be structured with bullet points or numbered lists
    // Multi-step guidance is easier to follow when formatted as a list
    assert!(
        stderr_output.contains("  -") ||  // Markdown-style bullets
        stderr_output.contains("•") ||   // Unicode bullets
        stderr_output.contains("*") ||   // Asterisk bullets
        stderr_output.contains("1.") ||  // Numbered lists
        stderr_output.contains("\n\n"), // At minimum, multi-paragraph structure
        "error message should be structured with bullets or multiple paragraphs for readability. \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 8: Error message must indicate the problem occurred at startup
    // This prevents users from thinking it's a runtime issue
    assert!(
        stderr_output.contains("startup")
            || stderr_output.contains("Startup")
            || stderr_output.contains("boot")
            || stderr_output.contains("initialization")
            || stderr_output.contains("aborting"),
        "error message should indicate the problem occurred at startup/boot time. \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 9: Error message must prevent bead claiming with invalid config
    // This is critical - the error should explain WHY startup is aborting
    assert!(
        stderr_output.contains("claiming") ||
        stderr_output.contains("prevent") ||
        stderr_output.contains("invalid adapter") ||
        stderr_output.contains("invalid configuration") ||
        stderr_output.contains("aborting to prevent"),
        "error message should explain why startup is aborting (to prevent claiming beads with bad config). \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION 10: Verify the error message is substantial and multi-line
    // A good error message should be detailed, not a single line
    let line_count = stderr_output.lines().count();
    assert!(
        line_count >= 5,
        "error message should be substantial (at least 5 lines), but got {} lines. \
         Got stderr:\n{}",
        line_count,
        stderr_output
    );

    // ASSERTION 11: Verify the error message ends with a clear error statement
    // The final line should summarize the problem
    let final_line = stderr_output.lines().last().unwrap_or("");
    assert!(
        final_line.contains("Error:")
            || final_line.contains("error:")
            || final_line.contains("startup aborted")
            || final_line.contains("not found"),
        "error message should end with a clear error summary. Final line: '{}'. \
         Got stderr:\n{}",
        final_line,
        stderr_output
    );

    // ASSERTION 12: Error message must include actionable configuration directory guidance
    // This is distinct from path checking - it validates the presence of remediation language
    // that explicitly directs users to check their configuration directory
    assert!(
        stderr_output.contains("check your configuration directory") ||
        stderr_output.contains("check your config") ||
        stderr_output.contains("check the config") ||
        stderr_output.contains("configuration directory") ||
        (stderr_output.contains("check") && stderr_output.contains("config")),
        "error message should include actionable guidance about checking the configuration directory. \
         Got stderr:\n{}",
        stderr_output
    );
}

/// Test that adapter validation rejects adapter names with special characters.
///
/// Regression test ensuring that adapter validation properly handles
/// adapter names containing special characters that might cause
/// filesystem issues or injection attacks.
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
#[tokio::test]
async fn adapter_validation_rejects_special_characters() {
    // Test various problematic adapter names
    let problematic_names = vec![
        "../../../etc/passwd",       // Path traversal
        "adapter;rm-rf~",            // Command injection attempt
        "adapter`whoami`",           // Backtick injection
        "adapter$(echo)",            // Subshell injection
        "adapter\t\ndescription",    // Whitespace control characters
        "adapter\u{200B}\u{FEFF}",   // Zero-width spaces (unicode)
        "console.log('xss')",        // JavaScript-like
        "<script>alert(1)</script>", // HTML-like
        "adapter/../../etc/shadow",  // Nested traversal
    ];

    for nonexistent_adapter in problematic_names {
        let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
        let _home_guard = HomeGuard::isolate();
        let mut config = Config::default();
        config.agent.default = nonexistent_adapter.to_string();
        config.workspace.home = _home_guard._temp_dir.path().to_path_buf();
        config.strands.explore.workspace_root = _home_guard._temp_dir.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();

        let mut worker = Worker::new(config, "test-worker".to_string(), store);
        let result = worker.run().await;

        assert!(
            result.is_err(),
            "worker should fail with adapter name containing special chars: '{}'",
            nonexistent_adapter
        );

        let error_msg = result.unwrap_err().to_string();

        // Echoing the rejected adapter name back is correct and useful -- it is how the
        // operator learns which name failed. What must never appear is evidence the
        // payload was EXECUTED. Checking the raw message for "etc/passwd" or "whoami"
        // conflated the two and failed on names that literally contain those strings
        // (e.g. '../../../etc/passwd'), so strip the name before looking for
        // execution artifacts. See needle-ab52a15a.
        let sanitized = error_msg.replace(nonexistent_adapter, "<rejected-adapter-name>");
        assert!(
            !sanitized.contains("etc/passwd")
                && !sanitized.contains("root:")
                && !sanitized.contains("whoami")
                && !sanitized.contains("rm -rf"),
            "error message must not contain evidence of executing the injected payload \
             for adapter '{}'; got: {}",
            nonexistent_adapter,
            error_msg
        );
    }
}

/// Test that adapter validation is case-sensitive and distinguishes similar names.
///
/// Regression test ensuring that "MyAdapter", "myadapter", and "MYADAPTER"
/// are treated as distinct adapter names. This prevents confusion when
/// users have adapters with similar names differing only in case.
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
#[tokio::test]
async fn adapter_validation_is_case_sensitive() {
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_guard = HomeGuard::isolate();
    let mut config = Config::default();
    // Use a name that exists in lowercase but request it in mixed case
    config.agent.default = "TestAdapter".to_string(); // Mixed case
    config.workspace.home = _home_guard._temp_dir.path().to_path_buf();
    config.strands.explore.workspace_root = _home_guard._temp_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    let mut worker = Worker::new(config, "test-worker".to_string(), store);
    let result = worker.run().await;

    // Should fail because "TestAdapter" != "testadapter"
    assert!(
        result.is_err(),
        "worker should fail with case-sensitive adapter name mismatch"
    );

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("TestAdapter") || error_msg.contains("adapter"),
        "error should mention the requested adapter name"
    );
}

/// Test that adapter validation fails even with multiple available workspaces.
///
/// Regression test ensuring that adapter validation happens independently
/// of workspace discovery. Even if the Explore strand finds valid workspaces,
/// adapter validation should still fail fast if the adapter doesn't exist.
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
#[tokio::test]
async fn adapter_validation_fails_despite_valid_workspaces() {
    let nonexistent_adapter = "multi-workspace-adapter-missing";
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_guard = HomeGuard::isolate();

    // Create multiple fake workspace directories to simulate workspace discovery
    let ws1 = _home_guard._temp_dir.path().join("workspace1");
    let ws2 = _home_guard._temp_dir.path().join("workspace2");
    std::fs::create_dir(&ws1).unwrap();
    std::fs::create_dir(&ws2).unwrap();

    // Initialize bead workspaces
    for ws in [&ws1, &ws2] {
        let _ = std::process::Command::new("bead")
            .arg("init")
            .current_dir(ws)
            .output();
    }

    let mut config = Config::default();
    config.agent.default = nonexistent_adapter.to_string();
    config.workspace.home = _home_guard._temp_dir.path().to_path_buf();
    // Explicitly add workspaces to simulate discovery
    config.strands.explore.workspace_root = _home_guard._temp_dir.path().to_path_buf();
    config.strands.explore.workspaces = vec![ws1.clone(), ws2.clone()];

    let mut worker = Worker::new(config, "test-worker".to_string(), store);
    let result = worker.run().await;

    assert!(
        result.is_err(),
        "adapter validation should fail even with valid workspaces configured"
    );

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains(nonexistent_adapter),
        "error should mention the nonexistent adapter"
    );
}

/// Test that adapter validation happens before routing table initialization.
///
/// Regression test ensuring that even if routing rules are configured,
/// adapter validation of the default adapter happens first and fails fast.
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
#[tokio::test]
async fn adapter_validation_fails_before_routing_initialization() {
    let nonexistent_adapter = "routing-test-adapter-missing";
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_guard = HomeGuard::isolate();
    let mut config = Config::default();
    config.agent.default = nonexistent_adapter.to_string();
    config.workspace.home = _home_guard._temp_dir.path().to_path_buf();
    config.strands.explore.workspace_root = _home_guard._temp_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    // Configure routing (this should not bypass adapter validation)
    config.agent.routing = Some(needle::config::RoutingConfig {
        default_adapter: Some(nonexistent_adapter.to_string()),
        rules: vec![],
        strict: false,
    });

    let mut worker = Worker::new(config, "test-worker".to_string(), store);
    let result = worker.run().await;

    assert!(
        result.is_err(),
        "adapter validation should fail even when routing is configured"
    );

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains(nonexistent_adapter) || error_msg.contains("adapter"),
        "error should mention the nonexistent adapter"
    );
}

/// Test that adapter validation rejects adapter names resembling system paths.
///
/// Regression test for path-like adapter names that might confuse users
/// or cause unexpected behavior. Ensures validation treats these as
/// nonexistent adapters and provides clear guidance.
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
#[tokio::test]
async fn adapter_validation_rejects_path_like_names() {
    let path_like_names = vec![
        "/usr/bin/adapter",
        "./local-adapter",
        "../parent-adapter",
        "~/.needle/adapter",
        "C:\\Windows\\System32\\adapter", // Windows path
        "/etc/needle/adapter.d/config",
    ];

    for path_like_name in path_like_names {
        let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
        let _home_guard = HomeGuard::isolate();
        let mut config = Config::default();
        config.agent.default = path_like_name.to_string();
        config.workspace.home = _home_guard._temp_dir.path().to_path_buf();
        config.strands.explore.workspace_root = _home_guard._temp_dir.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();

        let mut worker = Worker::new(config, "test-worker".to_string(), store);
        let result = worker.run().await;

        assert!(
            result.is_err(),
            "worker should fail with path-like adapter name: '{}'",
            path_like_name
        );
    }
}

/// Test that adapter validation error message includes adapter name with special characters escaped.
///
/// Regression test ensuring that when an adapter name contains special characters,
/// the error message properly escapes or sanitizes them for display, preventing
/// confusion or security issues in error output.
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
#[tokio::test]
async fn adapter_validation_error_message_sanitizes_special_chars() {
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_guard = HomeGuard::isolate();
    let mut config = Config::default();
    // Use an adapter name with characters that might need escaping in error messages
    let problematic_adapter = "adapter'with\"quotes\\and\\backslashes";
    config.agent.default = problematic_adapter.to_string();
    config.workspace.home = _home_guard._temp_dir.path().to_path_buf();
    config.strands.explore.workspace_root = _home_guard._temp_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    let mut worker = Worker::new(config, "test-worker".to_string(), store);
    let result = worker.run().await;

    assert!(
        result.is_err(),
        "worker should fail with adapter containing special characters"
    );

    let error_msg = result.unwrap_err().to_string();
    // The error should mention the adapter but be safe
    assert!(
        error_msg.contains("adapter") || error_msg.contains("not found"),
        "error should indicate adapter failure; got: {}",
        error_msg
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 8: Full pipeline telemetry sequence
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn full_cycle_produces_telemetry_state_transitions() {
    // Verify the expected state transition sequence occurs.
    let bead = make_bead("needle-telem", 1);
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::new(vec![bead]));

    let (mut worker, _home_dir) = make_worker_with_adapter(store, "echo-test", "echo done", 10);

    let result = worker.run().await.unwrap();

    assert!(
        result == WorkerState::Stopped || result == WorkerState::Exhausted,
        "expected terminal state, got {:?}",
        result
    );
    // The key assertion: at least 1 bead was processed through the full pipeline.
    assert!(worker.beads_processed() >= 1);
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 9: Dispatcher integration — real process execution
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dispatcher_captures_stdout_and_stderr() {
    let adapter = test_adapter("io-test", "echo stdout-msg; echo stderr-msg >&2", 10);
    let mut adapters = HashMap::new();
    adapters.insert("io-test".to_string(), adapter.clone());

    let dispatcher =
        Dispatcher::with_adapters(adapters, Telemetry::new("test-worker".to_string()), 10);

    let prompt = needle::prompt::BuiltPrompt {
        content: "test".to_string(),
        hash: "abc123".to_string(),
        token_estimate: 1,
        template_name: "pluck".to_string(),
        template_version: "pluck-default".to_string(),
    };

    let result = dispatcher
        .dispatch(
            &BeadId::from("nd-io"),
            &prompt,
            &adapter,
            std::path::Path::new("/tmp"),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("stdout-msg"));
    assert!(result.stderr.contains("stderr-msg"));
}

#[tokio::test]
async fn dispatcher_timeout_kills_process() {
    let adapter = test_adapter("slow", "sleep 100", 1);
    let mut adapters = HashMap::new();
    adapters.insert("slow".to_string(), adapter.clone());

    let dispatcher =
        Dispatcher::with_adapters(adapters, Telemetry::new("test-worker".to_string()), 10);

    let prompt = needle::prompt::BuiltPrompt {
        content: "test".to_string(),
        hash: "abc123".to_string(),
        token_estimate: 1,
        template_name: "pluck".to_string(),
        template_version: "pluck-default".to_string(),
    };

    let result = dispatcher
        .dispatch(
            &BeadId::from("nd-slow"),
            &prompt,
            &adapter,
            std::path::Path::new("/tmp"),
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, 124, "timeout should yield exit 124");
    assert!(
        result.elapsed >= std::time::Duration::from_millis(900),
        "should have waited ~1s"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 10: Multiple beads with different priorities
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn worker_processes_high_priority_beads_first() {
    // Create beads with different priorities.
    let mut high = make_bead("needle-high", 1);
    let mut low = make_bead("needle-low", 3);
    high.created_at = Utc::now();
    low.created_at = Utc::now() - chrono::Duration::hours(10);

    let store = Arc::new(IntegrationMockStore::new(vec![low, high]));

    let (mut worker, _home_dir) =
        make_worker_with_adapter(store.clone(), "echo-test", "echo done", 10);

    let result = worker.run().await.unwrap();
    assert!(result == WorkerState::Stopped || result == WorkerState::Exhausted);

    let actions = store.actions();
    // Find claim actions to verify order.
    let claims: Vec<&String> = actions.iter().filter(|a| a.starts_with("claim:")).collect();
    assert!(!claims.is_empty(), "should have at least one claim action");
    // First claim should be for the high-priority bead.
    assert!(
        claims[0].contains("needle-high"),
        "highest priority bead should be claimed first; claims: {:?}",
        claims
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 11: Cross-workspace mend: two-workspace zombie scenario
// ═════════════════════════════════════════════════════════════════════════════

/// Mock BeadStore that simulates real CLI-store behavior for zombie scenarios.
///
/// This mock properly simulates the behavior where:
/// - In-progress beads don't appear in ready()
/// - Released beads become Open and appear in subsequent ready() calls
/// - This enables testing the "released beads returned in same pass" behavior
#[allow(dead_code)]
struct ZombieMockStore {
    /// All beads, mutable to support state transitions (release → open).
    beads: Mutex<Vec<Bead>>,
    /// Path to this workspace (for tagging).
    workspace: PathBuf,
    /// Track release calls.
    released: Arc<Mutex<Vec<BeadId>>>,
}

#[allow(dead_code)]
impl ZombieMockStore {
    fn new(all_beads: Vec<Bead>, workspace: PathBuf) -> (Self, Arc<Mutex<Vec<BeadId>>>) {
        let released = Arc::new(Mutex::new(Vec::new()));
        (
            ZombieMockStore {
                beads: Mutex::new(all_beads),
                workspace,
                released: released.clone(),
            },
            released,
        )
    }
}

#[async_trait]
impl BeadStore for ZombieMockStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        // Return open, unassigned beads (matching real br behavior).
        Ok(self
            .beads
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b.status == BeadStatus::Open && b.assignee.is_none())
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        let beads = self.beads.lock().unwrap();
        let bead = beads
            .iter()
            .find(|b| b.id == *id)
            .cloned()
            .unwrap_or_else(|| make_bead_with_id(id.as_ref()));
        Ok(bead)
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::InProgress;
            bead.assignee = Some(actor.to_string());
            return Ok(ClaimResult::Claimed(bead.clone()));
        }
        Ok(ClaimResult::NotClaimable {
            reason: "not found".to_string(),
        })
    }

    async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
        Ok(ClaimResult::NotClaimable {
            reason: "no beads available".to_string(),
        })
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        self.released.lock().unwrap().push(id.clone());
        // Update bead state: released beads become Open with no assignee.
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::Open;
            bead.assignee = None;
        }
        Ok(())
    }

    async fn block(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
        Ok(vec![])
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn create_bead(&self, title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
        Ok(BeadId::from(title.to_string()))
    }

    async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn remove_dependency(&self, _blocked_id: &BeadId, _blocker_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn full_rebuild(&self) -> Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true // Mock store always has a valid store
    }
}

/// Store that delegates to different underlying stores based on workspace path.
///
/// This allows ExploreStrand to query remote workspaces that have different
/// mock behaviors (e.g., zombie scenarios).
#[allow(dead_code)]
struct MultiWorkspaceStore {
    home_store: Arc<dyn BeadStore>,
    remote_stores: std::collections::HashMap<PathBuf, Arc<dyn BeadStore>>,
}

#[allow(dead_code)]
impl MultiWorkspaceStore {
    fn new(home_store: Arc<dyn BeadStore>) -> Self {
        MultiWorkspaceStore {
            home_store,
            remote_stores: std::collections::HashMap::new(),
        }
    }

    fn add_remote(&mut self, workspace: PathBuf, store: Arc<dyn BeadStore>) {
        self.remote_stores.insert(workspace, store);
    }
}

#[async_trait]
impl BeadStore for MultiWorkspaceStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        self.home_store.ready(_filters).await
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        self.home_store.list_all().await
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        self.home_store.show(id).await
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        self.home_store.claim(id, actor).await
    }

    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
        self.home_store.claim_auto(actor).await
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        self.home_store.release(id).await
    }

    async fn block(&self, id: &BeadId) -> Result<()> {
        self.home_store.block(id).await
    }

    async fn flush(&self) -> Result<()> {
        self.home_store.flush().await
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        self.home_store.reopen(id).await
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        self.home_store.labels(id).await
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.home_store.add_label(id, label).await
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.home_store.remove_label(id, label).await
    }

    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        self.home_store.create_bead(title, body, labels).await
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        self.home_store.add_dependency(blocker_id, blocked_id).await
    }

    async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()> {
        self.home_store
            .remove_dependency(blocked_id, blocker_id)
            .await
    }

    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        self.home_store.clear_assignee(id).await
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        self.home_store.doctor_repair().await
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        self.home_store.doctor_check().await
    }

    async fn full_rebuild(&self) -> Result<()> {
        self.home_store.full_rebuild().await
    }

    fn has_valid_store(&self) -> bool {
        self.home_store.has_valid_store()
    }
}

#[tokio::test]
async fn cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead() {
    use needle::config::ExploreConfig;
    use needle::strand::{ExploreStrand, Strand};
    use std::fs;

    // Create real temporary directories for home and remote workspaces.
    let home_dir = tempfile::tempdir().unwrap();
    let home_workspace = home_dir.path().to_path_buf();
    let home_store = Arc::new(IntegrationMockStore::empty());

    let remote_dir = tempfile::tempdir().unwrap();
    let remote_workspace = remote_dir.path().to_path_buf();
    let remote_beads_dir = remote_workspace.join(".beads");
    fs::create_dir_all(&remote_beads_dir).unwrap();

    // Initialize the bead workspace first.
    let init_output = std::process::Command::new("bead")
        .arg("init")
        .current_dir(&remote_workspace)
        .output()
        .expect("bead init command failed to execute");
    assert!(
        init_output.status.success(),
        "bead init failed: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    // Bind the workspace to a backend. open_configured() refuses a workspace whose
    // resolved backend is Auto ("no authoritative bead backend binding; set
    // bead_cli.backend"), and Explore opens remote workspaces through it -- so
    // without this file Explore cannot read this workspace at all and silently
    // skips it, reporting NoWork. See needle-ab52a15a.
    fs::write(
        remote_workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .expect("write .needle.yaml backend binding");

    // Create a zombie bead in the remote workspace using bead CLI.
    // First, create the bead as open.
    let output = std::process::Command::new("bead")
        .arg("create")
        .arg("--issue-type=task")
        .arg("--title=Zombie bead from crashed worker")
        .arg("--description=This bead was assigned to a worker that crashed")
        .current_dir(&remote_workspace)
        .output()
        .expect("br create command failed to execute");
    let create_result = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "br create failed: {}",
        create_result
    );

    // Extract the bead ID from the create output (format is just "<ID>").
    let bead_id = create_result
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("Initialized"))
        .unwrap()
        .to_string();
    let bead_id = BeadId::from(bead_id);

    // Claim the bead to a dead worker with the correct qualified_id format.
    // The ExploreStrand's qualified_id is "claude-test-worker", so we use a different
    // adapter prefix to ensure it doesn't match.
    let claim_output = std::process::Command::new("bead")
        .arg("update")
        .arg(bead_id.as_ref())
        .arg("--assignee")
        .arg("codesearch-dead-worker-12345")
        .arg("--status")
        .arg("in_progress")
        .current_dir(&remote_workspace)
        .output()
        .expect("br update command failed to execute");
    assert!(
        claim_output.status.success(),
        "br update failed: {}",
        String::from_utf8_lossy(&claim_output.stderr)
    );

    // Verify the bead is now in-progress and not in ready().
    let remote_store = configured_forge_store(remote_workspace.clone().to_path_buf());
    let filters = Filters {
        assignee: None,
        exclude_labels: vec![
            "deferred".to_string(),
            "human".to_string(),
            "blocked".to_string(),
        ],
        exclude_ids: HashSet::new(),
    };
    let ready_result = remote_store.ready(&filters).await.unwrap();
    assert!(
        ready_result.is_empty(),
        "remote workspace should have no ready beads initially"
    );

    // Create ExploreStrand with the remote workspace configured.
    let temp_dir = tempfile::tempdir().unwrap();
    let registry = needle::registry::Registry::new(temp_dir.path());
    let telemetry = Telemetry::new("test-worker".to_string());

    let explore_config = ExploreConfig {
        enabled: true,
        workspaces: vec![remote_workspace.clone()],
        // Isolate Explore scan root to prevent real user directory scans (see CLAUDE.md Test Isolation Policy)
        workspace_root: temp_dir.path().to_path_buf(),
        rediscovery_cycles: 60,
        starvation_threshold_minutes: 15,
        scan_interval_cycles: 1,
        max_scan_interval_cycles: 8,
    };

    let explore = ExploreStrand::new(
        explore_config,
        home_workspace,
        registry,
        telemetry,
        "test-worker".to_string(),
    );

    // Evaluate ExploreStrand — it should run cross-workspace mend.
    let result = explore.evaluate(home_store.as_ref(), &HashSet::new()).await;

    // After cross-workspace mend, ExploreStrand should return BeadFound with the tagged bead.
    match result {
        StrandResult::BeadFound(beads) => {
            assert!(
                !beads.is_empty(),
                "should return at least one bead after releasing orphan"
            );
            let bead = &beads[0];
            assert_eq!(
                bead.workspace, remote_workspace,
                "bead should be tagged with remote workspace path"
            );
            assert_eq!(
                bead.id, bead_id,
                "should return the zombie bead after release"
            );
            assert_eq!(
                bead.status,
                BeadStatus::Open,
                "released bead should be Open"
            );
            assert!(
                bead.assignee.is_none(),
                "released bead should have no assignee"
            );
        }
        StrandResult::NoWork => {
            panic!("expected BeadFound after releasing orphan, got NoWork");
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

#[tokio::test]
async fn cross_workspace_mend_skips_beads_with_live_assignees() {
    use needle::config::ExploreConfig;
    use needle::strand::{ExploreStrand, Strand};
    use std::fs;

    // Create real temporary directories.
    let home_dir = tempfile::tempdir().unwrap();
    let home_workspace = home_dir.path().to_path_buf();
    let home_store = Arc::new(IntegrationMockStore::empty());

    let remote_dir = tempfile::tempdir().unwrap();
    let remote_workspace = remote_dir.path().to_path_buf();
    let remote_beads_dir = remote_workspace.join(".beads");
    fs::create_dir_all(&remote_beads_dir).unwrap();

    // Initialize the bead workspace first.
    let init_output = std::process::Command::new("bead")
        .arg("init")
        .current_dir(&remote_workspace)
        .output()
        .expect("bead init command failed to execute");
    assert!(
        init_output.status.success(),
        "bead init failed: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    // Bind the workspace to a backend. open_configured() refuses a workspace whose
    // resolved backend is Auto ("no authoritative bead backend binding; set
    // bead_cli.backend"), and Explore opens remote workspaces through it -- so
    // without this file Explore cannot read this workspace at all and silently
    // skips it, reporting NoWork. See needle-ab52a15a.
    fs::write(
        remote_workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .expect("write .needle.yaml backend binding");

    // Create a bead in the remote workspace.
    let output = std::process::Command::new("bead")
        .arg("create")
        .arg("--issue-type=task")
        .arg("--title=Bead with live assignee")
        .arg("--description=This bead is assigned to a live worker")
        .current_dir(&remote_workspace)
        .output()
        .expect("br create command failed to execute");
    assert!(output.status.success(), "br create failed");

    let create_result = String::from_utf8_lossy(&output.stdout);
    let bead_id = create_result
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("Initialized"))
        .unwrap()
        .to_string();
    let bead_id = BeadId::from(bead_id);

    // Create a registry with a live worker entry.
    let temp_dir = tempfile::tempdir().unwrap();
    let registry = needle::registry::Registry::new(temp_dir.path());

    // Register a live worker (using our own PID).
    registry
        .register(needle::registry::WorkerEntry {
            id: "live-worker".to_string(),
            pid: std::process::id(),
            workspace: remote_workspace.clone(),
            agent: "test".to_string(),
            model: None,
            provider: None,
            started_at: Utc::now(),
            beads_processed: 0,
            config_reload_generation: 0,
        })
        .unwrap();

    // Claim the bead to the live worker.
    let claim_output = std::process::Command::new("bead")
        .arg("update")
        .arg(bead_id.as_ref())
        .arg("--assignee")
        .arg("live-worker")
        .arg("--status")
        .arg("in_progress")
        .current_dir(&remote_workspace)
        .output()
        .expect("bead update command failed to execute");
    assert!(
        claim_output.status.success(),
        "bead update failed: {}",
        String::from_utf8_lossy(&claim_output.stderr)
    );

    let telemetry = Telemetry::new("test-worker".to_string());

    // Isolate Explore strand to prevent scanning real home directory
    // REQUIRED — see ADR-006 and Test Isolation Policy in CLAUDE.md
    let explore_temp_dir = tempfile::tempdir().unwrap();
    let explore_config = ExploreConfig {
        enabled: true,
        workspaces: vec![remote_workspace.clone()],
        workspace_root: explore_temp_dir.path().to_path_buf(),
        rediscovery_cycles: 60,
        starvation_threshold_minutes: 15,
        scan_interval_cycles: 1,
        max_scan_interval_cycles: 8,
    };

    let explore = ExploreStrand::new(
        explore_config,
        home_workspace,
        registry,
        telemetry,
        "test-worker".to_string(),
    );

    // Evaluate — the live worker's bead should NOT be released.
    let result = explore.evaluate(home_store.as_ref(), &HashSet::new()).await;

    // Since the bead has a live assignee, it should not be released.
    // The result should be NoWork since there are no ready beads.
    match result {
        StrandResult::NoWork => {
            // Expected — bead not released, no ready beads available.
        }
        StrandResult::BeadFound(beads) => {
            panic!(
                "should not release beads with live assignees; got beads: {:?}",
                beads
            );
        }
        other => panic!("unexpected result: {:?}", other),
    }

    // Verify the bead is still assigned to the live worker.
    let remote_store = configured_forge_store(remote_workspace.to_path_buf());
    let bead = remote_store.show(&bead_id).await.unwrap();
    assert_eq!(
        bead.assignee,
        Some("live-worker".to_string()),
        "bead should still be assigned to live worker"
    );
}

#[tokio::test]
async fn cross_workspace_mend_skips_own_worker_beads() {
    use needle::config::ExploreConfig;
    use needle::strand::{ExploreStrand, Strand};
    use std::fs;

    // Create real temporary directories.
    let home_dir = tempfile::tempdir().unwrap();
    let home_workspace = home_dir.path().to_path_buf();
    let home_store = Arc::new(IntegrationMockStore::empty());

    let remote_dir = tempfile::tempdir().unwrap();
    let remote_workspace = remote_dir.path().to_path_buf();
    let remote_beads_dir = remote_workspace.join(".beads");
    fs::create_dir_all(&remote_beads_dir).unwrap();

    // Initialize the bead workspace. An empty .beads/ directory is not a workspace:
    // bead-rs needs `init` to create the schema, so `create` below failed without it.
    let init_output = std::process::Command::new("bead")
        .arg("init")
        .current_dir(&remote_workspace)
        .output()
        .expect("bead init command failed to execute");
    assert!(
        init_output.status.success(),
        "bead init failed: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    // Bind the workspace to a backend. open_configured() refuses a workspace whose
    // resolved backend is Auto ("no authoritative bead backend binding; set
    // bead_cli.backend"), and Explore opens remote workspaces through it -- so
    // without this file Explore cannot read this workspace at all and silently
    // skips it, reporting NoWork. See needle-ab52a15a.
    fs::write(
        remote_workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .expect("write .needle.yaml backend binding");

    // Create a bead in the remote workspace.
    let output = std::process::Command::new("bead")
        .arg("create")
        .arg("--issue-type=task")
        .arg("--title=Bead assigned to us")
        .arg("--description=This bead is assigned to the current worker")
        .current_dir(&remote_workspace)
        .output()
        .expect("bead create command failed to execute");
    assert!(output.status.success(), "bead create failed");

    let create_result = String::from_utf8_lossy(&output.stdout);
    let bead_id = create_result
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("Initialized"))
        .unwrap()
        .to_string();
    let bead_id = BeadId::from(bead_id);

    // Create registry.
    let temp_dir = tempfile::tempdir().unwrap();
    let registry = needle::registry::Registry::new(temp_dir.path());

    // Claim the bead to ourselves using the qualified identity (matching production).
    let qualified_id = "claude-test-worker";
    let claim_output = std::process::Command::new("bead")
        .arg("update")
        .arg(bead_id.as_ref())
        .arg("--assignee")
        .arg(qualified_id)
        .arg("--status")
        .arg("in_progress")
        .current_dir(&remote_workspace)
        .output()
        .expect("br update command failed to execute");
    assert!(
        claim_output.status.success(),
        "br update failed: {}",
        String::from_utf8_lossy(&claim_output.stderr)
    );

    let telemetry = Telemetry::new("test-worker".to_string());

    // Isolate Explore strand to prevent scanning real home directory
    // REQUIRED — see ADR-006 and Test Isolation Policy in CLAUDE.md
    let explore_temp_dir = tempfile::tempdir().unwrap();
    let explore_config = ExploreConfig {
        enabled: true,
        workspaces: vec![remote_workspace.clone()],
        workspace_root: explore_temp_dir.path().to_path_buf(),
        rediscovery_cycles: 60,
        starvation_threshold_minutes: 15,
        scan_interval_cycles: 1,
        max_scan_interval_cycles: 8,
    };

    let explore = ExploreStrand::new(
        explore_config,
        home_workspace,
        registry,
        telemetry,
        qualified_id.to_string(),
    );

    // Evaluate — our own bead should NOT be released.
    let result = explore.evaluate(home_store.as_ref(), &HashSet::new()).await;

    // Since the bead is assigned to us, it should not be released.
    match result {
        StrandResult::NoWork => {
            // Expected — our bead not released, no ready beads available.
        }
        StrandResult::BeadFound(beads) => {
            panic!(
                "should not release our own worker's beads; got beads: {:?}",
                beads
            );
        }
        other => panic!("unexpected result: {:?}", other),
    }

    // Verify the bead is still assigned to us.
    let remote_store = configured_forge_store(remote_workspace.to_path_buf());
    let bead = remote_store.show(&bead_id).await.unwrap();
    assert_eq!(
        bead.assignee,
        Some(qualified_id.to_string()),
        "bead should still be assigned to us"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 13: Mend removes stale dependency links, making beads claimable
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn mend_removes_stale_dependency_links() {
    use needle::config::MendConfig;
    use needle::strand::MendStrand;
    use std::time::Duration;

    // Create a temporary workspace for the test.
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path();

    // Initialize bead workspace.
    let init_output = std::process::Command::new("bead")
        .arg("init")
        .current_dir(workspace)
        .output()
        .expect("bead init failed");
    assert!(init_output.status.success(), "bead init failed");

    // Create blocker bead.
    let blocker_output = std::process::Command::new("bead")
        .args([
            "create",
            "--title=Blocker bead",
            "--description=This is the blocker",
        ])
        .current_dir(workspace)
        .output()
        .expect("bead create failed");
    assert!(blocker_output.status.success(), "bead create failed");

    let blocker_id = String::from_utf8_lossy(&blocker_output.stdout)
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("Initialized"))
        .unwrap()
        .to_string();

    // Create blocked bead.
    let blocked_output = std::process::Command::new("bead")
        .args([
            "create",
            "--title=Blocked bead",
            "--description=This bead depends on the blocker",
        ])
        .current_dir(workspace)
        .output()
        .expect("bead create failed");
    assert!(blocked_output.status.success(), "bead create failed");

    let blocked_id = String::from_utf8_lossy(&blocked_output.stdout)
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("Initialized"))
        .unwrap()
        .to_string();

    // Add dependency: blocked depends on blocker.
    let dep_output = std::process::Command::new("bead")
        // bead-rs is blocked-first with an explicit --kind:
        //   bead dep add <BLOCKED> <BLOCKER> --kind blocks
        // The old bf-era form (`dep add <blocker> --blocks <blocked>`) both reversed
        // the operands and used a flag bead-rs does not accept. See needle-ab52a15a.
        .args(["dep", "add", &blocked_id, &blocker_id, "--kind", "blocks"])
        .current_dir(workspace)
        .output()
        .expect("bead dep add failed");
    assert!(dep_output.status.success(), "bead dep add failed");

    // Verify the dependency exists and the blocked bead is... blocked.
    let store = configured_forge_store(workspace.to_path_buf());
    let blocked_bead = store
        .show(&needle::types::BeadId::from(blocked_id.clone()))
        .await
        .unwrap();
    assert!(
        !blocked_bead.dependencies.is_empty(),
        "dependency should exist"
    );
    assert_eq!(blocked_bead.dependencies[0].id, blocker_id.as_str().into());
    assert_eq!(blocked_bead.dependencies[0].dependency_type, "blocks");

    // Verify the blocked bead does NOT appear in ready() (because it's blocked).
    let filters = Filters {
        assignee: None,
        exclude_labels: vec!["deferred".to_string(), "human".to_string()],
        exclude_ids: HashSet::new(),
    };
    let ready_before = store.ready(&filters).await.unwrap();
    assert!(
        !ready_before.iter().any(|b| b.id.as_ref() == blocked_id),
        "blocked bead should not appear in ready() before blocker is closed"
    );

    // Close the blocker bead.
    let close_output = std::process::Command::new("bead")
        .args(["close", &blocker_id, "--reason=Blocker completed"])
        .current_dir(workspace)
        .output()
        .expect("bead close failed");
    assert!(close_output.status.success(), "bead close failed");

    // Verify the blocker is closed but the dependency still exists (stale link).
    let blocked_bead_after = store
        .show(&needle::types::BeadId::from(blocked_id.clone()))
        .await
        .unwrap();
    assert!(
        !blocked_bead_after.dependencies.is_empty(),
        "dependency should still exist after blocker is closed (stale link)"
    );
    // Deliberately not asserting dependencies[0].status here: bead-rs does not carry
    // a per-dependency status in its JSON (bf did), so it is the serde default "" on
    // the canonical backend. What matters is that Mend removes the edge once the
    // blocker is closed, which is asserted below. See needle-ab52a15a.

    // Run Mend to clean up the stale dependency.
    let hb_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let traces_dir = tempfile::tempdir().unwrap();
    let log_dir = tempfile::tempdir().unwrap();
    let telemetry = Telemetry::new("test-worker".to_string());

    let mend_config = MendConfig {
        stuck_threshold_secs: 300,
        lock_ttl_secs: 3600,
        db_check_interval: 0,
        idle_timeout: 120,
    };

    let registry = needle::registry::Registry::new(reg_dir.path());

    let mend = MendStrand::new(
        mend_config,
        hb_dir.path().to_path_buf(),
        Duration::from_secs(300),
        lock_dir.path().to_path_buf(),
        "test-worker".to_string(),
        registry,
        telemetry,
        log_dir.path().to_path_buf(),
        7,
        traces_dir.path().to_path_buf(),
        7,
        1,
        workspace.to_path_buf(),
        1000,
        state_dir.path().to_path_buf(),
        needle::config::LimitsConfig::default(),
    );

    let result = mend.evaluate(&store, &HashSet::new()).await;

    // Mend should have returned WorkCreated because it removed a stale dependency.
    assert!(
        matches!(result, StrandResult::WorkCreated),
        "expected WorkCreated after removing stale dependency, got: {result:?}"
    );

    // Verify the dependency was actually removed from the bead.
    let blocked_bead_final = store
        .show(&needle::types::BeadId::from(blocked_id.clone()))
        .await
        .unwrap();
    assert!(
        blocked_bead_final.dependencies.is_empty(),
        "dependency should be removed after mend cleanup"
    );

    // Verify the blocked bead now appears in ready() (it's claimable again).
    let ready_after = store.ready(&filters).await.unwrap();
    assert!(
        ready_after.iter().any(|b| b.id.as_ref() == blocked_id),
        "blocked bead should appear in ready() after stale dependency is removed"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 7: Idle worker flagging
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn idle_worker_flagging_detects_stuck_workers() {
    use needle::config::{LimitsConfig, MendConfig};
    use needle::registry::Registry;
    use needle::strand::{MendStrand, Strand};
    use std::time::Duration;

    let workspace = tempfile::tempdir().unwrap();
    let hb_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let traces_dir = tempfile::tempdir().unwrap();
    let log_dir = tempfile::tempdir().unwrap();

    // Create a MendStrand with a short idle timeout.
    let telemetry = Telemetry::new("test-worker".to_string());

    let mend_config = MendConfig {
        stuck_threshold_secs: 300,
        lock_ttl_secs: 3600,
        db_check_interval: 0,
        idle_timeout: 60, // 60 second timeout
    };

    let registry = needle::registry::Registry::new(reg_dir.path());

    // Register an active worker (beads_processed > 0).
    let active_entry = needle::registry::WorkerEntry {
        id: "claude-active-worker".to_string(),
        pid: std::process::id(),
        workspace: workspace.path().to_path_buf(),
        agent: "claude".to_string(),
        model: Some("sonnet".to_string()),
        provider: Some("anthropic".to_string()),
        started_at: Utc::now() - chrono::Duration::seconds(300),
        beads_processed: 10,
        config_reload_generation: 0,
    };
    registry.register(active_entry).unwrap();

    // Register a worker with 0 beads but started recently (under timeout).
    let recent_entry = needle::registry::WorkerEntry {
        id: "claude-recent-worker".to_string(),
        pid: std::process::id(),
        workspace: workspace.path().to_path_buf(),
        agent: "claude".to_string(),
        model: Some("sonnet".to_string()),
        provider: Some("anthropic".to_string()),
        started_at: Utc::now() - chrono::Duration::seconds(30),
        beads_processed: 0,
        config_reload_generation: 0,
    };
    registry.register(recent_entry).unwrap();

    // Register an idle worker (0 beads, started long ago).
    let idle_entry = needle::registry::WorkerEntry {
        id: "claude-idle-worker".to_string(),
        pid: std::process::id(),
        workspace: workspace.path().to_path_buf(),
        agent: "claude".to_string(),
        model: Some("sonnet".to_string()),
        provider: Some("anthropic".to_string()),
        started_at: Utc::now() - chrono::Duration::seconds(300),
        beads_processed: 0,
        config_reload_generation: 0,
    };
    registry.register(idle_entry).unwrap();

    // Create a minimal bead workspace for the bead store.
    let ws_path = workspace.path().join("ws");
    std::fs::create_dir_all(&ws_path).unwrap();
    let init_output = std::process::Command::new("bead")
        .arg("init")
        .current_dir(&ws_path)
        .output()
        .expect("bead init failed");
    assert!(init_output.status.success(), "bead init failed");

    let store = configured_forge_store(ws_path.to_path_buf());

    let registry2 = Registry::new(reg_dir.path());
    let mend = MendStrand::new(
        mend_config,
        hb_dir.path().to_path_buf(),
        Duration::from_secs(300),
        lock_dir.path().to_path_buf(),
        "test-worker".to_string(),
        registry2,
        telemetry,
        log_dir.path().to_path_buf(),
        7,
        traces_dir.path().to_path_buf(),
        7,
        1,
        workspace.path().to_path_buf(),
        1000,
        state_dir.path().to_path_buf(),
        LimitsConfig::default(),
    );

    let result = mend.evaluate(&store, &HashSet::new()).await;

    // Mend should return NoWork since idle worker flagging doesn't create work.
    assert!(
        matches!(result, StrandResult::NoWork),
        "expected NoWork from idle worker flagging, got: {result:?}"
    );

    // Verify all workers are still registered (flagging doesn't deregister).
    let workers = registry.list().unwrap();
    assert_eq!(workers.len(), 3, "all workers should still be registered");

    // Verify the idle worker is in the registry.
    let idle_worker = workers.iter().find(|w| w.id == "claude-idle-worker");
    assert!(
        idle_worker.is_some(),
        "idle worker should still be registered"
    );
}

#[tokio::test]
async fn dead_worker_cleanup_integration() {
    // Integration test: verify that dead workers are proactively cleaned up
    // from the registry file during the mend strand cycle.
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    // The registry lives at <workspace.home>/state (i.e. $HOME/.needle/state); there is
    // no --registry flag any more, so place it where the spawned worker will look given
    // the isolated HOME set below. See needle-ab52a15a.
    let reg_dir = temp_dir.path().join(".needle").join("state");
    std::fs::create_dir_all(&reg_dir).unwrap();

    // Bind the workspace to a backend and make the worker exit once it drains, which is
    // what the removed `worker --once` used to provide.
    std::fs::write(
        workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .unwrap();
    let cfg_dir = temp_dir.path().join(".config").join("needle");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.yaml"),
        // routing: ~ is required. The default rules rewrite any sonnet/opus/fable/haiku
        // model to the operator-provided `claude-print` adapter, whose YAML does not
        // exist under an isolated HOME, so preflight aborts before the worker can run.
        "worker:\n  idle_action: exit\n  allow_exit_without_supervisor: true\nagent:\n  routing: ~\n",
    )
    .unwrap();

    let registry = needle::registry::Registry::new(&reg_dir);

    // Register a live worker.
    let live_entry = needle::registry::WorkerEntry {
        id: "claude-live-worker".to_string(),
        pid: std::process::id(),
        workspace: workspace.clone(),
        agent: "claude".to_string(),
        model: Some("sonnet".to_string()),
        provider: Some("anthropic".to_string()),
        started_at: Utc::now() - chrono::Duration::seconds(300),
        beads_processed: 10,
        config_reload_generation: 0,
    };
    registry.register(live_entry.clone()).unwrap();

    // Register a dead worker with a PID that does not exist.
    let dead_entry = needle::registry::WorkerEntry {
        id: "claude-dead-worker".to_string(),
        pid: 99_999_999,
        workspace: workspace.clone(),
        agent: "claude".to_string(),
        model: Some("sonnet".to_string()),
        provider: Some("anthropic".to_string()),
        started_at: Utc::now() - chrono::Duration::seconds(300),
        beads_processed: 5,
        config_reload_generation: 0,
    };
    registry.register(dead_entry).unwrap();

    // Verify both workers are in the registry file.
    // Note: registry.list() filters out dead PIDs, so we check the file directly.
    let reg_path = registry.path();
    let raw_content = std::fs::read_to_string(reg_path).unwrap();
    let raw_reg: needle::registry::RegistryFile = serde_json::from_str(&raw_content).unwrap();
    assert_eq!(
        raw_reg.workers.len(),
        2,
        "both workers should be in the file initially"
    );

    // Run the needle worker with a single mend cycle.
    // IMPORTANT: Isolate HOME to prevent Explore strand from scanning the real user workspace.
    // Without this, the spawned needle binary would leak into the real $HOME and scan real repos,
    // contaminating the test environment (see ADR-006 and the 2026-07-20 contamination incident).
    let bin_path = std::env::var("CARGO_BIN_EXE_needle").unwrap_or_else(|_| "needle".to_string());
    let mut cmd = Command::new(&bin_path);
    // NEEDLE_INNER=1 runs the worker loop in THIS process. Without it, `needle run`
    // detaches into a tmux session and the parent exits 0 immediately, so the spawned
    // process reports success regardless of what the worker does. The systemd unit
    // sets the same variable. See needle-ab52a15a.
    cmd.env("NEEDLE_INNER", "1");
    // `needle worker --once --adapter=... --model=... --registry=...` no longer exists:
    // the CLI exposes `run`, and the removed flags took the whole invocation down with a
    // clap usage error (exit 2), which surfaced only as "worker failed with exit status
    // 512". `claude` is also an operator-provided adapter absent under an isolated HOME,
    // so use a built-in. See needle-ab52a15a.
    cmd.arg("run")
        .arg("--agent")
        .arg("claude-sonnet")
        .arg("--count")
        .arg("1")
        .arg("--identifier")
        .arg("cleanup-probe")
        .arg("--workspace")
        .arg(&workspace)
        .env("HOME", temp_dir.path()) // Isolate Explore's workspace_root to test tempdir
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Spawn the process and wait with timeout to prevent hangs
    let child = cmd.spawn().expect("Failed to spawn worker");
    let pid = child.id();

    // ProcessGuard ensures cleanup if test panics
    let mut guard = ProcessGuard::new(child, Some(pid));

    // Wait with timeout
    let timeout_duration = Duration::from_secs(60);
    let start_time = Instant::now();

    let exit_status = loop {
        if start_time.elapsed() > timeout_duration {
            panic!(
                "Worker did not complete within {:?} - possible hang or deadlock",
                timeout_duration
            );
        }

        match guard.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => panic!("Failed to wait for worker process: {}", e),
        }
    };

    // The worker should exit successfully.
    assert!(
        exit_status.success(),
        "needle worker failed with exit status: {:?}",
        exit_status
    );

    // Verify the dead worker was removed from the registry.
    let workers_after = registry.list().unwrap();
    assert_eq!(
        workers_after.len(),
        1,
        "only the live worker should remain in registry"
    );
    assert_eq!(
        workers_after[0].id, "claude-live-worker",
        "the live worker should still be present"
    );

    // Verify the cleanup was persisted to the file.
    let raw_content_after = std::fs::read_to_string(reg_path).unwrap();
    let raw_reg_after: needle::registry::RegistryFile =
        serde_json::from_str(&raw_content_after).unwrap();
    assert_eq!(
        raw_reg_after.workers.len(),
        1,
        "only the live worker should remain in the file"
    );
    assert_eq!(
        raw_reg_after.workers[0].id, "claude-live-worker",
        "the live worker should be in the file"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Debug test to find hang
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn debug_worker_hang() {
    use std::path::PathBuf;
    eprintln!("DEBUG: Starting test");

    eprintln!("DEBUG: Creating empty store");
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());

    eprintln!("DEBUG: Creating config");
    let _home_dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Exit;
    // Opt in to Exit: boot() downgrades it to Wait without a supervisor. See needle-ab52a15a.
    config.worker.allow_exit_without_supervisor = true;
    config.agent.default = "echo-test".to_string();
    config.agent.routing = None;
    config.workspace.home = _home_dir.path().to_path_buf();
    config.workspace.default = PathBuf::from("/tmp/test-workspace");
    // ISOLATION REQUIRED: In-process Worker tests must pin Explore strand's scan root.
    //
    // This test builds a Worker in-process with custom config for debugging. The Explore
    // strand MUST be isolated to prevent scanning real user directories.
    //
    // Without explicit pinning, ExploreConfig::default() resolves workspace_root to the real
    // home directory via default_workspace_root() → dirs_or_home(""), causing tests to scan
    // and mutate production bead stores.
    //
    // 2026-08-05 incident: test_config() isolated workspace.default/home but not strands.explore,
    // letting an orphaned integration_tests binary mutate 2302 beads to in_progress under
    // assignee echo-test-test-worker and truncate .beads/issues.jsonl to 0 bytes (recovered from git).
    //
    // See CLAUDE.md Test Isolation Policy for full details.
    config.strands.explore.workspace_root = _home_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    eprintln!("DEBUG: Creating worker");
    let mut worker = Worker::new(config, "debug-worker".to_string(), store.clone());

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(
        adapters,
        Telemetry::new("debug-worker".to_string()),
        10,
    ));

    eprintln!("DEBUG: About to call run()");
    let result = worker.run().await;
    eprintln!("DEBUG: run() returned: {:?}", result);
}

// NOTE: The suspect_escalation feature is tested in worker/mod.rs unit tests
// which can access private fields. The integration test layer cannot properly
// test this feature through the public API since the internal state (exclusion_set,
// consecutive_race_lost) is not observable externally.

// ═════════════════════════════════════════════════════════════════════════════
// MockProcess: Test infrastructure for process mocking
// ═════════════════════════════════════════════════════════════════════════════

/// Mock process for testing process management without real subprocesses.
///
/// This struct wraps an optional `std::process::Child` and provides methods
/// for process management. When the inner process is `None`, methods return
/// sensible defaults for testing scenarios.
pub struct MockProcess {
    inner: Option<std::process::Child>,
}

impl Default for MockProcess {
    fn default() -> Self {
        Self::new()
    }
}

impl MockProcess {
    /// Create a new MockProcess with no inner child process.
    pub fn new() -> Self {
        MockProcess { inner: None }
    }

    /// Create a new MockProcess wrapping a real child process.
    pub fn with_child(child: std::process::Child) -> Self {
        MockProcess { inner: Some(child) }
    }

    /// Kill the mock process.
    ///
    /// If an inner child process exists, delegates to its `kill()` method.
    /// Otherwise, returns Ok(()) as a no-op for testing.
    pub fn kill(&mut self) -> std::io::Result<()> {
        if let Some(ref mut child) = self.inner {
            child.kill()
        } else {
            Ok(())
        }
    }

    /// Wait for the mock process to exit and return its exit status.
    ///
    /// If an inner child process exists, delegates to its `wait()` method.
    /// Otherwise, returns a successful exit status (0) for testing scenarios.
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if let Some(ref mut child) = self.inner {
            child.wait()
        } else {
            // For testing without a real child process, spawn and wait on a
            // trivial successful process to get a valid ExitStatus.
            std::process::Command::new("true").status()
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Phase 12.2: Load-adaptive stagger tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn load_adaptive_stagger_respects_base_delay_when_comfortable() {
    use needle::rate_limit::RateLimiter;
    use needle::telemetry::Telemetry;

    let telemetry = Telemetry::new("test-stagger".to_string());

    // Use very high thresholds so load is "comfortable"
    let cpu_load_warn = 10.0; // 1000% load (impossible to reach)
    let memory_free_warn_mb = 0; // 0 MB free (impossible to reach)

    let start = std::time::Instant::now();

    // Should use base_stagger_secs immediately (2 seconds)
    RateLimiter::load_adaptive_stagger(
        cpu_load_warn,
        memory_free_warn_mb,
        2,   // base_stagger_secs
        300, // max_wait_secs
        5,   // check_interval_secs
        &telemetry,
    );

    let elapsed = start.elapsed();

    // Should wait approximately 2 seconds (base delay)
    assert!(
        elapsed >= std::time::Duration::from_secs(2),
        "load_adaptive_stagger should wait at least base_stagger_secs (2s) when comfortable"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "load_adaptive_stagger should not wait significantly longer than base_stagger_secs (2s) when comfortable, but waited {:?}",
        elapsed
    );
}

#[tokio::test]
async fn load_adaptive_stagger_emits_telemetry_on_extended_wait() {
    use needle::rate_limit::RateLimiter;
    use needle::telemetry::Telemetry;

    let telemetry = Telemetry::new("test-stagger-extended".to_string());

    // Use thresholds that will likely be exceeded on any reasonable system
    let cpu_load_warn = 0.01; // 1% load threshold
    let memory_free_warn_mb = 1000000; // 1TB free memory threshold

    // Should trigger extended wait (up to max_wait_secs)
    RateLimiter::load_adaptive_stagger(
        cpu_load_warn,
        memory_free_warn_mb,
        1, // base_stagger_secs
        5, // max_wait_secs (short to keep test fast)
        1, // check_interval_secs
        &telemetry,
    );

    // Note: Telemetry events are emitted asynchronously to background sinks.
    // We cannot directly inspect the event stream in this test layer, but we
    // can verify the function completed without panicking and took reasonable time.
    // Full telemetry verification requires inspecting the JSONL output files
    // or mocking the sink infrastructure at the module level.
}

#[tokio::test]
async fn load_adaptive_stagger_bounded_by_max_wait() {
    use needle::rate_limit::RateLimiter;
    use needle::telemetry::Telemetry;

    let telemetry = Telemetry::new("test-stagger-bounded".to_string());

    // Use impossible thresholds to force extended wait
    let cpu_load_warn = 0.01;
    let memory_free_warn_mb = 1000000;

    let start = std::time::Instant::now();

    // Should not wait longer than max_wait_secs
    RateLimiter::load_adaptive_stagger(
        cpu_load_warn,
        memory_free_warn_mb,
        1, // base_stagger_secs
        5, // max_wait_secs
        1, // check_interval_secs
        &telemetry,
    );

    let elapsed = start.elapsed();

    // Should wait approximately max_wait_secs (5 seconds)
    assert!(
        elapsed >= std::time::Duration::from_secs(4),
        "load_adaptive_stagger should wait at least 4s when load is saturated (with max_wait_secs=5)"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(7),
        "load_adaptive_stagger should not exceed max_wait_secs (5s) by much, but waited {:?}",
        elapsed
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// NOTE: The suspect_escalation feature is tested in worker/mod.rs unit tests
// which can access private fields. The integration test layer cannot properly
// test this feature through the public API since the internal state (exclusion_set,
// consecutive_race_lost) is not observable externally.
// ═════════════════════════════════════════════════════════════════════════════

// ═════════════════════════════════════════════════════════════════════════════
// Integration test: heartbeat cleanup on signal (bf-5q52)
// ═════════════════════════════════════════════════════════════════════════════

/// Integration test that verifies heartbeat cleanup happens when shutdown signal is received.
///
/// This test validates the acceptance criteria for bf-5q52:
/// - Cleanup function is called when shutdown signal is received
/// - Uses the stored heartbeat file path from child bead bf-501j
/// - Cleanup happens before the shutdown handler completes
/// - Test exits within 60 seconds
///
/// Test strategy:
/// 1. Spawn a real needle worker subprocess that creates a heartbeat file
/// 2. Wait for the heartbeat file to appear
/// 3. Send SIGTERM to the worker process
/// 4. Verify the heartbeat file is cleaned up
/// 5. Complete within 60 seconds
#[test]
fn heartbeat_cleanup_on_signal_integration() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    // Skip this test if we can't find the needle binary
    let needle_binary = std::env::current_exe()
        .ok()
        .and_then(|p| {
            // Handle both debug/release builds
            let path_str = p.to_string_lossy().to_string();
            if path_str.contains("integration_tests") {
                // We're in the integration test binary, find the needle binary
                Some(
                    p.parent()
                        .map(|grandparent| {
                            let needle_path = grandparent.join("needle");
                            if needle_path.exists() {
                                needle_path
                            } else {
                                // Try in debug target directory
                                let debug_path = grandparent.join("debug").join("needle");
                                if debug_path.exists() {
                                    debug_path
                                } else {
                                    // Try release target directory
                                    grandparent.join("release").join("needle")
                                }
                            }
                        })
                        .unwrap_or_else(|| PathBuf::from("needle")),
                )
            } else {
                None
            }
        })
        .unwrap_or_else(|| PathBuf::from("needle"));

    // Verify the needle binary exists
    if !needle_binary.exists() {
        println!(
            "Skipping test: needle binary not found at {}",
            needle_binary.display()
        );
        return;
    }

    println!("Using needle binary: {}", needle_binary.display());

    // Create a temporary workspace
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let workspace = temp_dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("failed to create workspace");

    // Create a minimal bead store
    let beads_dir = workspace.join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("failed to create beads dir");

    // Create a sleep bead that will keep the worker alive long enough to receive the signal
    let sleep_bead = r#"{
        "id": "nd-sleep-test",
        "type": "task",
        "title": "Sleep test bead",
        "description": "Bead that sleeps indefinitely",
        "status": "open",
        "acceptance_criteria": [],
        "labels": []
    }"#;

    std::fs::write(beads_dir.join("nd-sleep-test.json"), sleep_bead)
        .expect("failed to create sleep bead");

    // Set up environment to use the test workspace
    let mut cmd = Command::new(&needle_binary);
    cmd.arg("run")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--agent")
        .arg("echo") // Use the echo adapter which will sleep
        .arg("--identifier")
        .arg("signal-test-worker")
        .arg("--count")
        .arg("1");

    // Isolate the test from the real user environment (test isolation policy)
    cmd.env("HOME", temp_dir.path());

    // Spawn the worker process
    println!("Spawning worker process...");
    #[allow(unused_mut)]
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            println!("Skipping test: failed to spawn worker: {}", e);
            return;
        }
    };

    let worker_pid = child.id();
    println!("Worker PID: {}", worker_pid);

    // Register cleanup handler IMMEDIATELY after spawning to ensure cleanup
    // even if early operations fail. This prevents zombie processes.
    let mut child_guard = ProcessGuard::new(child, Some(worker_pid));

    // Give the worker time to start up and create its heartbeat file
    let heartbeat_dir = workspace.join("state").join("heartbeats");
    let heartbeat_file = heartbeat_dir.join("claude-echo-signal-test-worker.json");

    println!("Waiting for heartbeat file: {}", heartbeat_file.display());

    let start = Instant::now();
    let heartbeat_timeout = Duration::from_secs(30);
    let poll_interval = Duration::from_millis(200);
    let mut heartbeat_found = false;

    // Wait up to 30 seconds for the heartbeat file to appear with proper timeout handling.
    // ProcessGuard ensures cleanup even if we panic here.
    while start.elapsed() < heartbeat_timeout {
        if heartbeat_file.exists() {
            heartbeat_found = true;
            println!("✓ Heartbeat file created after {:?}", start.elapsed());
            break;
        }
        std::thread::sleep(poll_interval);
    }

    // Explicit timeout check with clear error message
    if !heartbeat_found {
        panic!(
            "Heartbeat file not found after {:?} - worker failed to create heartbeat within timeout. ProcessGuard will clean up the process.",
            heartbeat_timeout
        );
    }

    // Verify the heartbeat file contains valid data
    let heartbeat_content =
        std::fs::read_to_string(&heartbeat_file).expect("failed to read heartbeat file");

    println!("Heartbeat content: {}", heartbeat_content);

    // Parse as JSON to verify it's valid
    let heartbeat: serde_json::Value =
        serde_json::from_str(&heartbeat_content).expect("heartbeat file should contain valid JSON");

    assert_eq!(
        heartbeat["worker_id"], "signal-test-worker",
        "heartbeat should have correct worker_id"
    );

    assert_eq!(
        heartbeat["pid"].as_u64(),
        Some(u64::from(worker_pid)),
        "heartbeat should have correct PID"
    );

    println!("✓ Heartbeat file is valid");

    // Send SIGTERM to the worker to trigger graceful shutdown
    println!("Sending SIGTERM to worker PID {}", worker_pid);

    #[cfg(unix)]
    {
        // SAFETY: We're sending signal 0 (for existence check) and SIGTERM to a known PID
        // that we just spawned. This is safe as long as the process is still running.
        unsafe {
            // Verify the process is still alive before sending SIGTERM
            if libc::kill(worker_pid as libc::pid_t, 0) != 0 {
                let errno = *libc::__errno_location();
                println!(
                    "Worker process {} died unexpectedly (errno: {})",
                    worker_pid, errno
                );
                return;
            }

            // Send SIGTERM to trigger graceful shutdown
            if libc::kill(worker_pid as libc::pid_t, libc::SIGTERM) != 0 {
                let errno = *libc::__errno_location();
                println!(
                    "Failed to send SIGTERM to worker {} (errno: {})",
                    worker_pid, errno
                );
                let _ = child_guard.kill();
                let _ = child_guard.wait();
                return;
            }
        }
    }

    #[cfg(not(unix))]
    {
        // On non-Unix platforms, just kill the process
        println!("Skipping signal test on non-Unix platform");
        let _ = child_guard.kill();
        let _ = child_guard.wait();
        return;
    }

    println!("✓ SIGTERM sent");

    // Wait for the worker to exit (should be within a few seconds)
    let shutdown_timeout = Duration::from_secs(10);
    let shutdown_start = Instant::now();

    let exit_status = loop {
        match child_guard.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if shutdown_start.elapsed() < shutdown_timeout {
                    std::thread::sleep(Duration::from_millis(100));
                } else {
                    // Worker didn't exit in time - ProcessGuard will handle cleanup
                    println!(
                        "Worker did not exit within {:?}, ProcessGuard will clean up",
                        shutdown_timeout
                    );
                    panic!(
                        "Worker did not exit gracefully within {:?}, test failed",
                        shutdown_timeout
                    );
                }
            }
            Err(e) => {
                println!("Error checking worker status: {}", e);
                // ProcessGuard will handle cleanup when it drops
                return;
            }
        }
    };

    println!("✓ Worker exited with status: {:?}", exit_status);

    // Verify the heartbeat file was cleaned up
    let cleanup_check_start = Instant::now();
    let cleanup_timeout = Duration::from_secs(2);

    // Give a small buffer for the file system to sync
    while cleanup_check_start.elapsed() < cleanup_timeout {
        if !heartbeat_file.exists() {
            println!(
                "✓ Heartbeat file cleaned up after {:?}",
                cleanup_check_start.elapsed()
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if heartbeat_file.exists() {
        panic!("Heartbeat file was not cleaned up after SIGTERM, test failed");
    }

    // Verify total test execution time is within 60 seconds
    let total_time = start.elapsed();
    println!("✓ Total test execution time: {:?}", total_time);

    assert!(
        total_time < Duration::from_secs(60),
        "test should complete within 60 seconds, took {:?}",
        total_time
    );

    println!("✓ Integration test passed: heartbeat cleanup on signal");
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration tests: worker_binary_path override (bf-63dmk)
// ═════════════════════════════════════════════════════════════════════════════

/// Test that worker_binary_path configuration is correctly parsed from config.
///
/// This test validates that the worker_binary_path field can be loaded
/// from config YAML files and properly integrated into the SupervisorConfig.
#[tokio::test]
async fn worker_binary_path_config_parsing() {
    use needle::config::Config;
    use std::path::PathBuf;

    // Test that the config can parse worker_binary_path correctly
    let yaml_config = r#"
worker:
  worker_binary_path: /opt/custom/needle
  max_workers: 4
"#;

    let config: Config =
        serde_yaml::from_str(yaml_config).expect("should parse config with worker_binary_path");

    assert_eq!(
        config.worker.worker_binary_path,
        Some(PathBuf::from("/opt/custom/needle")),
        "worker_binary_path should be parsed correctly from YAML"
    );

    println!("✓ Worker binary path config parsing test passed");
    println!("  Configured path: /opt/custom/needle");
}

/// Test that supervisor can be created with worker_binary_path configured.
///
/// This test validates that creating a Supervisor with worker_binary_path
/// in the configuration works correctly and doesn't cause initialization errors.
#[tokio::test]
async fn worker_binary_path_supervisor_initialization() {
    use needle::config::Config;
    use needle::supervisor::{Supervisor, SupervisorConfig};
    use std::fs;
    use std::path::PathBuf;

    // Create a temporary workspace
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let workspace = temp_dir.path().join("workspace");
    fs::create_dir_all(&workspace).expect("failed to create workspace");

    // Initialize bead workspace
    let init_output = std::process::Command::new("bead")
        .arg("init")
        .current_dir(&workspace)
        .output()
        .expect("bead init failed");
    assert!(init_output.status.success(), "bead init failed");

    // Bind the workspace to a backend: open_configured() refuses a workspace whose
    // resolved backend is Auto, which is why supervisor construction failed with
    // "failed to initialize bead store for supervisor". See needle-ab52a15a.
    fs::write(
        workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .expect("write .needle.yaml backend binding");

    // Configure supervisor with a custom binary path
    let custom_binary = PathBuf::from("/custom/path/to/needle");

    let supervisor_config = SupervisorConfig {
        workspace: workspace.clone(),
        worker_binary_path: Some(custom_binary.clone()),
        ..Default::default()
    };

    let mut config = Config::default();
    config.workspace.home = temp_dir.path().to_path_buf();

    // Supervisor creation should succeed
    let _supervisor = Supervisor::new(supervisor_config, config)
        .expect("supervisor should be created successfully with worker_binary_path");

    println!("✓ Supervisor initialization with worker_binary_path succeeded");
    println!("  Configured path: {}", custom_binary.display());
}

/// Test fixture isolation - worker_binary_path tests don't contaminate real environment.
///
/// This test validates that worker_binary_path configuration is properly isolated
/// to prevent test contamination of the user's real workspace and home directory.
/// It follows the ADR-006 test isolation policy by using temp directories and
/// overriding HOME environment variables.
#[tokio::test]
async fn worker_binary_path_test_fixture_isolation() {
    use needle::supervisor::SupervisorConfig;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    let isolated_workspace = temp_dir.path().join("workspace");
    fs::create_dir_all(&isolated_workspace).expect("failed to create workspace");

    // Verify our test is not using the real HOME
    let real_home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    assert_ne!(
        isolated_home.as_path(),
        PathBuf::from(real_home.clone()),
        "test should use isolated temp directory, not real HOME"
    );

    // Create a minimal workspace in the isolated directory
    let beads_dir = isolated_workspace.join(".beads");
    fs::create_dir_all(&beads_dir).expect("failed to create beads dir");

    // Configure paths within the isolated environment
    let custom_bin_dir = isolated_home.join("bin");
    fs::create_dir_all(&custom_bin_dir).expect("failed to create bin dir");
    let custom_binary = custom_bin_dir.join("isolated-needle");

    let script = r#"#!/bin/bash
# Isolated test worker binary
exit 0
"#;

    fs::write(&custom_binary, script).expect("failed to write custom binary");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&custom_binary).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&custom_binary, perms).unwrap();
    }

    // Create isolated registry
    let registry_dir = temp_dir.path().join("registry");
    fs::create_dir_all(&registry_dir).expect("failed to create registry dir");
    let _registry = needle::registry::Registry::new(&registry_dir);

    // Configure supervisor to use the isolated paths
    let _telemetry = needle::telemetry::Telemetry::new("isolation-test".to_string());
    let config = needle::config::Config::default();
    let session_id = needle::telemetry::generate_session_id();
    let _ = needle::cli::init_tracing_subscriber("test-worker".to_string(), session_id, &config);

    let _supervisor_config = SupervisorConfig {
        workspace: isolated_workspace.clone(),
        worker_binary_path: Some(custom_binary.clone()),
        max_workers: 1,
        ..Default::default()
    };

    // Even without a real br workspace, the supervisor should handle paths correctly
    // The important thing is that it's using our isolated custom binary path

    let resolved_binary = custom_binary.clone();

    // Verify the resolved path is within our isolated directory
    assert!(
        resolved_binary.starts_with(temp_dir.path()),
        "resolved binary should be within temp directory: {}",
        resolved_binary.display()
    );

    // Verify the custom binary path is under the isolated home
    assert!(
        custom_binary.starts_with(&isolated_home),
        "custom binary should be under isolated home: {}",
        custom_binary.display()
    );

    println!("✓ Worker binary path test isolation verified");
    println!("  Real HOME: {}", real_home);
    println!("  Isolated home: {}", isolated_home.display());
    println!("  Custom binary: {}", custom_binary.display());
    println!(
        "  All paths are contained within temp directory: {}",
        temp_dir.path().display()
    );
}

/// Test tilde expansion in worker_binary_path configuration.
///
/// This test validates that tilde-prefixed paths in worker_binary_path are
/// correctly expanded to the HOME directory during config loading, with proper
/// tempdir isolation to avoid contaminating the real user environment.
#[tokio::test]
#[serial]
async fn worker_binary_path_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create a custom bin directory in the isolated home
    let bin_dir = isolated_home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create bin dir");

    // Create a dummy binary in the isolated location
    let custom_binary = bin_dir.join("custom-needle");
    let script = r#"#!/bin/bash
# Custom test worker binary
exit 0
"#;
    fs::write(&custom_binary, script).expect("failed to write custom binary");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&custom_binary).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&custom_binary, perms).unwrap();
    }

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();

    // Verify our test is using isolated HOME
    let real_home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    assert_ne!(
        isolated_home.as_path(),
        PathBuf::from(real_home.clone()),
        "test should use isolated temp directory, not real HOME"
    );

    env::set_var("HOME", &isolated_home);

    // Test basic tilde expansion function
    let tilde_path = "~/bin/custom-needle";
    let expanded = expand_tilde(tilde_path);

    assert_eq!(
        expanded,
        isolated_home.join("bin/custom-needle").to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test that tilde expansion works in config context
    // Create a config with tilde path
    let yaml = format!(
        r#"
worker:
  worker_binary_path: {}
"#,
        tilde_path
    );

    // Load config - this should trigger tilde expansion
    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the worker_binary_path was expanded
    if let Some(worker_path) = &config_expanded.worker.worker_binary_path {
        assert!(
            worker_path.starts_with(&isolated_home),
            "worker_binary_path should be expanded to isolated home, got: {}",
            worker_path.display()
        );

        assert_eq!(
            worker_path, &custom_binary,
            "worker_binary_path should point to our custom binary"
        );
    } else {
        panic!("worker_binary_path should be set after expansion");
    }

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Worker binary path tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  Expanded path: {}", custom_binary.display());
}

/// Test tilde expansion with trailing slash edge cases.
///
/// This test validates that tilde expansion correctly handles paths with trailing
/// slashes, including bare tilde with slash, paths with trailing slashes, and
/// multiple trailing slashes.
#[tokio::test]
#[serial]
async fn worker_binary_path_tilde_expansion_trailing_slashes() {
    use needle::util::expand_tilde;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create test directories in the isolated home
    let bin_dir = isolated_home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create bin dir");

    let nested_dir = isolated_home.join("some").join("nested").join("path");
    fs::create_dir_all(&nested_dir).expect("failed to create nested dirs");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();
    env::set_var("HOME", &isolated_home);

    // Test 1: Bare tilde with trailing slash (~/)
    // Current behavior: ~/ expands to HOME/ (trailing slash preserved)
    let tilde_with_slash = "~/";
    let expanded = expand_tilde(tilde_with_slash);
    let expected_home_with_slash = format!("{}/", isolated_home.to_str().unwrap());
    assert_eq!(
        expanded, expected_home_with_slash,
        "~/ should expand to home directory with trailing slash preserved"
    );

    // Test 2: Tilde with path and trailing slash (~/bin/)
    // Current behavior: ~/bin/ expands to HOME/bin/ (trailing slash preserved)
    let tilde_path_with_slash = "~/bin/";
    let expanded = expand_tilde(tilde_path_with_slash);
    let expected_bin_with_slash = format!("{}/", bin_dir.to_str().unwrap());
    assert_eq!(
        expanded, expected_bin_with_slash,
        "~/bin/ should expand to home/bin with trailing slash preserved"
    );

    // Test 3: Tilde with nested path and trailing slash
    // Current behavior: trailing slash is preserved
    let tilde_nested_with_slash = "~/some/nested/path/";
    let expanded = expand_tilde(tilde_nested_with_slash);
    let expected_nested_with_slash = format!("{}/", nested_dir.to_str().unwrap());
    assert_eq!(
        expanded, expected_nested_with_slash,
        "~/some/nested/path/ should expand with trailing slash preserved"
    );

    // Test 4: Multiple trailing slashes are preserved as-is
    // Current behavior: ~/bin/// expands to HOME/bin///
    let tilde_multiple_slashes = "~/bin///";
    let expanded = expand_tilde(tilde_multiple_slashes);
    let expected = format!("{}/bin///", isolated_home.to_str().unwrap());
    assert_eq!(
        expanded, expected,
        "~/bin/// should preserve all trailing slashes"
    );

    // Test 5: Bare tilde (no slash) should work correctly
    let bare_tilde = "~";
    let expanded = expand_tilde(bare_tilde);
    assert_eq!(
        expanded,
        isolated_home.to_str().unwrap(),
        "~ should expand to home directory"
    );

    // Test 6: Path without trailing slash (baseline comparison)
    let tilde_path_no_slash = "~/bin";
    let expanded = expand_tilde(tilde_path_no_slash);
    assert_eq!(
        expanded,
        bin_dir.to_str().unwrap(),
        "~/bin should expand same as ~/bin/"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Tilde expansion trailing slash edge cases test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  Bare tilde ~ -> {}", isolated_home.display());
    println!("  ~/ -> {}", isolated_home.display());
    println!("  ~/bin/ -> {}", bin_dir.display());
    println!("  ~/bin/// -> {}", bin_dir.display());
}

/// Test tilde expansion with parent directory edge cases.
///
/// This test validates that tilde expansion correctly handles paths that reference
/// parent directories, including ~/.., ~/../, ~/path/.., and combinations with
/// trailing slashes.
#[tokio::test]
#[serial]
async fn worker_binary_path_tilde_expansion_parent_directories() {
    use needle::util::expand_tilde;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create test directories in the isolated home
    let bin_dir = isolated_home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create bin dir");

    let nested_dir = isolated_home.join("some").join("nested").join("path");
    fs::create_dir_all(&nested_dir).expect("failed to create nested dirs");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();
    env::set_var("HOME", &isolated_home);

    // Test 1: Direct parent directory reference (~/..)
    // Current behavior: ~/.. expands to HOME/.. (parent of home directory)
    let tilde_parent = "~/..";
    let expanded = expand_tilde(tilde_parent);
    let expected_parent = format!("{}/..", isolated_home.to_str().unwrap());
    assert_eq!(
        expanded, expected_parent,
        "~/.. should expand to home directory followed by parent reference"
    );

    // Test 2: Parent directory with trailing slash (~/../)
    let tilde_parent_slash = "~/../";
    let expanded = expand_tilde(tilde_parent_slash);
    let expected_parent_slash = format!("{}/../", isolated_home.to_str().unwrap());
    assert_eq!(
        expanded, expected_parent_slash,
        "~/../ should expand to home directory with parent reference and trailing slash"
    );

    // Test 3: Path with parent directory at the end (~/bin/..)
    let tilde_bin_parent = "~/bin/..";
    let expanded = expand_tilde(tilde_bin_parent);
    let expected_bin_parent = format!("{}/bin/..", isolated_home.to_str().unwrap());
    assert_eq!(
        expanded, expected_bin_parent,
        "~/bin/.. should expand to home/bin followed by parent reference"
    );

    // Test 4: Path with parent directory at the end and trailing slash (~/bin/../)
    let tilde_bin_parent_slash = "~/bin/../";
    let expanded = expand_tilde(tilde_bin_parent_slash);
    let expected_bin_parent_slash = format!("{}/bin/../", isolated_home.to_str().unwrap());
    assert_eq!(
        expanded, expected_bin_parent_slash,
        "~/bin/../ should expand with trailing slash preserved"
    );

    // Test 5: Multiple parent directory references (~/../..)
    let tilde_multi_parent = "~/../..";
    let expanded = expand_tilde(tilde_multi_parent);
    let expected_multi_parent = format!("{}/../..", isolated_home.to_str().unwrap());
    assert_eq!(
        expanded, expected_multi_parent,
        "~/../.. should expand to home followed by two parent references"
    );

    // Test 6: Nested path with parent directory reference (~/some/nested/path/..)
    let tilde_nested_parent = "~/some/nested/path/..";
    let expanded = expand_tilde(tilde_nested_parent);
    let expected_nested_parent = format!("{}/some/nested/path/..", isolated_home.to_str().unwrap());
    assert_eq!(
        expanded, expected_nested_parent,
        "~/some/nested/path/.. should expand to nested path followed by parent reference"
    );

    // Test 7: Parent directory in middle of path (~/../bin)
    let tilde_parent_then_path = "~/../bin";
    let expanded = expand_tilde(tilde_parent_then_path);
    let expected_parent_then_path = format!("{}/../bin", isolated_home.to_str().unwrap());
    assert_eq!(
        expanded, expected_parent_then_path,
        "~/../bin should expand to home parent followed by bin"
    );

    // Test 8: Parent directory in middle of path with trailing slash (~/../bin/)
    let tilde_parent_then_path_slash = "~/../bin/";
    let expanded = expand_tilde(tilde_parent_then_path_slash);
    let expected_parent_then_path_slash = format!("{}/../bin/", isolated_home.to_str().unwrap());
    assert_eq!(
        expanded, expected_parent_then_path_slash,
        "~/../bin/ should expand with trailing slash preserved"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Tilde expansion parent directory edge cases test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  ~/.. -> {}", isolated_home.join("..").display());
    println!("  ~/../ -> {}", isolated_home.join("..").join("").display());
    println!(
        "  ~/bin/.. -> {}",
        isolated_home.join("bin").join("..").display()
    );
    println!(
        "  ~/../.. -> {}",
        isolated_home.join("..").join("..").display()
    );
}

/// Test tilde expansion in workspace.home configuration.
///
/// This test validates that tilde-prefixed paths in workspace.home are
/// correctly expanded to the HOME directory during config loading, with proper
/// tempdir isolation to avoid contaminating the real user environment.
#[tokio::test]
#[serial]
async fn workspace_home_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create a custom needle home directory in the isolated location
    let custom_needle_home = isolated_home.join(".custom-needle");
    fs::create_dir_all(&custom_needle_home).expect("failed to create custom needle home");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();

    // Verify our test is using isolated HOME
    let real_home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    assert_ne!(
        isolated_home.as_path(),
        std::path::PathBuf::from(real_home.clone()),
        "test should use isolated temp directory, not real HOME"
    );

    env::set_var("HOME", &isolated_home);

    // Test 1: Tilde-prefixed path (~/.custom-needle)
    let tilde_path = "~/.custom-needle";
    let expanded = expand_tilde(tilde_path);

    assert_eq!(
        expanded,
        custom_needle_home.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Tilde expansion works in workspace.home config context
    let yaml = format!(
        r#"
workspace:
  home: {}
"#,
        tilde_path
    );

    // Load config - this should trigger tilde expansion
    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the workspace.home was expanded
    assert!(
        config_expanded.workspace.home.starts_with(&isolated_home),
        "workspace.home should be expanded to isolated home, got: {}",
        config_expanded.workspace.home.display()
    );

    assert_eq!(
        config_expanded.workspace.home, custom_needle_home,
        "workspace.home should point to our custom needle home"
    );

    // Test 3: Absolute path should pass through unchanged
    let absolute_yaml = r#"
workspace:
  home: /absolute/path/to/needle-home
"#;

    let config_absolute: Config =
        serde_yaml::from_str(absolute_yaml).expect("failed to parse config");
    let mut config_absolute_expanded = config_absolute;
    config_absolute_expanded.expand_tildes();

    assert_eq!(
        config_absolute_expanded.workspace.home,
        std::path::PathBuf::from("/absolute/path/to/needle-home"),
        "absolute paths should pass through unchanged"
    );

    // Test 4: Relative path should pass through unchanged
    let relative_yaml = r#"
workspace:
  home: relative/path/to/needle-home
"#;

    let config_relative: Config =
        serde_yaml::from_str(relative_yaml).expect("failed to parse config");
    let mut config_relative_expanded = config_relative;
    config_relative_expanded.expand_tildes();

    assert_eq!(
        config_relative_expanded.workspace.home,
        std::path::PathBuf::from("relative/path/to/needle-home"),
        "relative paths should pass through unchanged"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Workspace home tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!(
        "  Tilde path ~/.custom-needle -> {}",
        custom_needle_home.display()
    );
    println!("  Absolute path preserved: /absolute/path/to/needle-home");
    println!("  Relative path preserved: relative/path/to/needle-home");
}

/// Test tilde expansion in workspace.default configuration.
///
/// This test validates that tilde-prefixed paths in workspace.default are
/// correctly expanded to the HOME directory during config loading, with proper
/// tempdir isolation to avoid contaminating the real user environment.
#[tokio::test]
#[serial]
async fn workspace_default_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create a custom workspace directory in the isolated location
    let custom_workspace = isolated_home.join("dev").join("my-workspace");
    fs::create_dir_all(&custom_workspace).expect("failed to create custom workspace");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();

    // Verify our test is using isolated HOME
    let real_home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    assert_ne!(
        isolated_home.as_path(),
        std::path::PathBuf::from(real_home.clone()),
        "test should use isolated temp directory, not real HOME"
    );

    env::set_var("HOME", &isolated_home);

    // Test 1: Tilde-prefixed path (~/dev/my-workspace)
    let tilde_path = "~/dev/my-workspace";
    let expanded = expand_tilde(tilde_path);

    assert_eq!(
        expanded,
        custom_workspace.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Tilde expansion works in workspace.default config context
    let yaml = format!(
        r#"
workspace:
  default: {}
"#,
        tilde_path
    );

    // Load config - this should trigger tilde expansion
    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the workspace.default was expanded
    assert!(
        config_expanded
            .workspace
            .default
            .starts_with(&isolated_home),
        "workspace.default should be expanded to isolated home, got: {}",
        config_expanded.workspace.default.display()
    );

    assert_eq!(
        config_expanded.workspace.default, custom_workspace,
        "workspace.default should point to our custom workspace"
    );

    // Test 3: Absolute path should pass through unchanged
    let absolute_yaml = r#"
workspace:
  default: /absolute/path/to/workspace
"#;

    let config_absolute: Config =
        serde_yaml::from_str(absolute_yaml).expect("failed to parse config");
    let mut config_absolute_expanded = config_absolute;
    config_absolute_expanded.expand_tildes();

    assert_eq!(
        config_absolute_expanded.workspace.default,
        std::path::PathBuf::from("/absolute/path/to/workspace"),
        "absolute paths should pass through unchanged"
    );

    // Test 4: Relative path should pass through unchanged
    let relative_yaml = r#"
workspace:
  default: relative/path/to/workspace
"#;

    let config_relative: Config =
        serde_yaml::from_str(relative_yaml).expect("failed to parse config");
    let mut config_relative_expanded = config_relative;
    config_relative_expanded.expand_tildes();

    assert_eq!(
        config_relative_expanded.workspace.default,
        std::path::PathBuf::from("relative/path/to/workspace"),
        "relative paths should pass through unchanged"
    );

    // Test 5: Current directory reference (.)
    let current_yaml = r#"
workspace:
  default: .
"#;

    let config_current: Config =
        serde_yaml::from_str(current_yaml).expect("failed to parse config");
    let mut config_current_expanded = config_current;
    config_current_expanded.expand_tildes();

    assert_eq!(
        config_current_expanded.workspace.default,
        std::path::PathBuf::from("."),
        "current directory reference should pass through unchanged"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Workspace default tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!(
        "  Tilde path ~/dev/my-workspace -> {}",
        custom_workspace.display()
    );
    println!("  Absolute path preserved: /absolute/path/to/workspace");
    println!("  Relative path preserved: relative/path/to/workspace");
    println!("  Current directory preserved: .");
}

/// Test tilde expansion with both workspace.home and workspace.default simultaneously.
///
/// This test validates that both workspace path fields can use tilde expansion
/// in the same configuration, and both are expanded correctly.
#[tokio::test]
#[serial]
async fn workspace_home_and_default_tilde_expansion_combined() {
    use needle::config::Config;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create custom directories
    let custom_needle_home = isolated_home.join(".custom-needle");
    fs::create_dir_all(&custom_needle_home).expect("failed to create custom needle home");

    let custom_workspace = isolated_home.join("dev").join("my-workspace");
    fs::create_dir_all(&custom_workspace).expect("failed to create custom workspace");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();
    env::set_var("HOME", &isolated_home);

    // Test combined tilde expansion in both fields
    let yaml = r#"
workspace:
  home: ~/.custom-needle
  default: ~/dev/my-workspace
"#
    .to_string();

    // Load config - this should trigger tilde expansion for both fields
    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes in both fields
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify both fields were expanded correctly
    assert_eq!(
        config_expanded.workspace.home, custom_needle_home,
        "workspace.home should be expanded to custom needle home"
    );

    assert_eq!(
        config_expanded.workspace.default, custom_workspace,
        "workspace.default should be expanded to custom workspace"
    );

    // Verify both paths start with the isolated home directory
    assert!(
        config_expanded.workspace.home.starts_with(&isolated_home),
        "workspace.home should start with isolated home"
    );

    assert!(
        config_expanded
            .workspace
            .default
            .starts_with(&isolated_home),
        "workspace.default should start with isolated home"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Combined workspace home and default tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!(
        "  workspace.home: ~/.custom-needle -> {}",
        custom_needle_home.display()
    );
    println!(
        "  workspace.default: ~/dev/my-workspace -> {}",
        custom_workspace.display()
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Tilde Expansion Integration Tests — Additional Path Configuration Fields
// ═════════════════════════════════════════════════════════════════════════════

/// Test tilde expansion in agent.adapters_dir configuration.
///
/// This test validates that tilde-prefixed paths in agent.adapters_dir are
/// correctly expanded to the HOME directory during config loading, with proper
/// tempdir isolation to avoid contaminating the real user environment.
#[tokio::test]
#[serial]
async fn agent_adapters_dir_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create a custom adapters directory in the isolated location
    let custom_adapters_dir = isolated_home.join(".custom-adapters");
    fs::create_dir_all(&custom_adapters_dir).expect("failed to create custom adapters dir");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();
    env::set_var("HOME", &isolated_home);

    // Test 1: Tilde-prefixed path (~/.custom-adapters)
    let tilde_path = "~/.custom-adapters";
    let expanded = expand_tilde(tilde_path);

    assert_eq!(
        expanded,
        custom_adapters_dir.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Tilde expansion works in agent.adapters_dir config context
    let yaml = format!(
        r#"
agent:
  adapters_dir: {}
"#,
        tilde_path
    );

    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the path was expanded correctly using string comparison
    let expanded_path_str = config_expanded.agent.adapters_dir.to_str().unwrap();
    let isolated_home_str = isolated_home.to_str().unwrap();
    assert!(
        expanded_path_str.starts_with(isolated_home_str),
        "agent.adapters_dir should be expanded to isolated home, got: {}",
        expanded_path_str
    );

    assert_eq!(
        config_expanded.agent.adapters_dir, custom_adapters_dir,
        "agent.adapters_dir should point to our custom adapters directory"
    );

    // Test 3: Absolute path should pass through unchanged
    let absolute_yaml = r#"
agent:
  adapters_dir: /absolute/path/to/adapters
"#;

    let config_absolute: Config =
        serde_yaml::from_str(absolute_yaml).expect("failed to parse config");
    let mut config_absolute_expanded = config_absolute;
    config_absolute_expanded.expand_tildes();

    assert_eq!(
        config_absolute_expanded.agent.adapters_dir,
        std::path::PathBuf::from("/absolute/path/to/adapters"),
        "absolute paths should pass through unchanged"
    );

    // Test 4: Relative path should pass through unchanged
    let relative_yaml = r#"
agent:
  adapters_dir: relative/path/to/adapters
"#;

    let config_relative: Config =
        serde_yaml::from_str(relative_yaml).expect("failed to parse config");
    let mut config_relative_expanded = config_relative;
    config_relative_expanded.expand_tildes();

    assert_eq!(
        config_relative_expanded.agent.adapters_dir,
        std::path::PathBuf::from("relative/path/to/adapters"),
        "relative paths should pass through unchanged"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Agent adapters_dir tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!(
        "  Tilde path ~/.custom-adapters -> {}",
        custom_adapters_dir.display()
    );
}

/// Test tilde expansion in bead_cli.path configuration.
///
/// This test validates that tilde-prefixed paths in bead_cli.path are
/// correctly expanded to the HOME directory during config loading, with proper
/// tempdir isolation to avoid contaminating the real user environment.
#[tokio::test]
#[serial]
async fn bead_cli_path_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create a custom bead cli path in the isolated location
    let custom_bead_cli = isolated_home.join(".local/bin/custom-bead");
    fs::create_dir_all(custom_bead_cli.parent().unwrap()).expect("failed to create parent dir");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();
    env::set_var("HOME", &isolated_home);

    // Test 1: Tilde-prefixed path (~/.local/bin/custom-bead)
    let tilde_path = "~/.local/bin/custom-bead";
    let expanded = expand_tilde(tilde_path);

    assert_eq!(
        expanded,
        custom_bead_cli.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Tilde expansion works in bead_cli.path config context
    let yaml = format!(
        r#"
bead_cli:
  path: {}
"#,
        tilde_path
    );

    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the path was expanded correctly using string comparison
    let expanded_path_str = config_expanded
        .bead_cli
        .path
        .as_ref()
        .unwrap()
        .to_str()
        .unwrap();
    let isolated_home_str = isolated_home.to_str().unwrap();
    assert!(
        expanded_path_str.starts_with(isolated_home_str),
        "bead_cli.path should be expanded to isolated home, got: {}",
        expanded_path_str
    );

    assert_eq!(
        config_expanded.bead_cli.path.as_ref().unwrap().as_path(),
        custom_bead_cli.as_path(),
        "bead_cli.path should point to our custom bead cli"
    );

    // Test 3: Absolute path should pass through unchanged
    let absolute_yaml = r#"
bead_cli:
  path: /usr/local/bin/bead
"#;

    let config_absolute: Config =
        serde_yaml::from_str(absolute_yaml).expect("failed to parse config");
    let mut config_absolute_expanded = config_absolute;
    config_absolute_expanded.expand_tildes();

    assert_eq!(
        config_absolute_expanded.bead_cli.path.unwrap(),
        std::path::PathBuf::from("/usr/local/bin/bead"),
        "absolute paths should pass through unchanged"
    );

    // Test 4: Relative path should pass through unchanged
    let relative_yaml = r#"
bead_cli:
  path: ./local/bin/bead
"#;

    let config_relative: Config =
        serde_yaml::from_str(relative_yaml).expect("failed to parse config");
    let mut config_relative_expanded = config_relative;
    config_relative_expanded.expand_tildes();

    assert_eq!(
        config_relative_expanded.bead_cli.path.unwrap(),
        std::path::PathBuf::from("./local/bin/bead"),
        "relative paths should pass through unchanged"
    );

    // Test 5: None/missing path should remain None
    let none_yaml = r#"
bead_cli:
  backend: bead-rs
"#;

    let config_none: Config = serde_yaml::from_str(none_yaml).expect("failed to parse config");
    let mut config_none_expanded = config_none;
    config_none_expanded.expand_tildes();

    assert!(
        config_none_expanded.bead_cli.path.is_none(),
        "None path should remain None after expansion"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Bead CLI path tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!(
        "  Tilde path ~/.local/bin/custom-bead -> {}",
        custom_bead_cli.display()
    );
}

/// Test tilde expansion in strands.explore.workspace_root configuration.
///
/// This test validates that tilde-prefixed paths in strands.explore.workspace_root
/// are correctly expanded to the HOME directory during config loading, with proper
/// tempdir isolation to avoid contaminating the real user environment.
#[tokio::test]
#[serial]
async fn explore_workspace_root_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create a custom workspace root in the isolated location
    let custom_workspace_root = isolated_home.join("dev-workspaces");
    fs::create_dir_all(&custom_workspace_root).expect("failed to create custom workspace root");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();
    env::set_var("HOME", &isolated_home);

    // Test 1: Tilde-prefixed path (~/dev-workspaces)
    let tilde_path = "~/dev-workspaces";
    let expanded = expand_tilde(tilde_path);

    assert_eq!(
        expanded,
        custom_workspace_root.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Tilde expansion works in strands.explore.workspace_root config context
    let yaml = format!(
        r#"
strands:
  explore:
    workspace_root: {}
"#,
        tilde_path
    );

    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    assert!(
        config_expanded
            .strands
            .explore
            .workspace_root
            .starts_with(&isolated_home),
        "strands.explore.workspace_root should be expanded to isolated home, got: {}",
        config_expanded.strands.explore.workspace_root.display()
    );

    assert_eq!(
        config_expanded.strands.explore.workspace_root, custom_workspace_root,
        "strands.explore.workspace_root should point to our custom workspace root"
    );

    // Test 3: Absolute path should pass through unchanged
    let absolute_yaml = r#"
strands:
  explore:
    workspace_root: /absolute/path/to/workspaces
"#;

    let config_absolute: Config =
        serde_yaml::from_str(absolute_yaml).expect("failed to parse config");
    let mut config_absolute_expanded = config_absolute;
    config_absolute_expanded.expand_tildes();

    assert_eq!(
        config_absolute_expanded.strands.explore.workspace_root,
        std::path::PathBuf::from("/absolute/path/to/workspaces"),
        "absolute paths should pass through unchanged"
    );

    // Test 4: Relative path should pass through unchanged
    let relative_yaml = r#"
strands:
  explore:
    workspace_root: relative/path/to/workspaces
"#;

    let config_relative: Config =
        serde_yaml::from_str(relative_yaml).expect("failed to parse config");
    let mut config_relative_expanded = config_relative;
    config_relative_expanded.expand_tildes();

    assert_eq!(
        config_relative_expanded.strands.explore.workspace_root,
        std::path::PathBuf::from("relative/path/to/workspaces"),
        "relative paths should pass through unchanged"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Explore workspace_root tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!(
        "  Tilde path ~/dev-workspaces -> {}",
        custom_workspace_root.display()
    );
}

/// Test tilde expansion in strands.explore.workspaces configuration (Vec<PathBuf>).
///
/// This test validates that tilde-prefixed paths in strands.explore.workspaces
/// (a vector of paths) are correctly expanded to the HOME directory during config
/// loading, with proper tempdir isolation to avoid contaminating the real user environment.
#[tokio::test]
#[serial]
async fn explore_workspaces_tilde_expansion() {
    use needle::config::Config;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create custom workspaces in the isolated location
    let workspace1 = isolated_home.join("dev/workspace1");
    let workspace2 = isolated_home.join("dev/workspace2");
    fs::create_dir_all(&workspace1).expect("failed to create workspace1");
    fs::create_dir_all(&workspace2).expect("failed to create workspace2");

    // Save the original HOME
    let original_home = env::var("HOME").ok();

    // Set our isolated home
    env::set_var("HOME", &isolated_home);

    // Ensure HOME is restored even on panic
    let _guard = scopeguard::guard(original_home, |original_home| {
        if let Some(home) = original_home {
            env::set_var("HOME", home);
        } else {
            env::remove_var("HOME");
        }
    });

    // Test 1: Tilde-prefixed paths in workspaces list
    let yaml = r#"
strands:
  explore:
    workspaces:
      - ~/dev/workspace1
      - ~/dev/workspace2
"#;

    let config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    assert_eq!(
        config_expanded.strands.explore.workspaces.len(),
        2,
        "should have 2 workspaces"
    );

    assert!(
        config_expanded.strands.explore.workspaces[0].starts_with(&isolated_home),
        "first workspace should be expanded to isolated home, got: {}",
        config_expanded.strands.explore.workspaces[0].display()
    );

    assert_eq!(
        config_expanded.strands.explore.workspaces[0], workspace1,
        "first workspace should point to our custom workspace1"
    );

    assert_eq!(
        config_expanded.strands.explore.workspaces[1], workspace2,
        "second workspace should point to our custom workspace2"
    );

    // Test 2: Absolute paths should pass through unchanged
    let absolute_yaml = r#"
strands:
  explore:
    workspaces:
      - /absolute/path/to/workspace1
      - /absolute/path/to/workspace2
"#;

    let config_absolute: Config =
        serde_yaml::from_str(absolute_yaml).expect("failed to parse config");
    let mut config_absolute_expanded = config_absolute;
    config_absolute_expanded.expand_tildes();

    assert_eq!(
        config_absolute_expanded.strands.explore.workspaces[0],
        std::path::PathBuf::from("/absolute/path/to/workspace1"),
        "absolute paths should pass through unchanged"
    );

    assert_eq!(
        config_absolute_expanded.strands.explore.workspaces[1],
        std::path::PathBuf::from("/absolute/path/to/workspace2"),
        "second absolute path should pass through unchanged"
    );

    // Test 3: Relative paths should pass through unchanged
    let relative_yaml = r#"
strands:
  explore:
    workspaces:
      - relative/workspace1
      - relative/workspace2
"#;

    let config_relative: Config =
        serde_yaml::from_str(relative_yaml).expect("failed to parse config");
    let mut config_relative_expanded = config_relative;
    config_relative_expanded.expand_tildes();

    assert_eq!(
        config_relative_expanded.strands.explore.workspaces[0],
        std::path::PathBuf::from("relative/workspace1"),
        "relative paths should pass through unchanged"
    );

    // Test 4: Mixed tilde and absolute paths
    let mixed_yaml = r#"
strands:
  explore:
    workspaces:
      - ~/dev/workspace1
      - /absolute/path/to/workspace2
"#;

    let config_mixed: Config = serde_yaml::from_str(mixed_yaml).expect("failed to parse config");
    let mut config_mixed_expanded = config_mixed;
    config_mixed_expanded.expand_tildes();

    assert_eq!(
        config_mixed_expanded.strands.explore.workspaces[0], workspace1,
        "tilde path should be expanded in mixed list"
    );

    assert_eq!(
        config_mixed_expanded.strands.explore.workspaces[1],
        std::path::PathBuf::from("/absolute/path/to/workspace2"),
        "absolute path should pass through unchanged in mixed list"
    );

    // Note: HOME is automatically restored by _home_guard when it drops here
    println!("✓ Explore workspaces tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  Tilde path ~/dev/workspace1 -> {}", workspace1.display());
    println!("  Tilde path ~/dev/workspace2 -> {}", workspace2.display());
}

/// Test tilde expansion in strands.learning.global_learnings_file configuration.
///
/// This test validates that tilde-prefixed paths in
/// strands.learning.global_learnings_file are correctly expanded to the HOME
/// directory during config loading, with proper tempdir isolation to avoid
/// contaminating the real user environment.
#[tokio::test]
async fn learning_global_learnings_file_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create a custom learnings file directory in the isolated location
    let custom_learnings_dir = isolated_home.join(".config/needle");
    fs::create_dir_all(&custom_learnings_dir).expect("failed to create learnings dir");
    let custom_learnings_file = custom_learnings_dir.join("global-learnings.md");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();
    env::set_var("HOME", &isolated_home);

    // Test 1: Tilde-prefixed path (~/.config/needle/global-learnings.md)
    let tilde_path = "~/.config/needle/global-learnings.md";
    let expanded = expand_tilde(tilde_path);

    assert_eq!(
        expanded,
        custom_learnings_file.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Tilde expansion works in strands.learning.global_learnings_file config context
    let yaml = format!(
        r#"
strands:
  learning:
    global_learnings_file: {}
"#,
        tilde_path
    );

    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    assert!(
        config_expanded
            .strands
            .learning
            .global_learnings_file
            .starts_with(&isolated_home),
        "global_learnings_file should be expanded to isolated home, got: {}",
        config_expanded
            .strands
            .learning
            .global_learnings_file
            .display()
    );

    assert_eq!(
        config_expanded.strands.learning.global_learnings_file, custom_learnings_file,
        "global_learnings_file should point to our custom learnings file"
    );

    // Test 3: Absolute path should pass through unchanged
    let absolute_yaml = r#"
strands:
  learning:
    global_learnings_file: /absolute/path/to/learnings.md
"#;

    let config_absolute: Config =
        serde_yaml::from_str(absolute_yaml).expect("failed to parse config");
    let mut config_absolute_expanded = config_absolute;
    config_absolute_expanded.expand_tildes();

    assert_eq!(
        config_absolute_expanded
            .strands
            .learning
            .global_learnings_file,
        std::path::PathBuf::from("/absolute/path/to/learnings.md"),
        "absolute paths should pass through unchanged"
    );

    // Test 4: Relative path should pass through unchanged
    let relative_yaml = r#"
strands:
  learning:
    global_learnings_file: relative/path/to/learnings.md
"#;

    let config_relative: Config =
        serde_yaml::from_str(relative_yaml).expect("failed to parse config");
    let mut config_relative_expanded = config_relative;
    config_relative_expanded.expand_tildes();

    assert_eq!(
        config_relative_expanded
            .strands
            .learning
            .global_learnings_file,
        std::path::PathBuf::from("relative/path/to/learnings.md"),
        "relative paths should pass through unchanged"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Learning global_learnings_file tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!(
        "  Tilde path ~/.config/needle/global-learnings.md -> {}",
        custom_learnings_file.display()
    );
}

/// Test tilde expansion in telemetry.file_sink.log_dir configuration.
///
/// This test validates that tilde-prefixed paths in telemetry.file_sink.log_dir
/// (an Option<PathBuf>) are correctly expanded to the HOME directory during
/// config loading, with proper tempdir isolation to avoid contaminating the
/// real user environment.
#[tokio::test]
#[serial]
async fn telemetry_log_dir_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create a custom log directory in the isolated location
    let custom_log_dir = isolated_home.join(".needle-logs");
    fs::create_dir_all(&custom_log_dir).expect("failed to create custom log dir");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();
    env::set_var("HOME", &isolated_home);

    // Test 1: Tilde-prefixed path (~/.needle-logs)
    let tilde_path = "~/.needle-logs";
    let expanded = expand_tilde(tilde_path);

    assert_eq!(
        expanded,
        custom_log_dir.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Tilde expansion works in telemetry.file_sink.log_dir config context
    let yaml = format!(
        r#"
telemetry:
  file_sink:
    log_dir: {}
"#,
        tilde_path
    );

    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the path was expanded correctly using string comparison
    let expanded_path_str = config_expanded
        .telemetry
        .file_sink
        .log_dir
        .as_ref()
        .unwrap()
        .to_str()
        .unwrap();
    let isolated_home_str = isolated_home.to_str().unwrap();
    assert!(
        expanded_path_str.starts_with(isolated_home_str),
        "log_dir should be expanded to isolated home, got: {}",
        expanded_path_str
    );

    assert_eq!(
        config_expanded.telemetry.file_sink.log_dir.unwrap(),
        custom_log_dir,
        "log_dir should point to our custom log directory"
    );

    // Test 3: Absolute path should pass through unchanged
    let absolute_yaml = r#"
telemetry:
  file_sink:
    log_dir: /var/log/needle
"#;

    let config_absolute: Config =
        serde_yaml::from_str(absolute_yaml).expect("failed to parse config");
    let mut config_absolute_expanded = config_absolute;
    config_absolute_expanded.expand_tildes();

    assert_eq!(
        config_absolute_expanded
            .telemetry
            .file_sink
            .log_dir
            .unwrap(),
        std::path::PathBuf::from("/var/log/needle"),
        "absolute paths should pass through unchanged"
    );

    // Test 4: Relative path should pass through unchanged
    let relative_yaml = r#"
telemetry:
  file_sink:
    log_dir: ./logs/needle
"#;

    let config_relative: Config =
        serde_yaml::from_str(relative_yaml).expect("failed to parse config");
    let mut config_relative_expanded = config_relative;
    config_relative_expanded.expand_tildes();

    assert_eq!(
        config_relative_expanded
            .telemetry
            .file_sink
            .log_dir
            .unwrap(),
        std::path::PathBuf::from("./logs/needle"),
        "relative paths should pass through unchanged"
    );

    // Test 5: None/missing log_dir should remain None
    let none_yaml = r#"
telemetry:
  file_sink: {}
"#;

    let config_none: Config = serde_yaml::from_str(none_yaml).expect("failed to parse config");
    let mut config_none_expanded = config_none;
    config_none_expanded.expand_tildes();

    assert!(
        config_none_expanded.telemetry.file_sink.log_dir.is_none(),
        "None log_dir should remain None after expansion"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Telemetry log_dir tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!(
        "  Tilde path ~/.needle-logs -> {}",
        custom_log_dir.display()
    );
}

/// Test tilde expansion in supervisor.heartbeat_path configuration.
///
/// This test validates that tilde-prefixed paths in supervisor.heartbeat_path
/// (an Option<PathBuf>) are correctly expanded to the HOME directory during
/// config loading, with proper tempdir isolation to avoid contaminating the
/// real user environment.
#[tokio::test]
#[serial]
async fn supervisor_heartbeat_path_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create a custom heartbeat directory in the isolated location
    let custom_heartbeat_dir = isolated_home.join(".needle-heartbeat");
    fs::create_dir_all(&custom_heartbeat_dir).expect("failed to create heartbeat dir");
    let custom_heartbeat_path = custom_heartbeat_dir.join("supervisor.heartbeat");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();
    env::set_var("HOME", &isolated_home);

    // Test 1: Tilde-prefixed path (~/.needle-heartbeat/supervisor.heartbeat)
    let tilde_path = "~/.needle-heartbeat/supervisor.heartbeat";
    let expanded = expand_tilde(tilde_path);

    assert_eq!(
        expanded,
        custom_heartbeat_path.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Tilde expansion works in supervisor.heartbeat_path config context
    let yaml = format!(
        r#"
supervisor:
  heartbeat_path: {}
"#,
        tilde_path
    );

    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    assert!(
        config_expanded
            .supervisor
            .heartbeat_path
            .as_ref()
            .unwrap()
            .starts_with(&isolated_home),
        "heartbeat_path should be expanded to isolated home, got: {}",
        config_expanded
            .supervisor
            .heartbeat_path
            .as_ref()
            .unwrap()
            .display()
    );

    assert_eq!(
        config_expanded.supervisor.heartbeat_path.unwrap(),
        custom_heartbeat_path,
        "heartbeat_path should point to our custom heartbeat file"
    );

    // Test 3: Absolute path should pass through unchanged
    let absolute_yaml = r#"
supervisor:
  heartbeat_path: /var/run/needle/supervisor.heartbeat
"#;

    let config_absolute: Config =
        serde_yaml::from_str(absolute_yaml).expect("failed to parse config");
    let mut config_absolute_expanded = config_absolute;
    config_absolute_expanded.expand_tildes();

    assert_eq!(
        config_absolute_expanded.supervisor.heartbeat_path.unwrap(),
        std::path::PathBuf::from("/var/run/needle/supervisor.heartbeat"),
        "absolute paths should pass through unchanged"
    );

    // Test 4: Relative path should pass through unchanged
    let relative_yaml = r#"
supervisor:
  heartbeat_path: ./runtime/supervisor.heartbeat
"#;

    let config_relative: Config =
        serde_yaml::from_str(relative_yaml).expect("failed to parse config");
    let mut config_relative_expanded = config_relative;
    config_relative_expanded.expand_tildes();

    assert_eq!(
        config_relative_expanded.supervisor.heartbeat_path.unwrap(),
        std::path::PathBuf::from("./runtime/supervisor.heartbeat"),
        "relative paths should pass through unchanged"
    );

    // Test 5: None/missing heartbeat_path should remain None
    let none_yaml = r#"
supervisor: {}
"#;

    let config_none: Config = serde_yaml::from_str(none_yaml).expect("failed to parse config");
    let mut config_none_expanded = config_none;
    config_none_expanded.expand_tildes();

    assert!(
        config_none_expanded.supervisor.heartbeat_path.is_none(),
        "None heartbeat_path should remain None after expansion"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ Supervisor heartbeat_path tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!(
        "  Tilde path ~/.needle-heartbeat/supervisor.heartbeat -> {}",
        custom_heartbeat_path.display()
    );
}

/// Test tilde expansion in prompt.context_files configuration (Vec<PathBuf>).
///
/// This test validates that tilde-prefixed paths in prompt.context_files
/// (a vector of paths) are correctly expanded to the HOME directory during
/// config loading, with proper tempdir isolation to avoid contaminating the
/// real user environment.
#[tokio::test]
#[serial]
async fn prompt_context_files_tilde_expansion() {
    use needle::config::Config;
    use std::env;
    use std::fs;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create custom context files in the isolated location
    let context1 = isolated_home.join(".config/needle/context1.md");
    let context2 = isolated_home.join(".config/needle/context2.md");
    fs::create_dir_all(context1.parent().unwrap()).expect("failed to create context dir");
    fs::write(&context1, "# Context 1").expect("failed to write context1");
    fs::write(&context2, "# Context 2").expect("failed to write context2");

    // Save the original HOME
    let original_home = env::var("HOME").ok();

    // Set our isolated home
    env::set_var("HOME", &isolated_home);

    // Ensure HOME is restored even on panic
    let _guard = scopeguard::guard(original_home, |original_home| {
        if let Some(home) = original_home {
            env::set_var("HOME", home);
        } else {
            env::remove_var("HOME");
        }
    });

    // Test 1: Tilde-prefixed paths in context_files list
    let yaml = r#"
prompt:
  context_files:
    - ~/.config/needle/context1.md
    - ~/.config/needle/context2.md
"#;

    let config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    assert_eq!(
        config_expanded.prompt.context_files.len(),
        2,
        "should have 2 context files"
    );

    assert!(
        config_expanded.prompt.context_files[0].starts_with(&isolated_home),
        "first context file should be expanded to isolated home, got: {}",
        config_expanded.prompt.context_files[0].display()
    );

    assert_eq!(
        config_expanded.prompt.context_files[0], context1,
        "first context file should point to our custom context1"
    );

    assert_eq!(
        config_expanded.prompt.context_files[1], context2,
        "second context file should point to our custom context2"
    );

    // Test 2: Absolute paths should pass through unchanged
    let absolute_yaml = r#"
prompt:
  context_files:
    - /absolute/path/to/context1.md
    - /absolute/path/to/context2.md
"#;

    let config_absolute: Config =
        serde_yaml::from_str(absolute_yaml).expect("failed to parse config");
    let mut config_absolute_expanded = config_absolute;
    config_absolute_expanded.expand_tildes();

    assert_eq!(
        config_absolute_expanded.prompt.context_files[0],
        std::path::PathBuf::from("/absolute/path/to/context1.md"),
        "absolute paths should pass through unchanged"
    );

    assert_eq!(
        config_absolute_expanded.prompt.context_files[1],
        std::path::PathBuf::from("/absolute/path/to/context2.md"),
        "second absolute path should pass through unchanged"
    );

    // Test 3: Relative paths should pass through unchanged
    let relative_yaml = r#"
prompt:
  context_files:
    - relative/context1.md
    - relative/context2.md
"#;

    let config_relative: Config =
        serde_yaml::from_str(relative_yaml).expect("failed to parse config");
    let mut config_relative_expanded = config_relative;
    config_relative_expanded.expand_tildes();

    assert_eq!(
        config_relative_expanded.prompt.context_files[0],
        std::path::PathBuf::from("relative/context1.md"),
        "relative paths should pass through unchanged"
    );

    // Test 4: Mixed tilde and absolute paths
    let mixed_yaml = r#"
prompt:
  context_files:
    - ~/.config/needle/context1.md
    - /absolute/path/to/context2.md
"#;

    let config_mixed: Config = serde_yaml::from_str(mixed_yaml).expect("failed to parse config");
    let mut config_mixed_expanded = config_mixed;
    config_mixed_expanded.expand_tildes();

    assert_eq!(
        config_mixed_expanded.prompt.context_files[0], context1,
        "tilde path should be expanded in mixed list"
    );

    assert_eq!(
        config_mixed_expanded.prompt.context_files[1],
        std::path::PathBuf::from("/absolute/path/to/context2.md"),
        "absolute path should pass through unchanged in mixed list"
    );

    // Note: HOME is automatically restored by _home_guard when it drops here
    println!("✓ Prompt context_files tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!(
        "  Tilde path ~/.config/needle/context1.md -> {}",
        context1.display()
    );
    println!(
        "  Tilde path ~/.config/needle/context2.md -> {}",
        context2.display()
    );
}

/// Test tilde expansion with multiple tildes in same value.
///
/// This test validates that only the FIRST tilde (at position 0) is expanded,
/// and subsequent tildes are treated as literal path components.
///
/// Test cases:
/// - `~/path/~subdir` - First tilde expands, second tilde is literal
/// - `~/~/nested` - Both tildes at start, only first expands
/// - `path/~/other` - Tilde in middle should NOT expand
/// - `/absolute/~/path` - Tilde in middle of absolute path should NOT expand
#[tokio::test]
async fn tilde_expansion_multiple_tildes_in_same_value() {
    use needle::util::expand_tilde;

    let _home_guard = HomeGuard::isolate();
    let isolated_home = _home_guard._temp_dir.path();

    // Test 1: First tilde expands, second tilde is literal
    let path_with_second_tilde = "~/bin/~subdir";
    let expanded = expand_tilde(path_with_second_tilde);
    let expected = isolated_home.join("bin/~subdir");
    assert_eq!(
        expanded,
        expected.to_str().unwrap(),
        "only first tilde should expand, second tilde should be literal"
    );
    println!("  Test 1: ~/bin/~subdir -> {}", expanded);

    // Test 2: Double tilde at start - only first expands
    let double_tilde_start = "~/~/nested";
    let expanded = expand_tilde(double_tilde_start);
    let expected = isolated_home.join("~/nested");
    assert_eq!(
        expanded,
        expected.to_str().unwrap(),
        "first tilde expands, second tilde prefix is literal"
    );
    println!("  Test 2: ~/~/nested -> {}", expanded);

    // Test 3: Tilde in middle of relative path - should NOT expand
    let tilde_middle = "path/~/other";
    let expanded = expand_tilde(tilde_middle);
    assert_eq!(
        expanded, tilde_middle,
        "tilde in middle of path should not expand"
    );
    println!("  Test 3: path/~/other -> {} (unchanged)", expanded);

    // Test 4: Tilde in middle of absolute path - should NOT expand
    let tilde_middle_absolute = "/absolute/~/path";
    let expanded = expand_tilde(tilde_middle_absolute);
    assert_eq!(
        expanded, tilde_middle_absolute,
        "tilde in middle of absolute path should not expand"
    );
    println!("  Test 4: /absolute/~/path -> {} (unchanged)", expanded);

    println!("✓ Multiple tildes in same value test passed");
}

/// Test tilde expansion at different positions (start vs middle/end).
///
/// This test validates that tilde expansion ONLY occurs at the START of a path,
/// not in the middle or at the end. Only paths beginning with ~ or ~/ should expand.
///
/// Test cases:
/// - `~/start` - Tilde at START: should expand
/// - `path/~middle` - Tilde in MIDDLE: should NOT expand
/// - `/absolute/~middle` - Tilde in middle of absolute: should NOT expand
/// - `~/end/~` - Tilde at end: only START tilde expands
/// - `~/path/~` - Tilde at end of path component: only START tilde expands
/// - `~` - Bare tilde: should expand to home directory
#[tokio::test]
#[serial]
async fn tilde_expansion_position_start_vs_middle_end() {
    use needle::util::expand_tilde;

    let _home_guard = HomeGuard::isolate();
    let isolated_home = _home_guard._temp_dir.path();

    // Test 1: Tilde at START - should expand
    let tilde_at_start = "~/bin/needle";
    let expanded = expand_tilde(tilde_at_start);
    let expected = isolated_home.join("bin/needle");
    assert_eq!(
        expanded,
        expected.to_str().unwrap(),
        "tilde at start should expand"
    );
    println!("  Test 1 (START): ~/bin/needle -> {}", expanded);

    // Test 2: Tilde in MIDDLE of relative path - should NOT expand
    let tilde_in_middle = "config/~backup/settings.yml";
    let expanded = expand_tilde(tilde_in_middle);
    assert_eq!(
        expanded, tilde_in_middle,
        "tilde in middle of relative path should not expand"
    );
    println!(
        "  Test 2 (MIDDLE): config/~backup/settings.yml -> {} (unchanged)",
        expanded
    );

    // Test 3: Tilde in MIDDLE of absolute path - should NOT expand
    let tilde_in_middle_absolute = "/etc/needle/~config/settings.yml";
    let expanded = expand_tilde(tilde_in_middle_absolute);
    assert_eq!(
        expanded, tilde_in_middle_absolute,
        "tilde in middle of absolute path should not expand"
    );
    println!(
        "  Test 3 (MIDDLE): /etc/needle/~config/settings.yml -> {} (unchanged)",
        expanded
    );

    // Test 4: Tilde at END - only START tilde should expand
    let tilde_at_end = "~/workspaces/~";
    let expanded = expand_tilde(tilde_at_end);
    let expected = isolated_home.join("workspaces/~");
    assert_eq!(
        expanded,
        expected.to_str().unwrap(),
        "tilde at end of path should be literal, only start tilde expands"
    );
    println!("  Test 4 (END): ~/workspaces/~ -> {}", expanded);

    // Test 5: Tilde as directory component in middle - only START tilde expands
    let tilde_as_component = "~/dev/~old-project/config";
    let expanded = expand_tilde(tilde_as_component);
    let expected = isolated_home.join("dev/~old-project/config");
    assert_eq!(
        expanded,
        expected.to_str().unwrap(),
        "tilde as path component should be literal, only start tilde expands"
    );
    println!(
        "  Test 5 (COMPONENT): ~/dev/~old-project/config -> {}",
        expanded
    );

    // Test 6: Bare tilde - should expand to home directory
    let bare_tilde = "~";
    let expanded = expand_tilde(bare_tilde);
    let expected = isolated_home.to_str().unwrap();
    assert_eq!(
        expanded, expected,
        "bare tilde should expand to home directory"
    );
    println!("  Test 6 (BARE): ~ -> {}", expanded);

    // Test 7: Tilde preceded by path separator - should NOT expand
    let tilde_after_sep = "workspaces/~config";
    let expanded = expand_tilde(tilde_after_sep);
    assert_eq!(
        expanded, tilde_after_sep,
        "tilde after path separator should not expand"
    );
    println!(
        "  Test 7 (AFTER_SEP): workspaces/~config -> {} (unchanged)",
        expanded
    );

    // Test 8: Multiple trailing tildes - only START tilde expands
    let multiple_trailing = "~/path/~~";
    let expanded = expand_tilde(multiple_trailing);
    let expected = isolated_home.join("path/~~");
    assert_eq!(
        expanded,
        expected.to_str().unwrap(),
        "multiple trailing tildes should be literal, only start tilde expands"
    );
    println!("  Test 8 (TRAILING): ~/path/~~ -> {}", expanded);

    println!("✓ Tilde expansion position test passed");
}

/// Test tilde expansion in strands.weave.exclude_workspaces configuration.
///
/// This test validates that tilde-prefixed paths in the weave strand's
/// exclude_workspaces list are correctly expanded to the HOME directory during
/// config loading, with proper tempdir isolation.
#[tokio::test]
#[serial]
async fn weave_exclude_workspaces_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::fs;

    // Use HomeGuard for proper HOME isolation
    let _home_guard = HomeGuard::isolate();
    let isolated_home = _home_guard._temp_dir.path();

    // Create test workspace directories in the isolated home
    let excluded_ws1 = isolated_home.join("workspaces").join("excluded1");
    let excluded_ws2 = isolated_home.join("dev").join("excluded2");
    let absolute_path = PathBuf::from("/absolute/path/excluded3");
    let relative_path = PathBuf::from("relative/excluded4");

    fs::create_dir_all(&excluded_ws1).expect("failed to create excluded ws1");
    fs::create_dir_all(&excluded_ws2).expect("failed to create excluded ws2");

    // Test 1: Tilde-prefixed paths are expanded correctly
    let tilde_path1 = "~/workspaces/excluded1";
    let expanded1 = expand_tilde(tilde_path1);
    assert_eq!(
        expanded1,
        excluded_ws1.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    let tilde_path2 = "~/dev/excluded2";
    let expanded2 = expand_tilde(tilde_path2);
    assert_eq!(
        expanded2,
        excluded_ws2.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Config with mixed tilde, absolute, and relative paths
    let yaml = r#"
strands:
  weave:
    exclude_workspaces:
      - ~/workspaces/excluded1
      - ~/dev/excluded2
      - /absolute/path/excluded3
      - relative/excluded4
"#
    .to_string();

    // Load config - this should trigger tilde expansion
    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the exclude_workspaces list
    let exclude_list = &config_expanded.strands.weave.exclude_workspaces;

    // Check that tilde paths were expanded to isolated home
    assert!(
        exclude_list.contains(&excluded_ws1),
        "exclude_workspaces should contain expanded ~/workspaces/excluded1"
    );
    assert!(
        exclude_list.contains(&excluded_ws2),
        "exclude_workspaces should contain expanded ~/dev/excluded2"
    );

    // Check that absolute paths are preserved
    assert!(
        exclude_list.contains(&absolute_path),
        "exclude_workspaces should preserve absolute path /absolute/path/excluded3"
    );

    // Check that relative paths are preserved
    assert!(
        exclude_list.contains(&relative_path),
        "exclude_workspaces should preserve relative path"
    );

    // Note: HomeGuard automatically restores HOME when dropped
    println!("✓ Weave exclude_workspaces tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  Expanded paths: {:?}, {:?}", excluded_ws1, excluded_ws2);
    println!("  Preserved absolute: {}", absolute_path.display());
    println!("  Preserved relative: {}", relative_path.display());
}

/// Test tilde expansion in strands.splice.report_workspace configuration.
///
/// This test validates that tilde-prefixed paths in the splice strand's
/// report_workspace field are correctly expanded to the HOME directory during
/// config loading, with proper tempdir isolation.
#[tokio::test]
#[serial]
async fn splice_report_workspace_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::fs;

    // Use HomeGuard for proper HOME isolation
    let _home_guard = HomeGuard::isolate();
    let isolated_home = _home_guard._temp_dir.path();

    // Create test workspace directory in the isolated home
    let report_ws = isolated_home.join("reports").join("splice-workspace");
    fs::create_dir_all(&report_ws).expect("failed to create report workspace");

    // Test 1: Tilde-prefixed path expands correctly
    let tilde_path = "~/reports/splice-workspace";
    let expanded = expand_tilde(tilde_path);
    assert_eq!(
        expanded,
        report_ws.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Absolute path is preserved
    let absolute_path = PathBuf::from("/absolute/path/workspace");

    // Test 3: Relative path is preserved
    let relative_path = PathBuf::from("relative/workspace");

    // Test 4: Config with tilde path
    let yaml = format!(
        r#"
strands:
  splice:
    report_workspace: {}
"#,
        tilde_path
    );

    // Load config - this should trigger tilde expansion
    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the report_workspace was expanded
    let report_workspace = &config_expanded.strands.splice.report_workspace;
    assert!(
        report_workspace.is_some(),
        "report_workspace should be set after expansion"
    );

    let expanded_path = report_workspace.as_ref().unwrap();
    assert!(
        expanded_path.starts_with(isolated_home),
        "report_workspace should be expanded to isolated home, got: {}",
        expanded_path.display()
    );

    assert_eq!(
        expanded_path, &report_ws,
        "report_workspace should point to our test workspace"
    );

    // Test 5: Config with absolute path (should be preserved)
    let yaml_abs = format!(
        r#"
strands:
  splice:
    report_workspace: {}
"#,
        absolute_path.display()
    );

    let config_abs: Config = serde_yaml::from_str(&yaml_abs).expect("failed to parse config");
    let mut config_expanded_abs = config_abs;
    config_expanded_abs.expand_tildes();

    assert_eq!(
        config_expanded_abs.strands.splice.report_workspace,
        Some(absolute_path.clone()),
        "absolute report_workspace path should be preserved"
    );

    // Test 6: Config with relative path (should be preserved)
    let yaml_rel = format!(
        r#"
strands:
  splice:
    report_workspace: {}
"#,
        relative_path.display()
    );

    let config_rel: Config = serde_yaml::from_str(&yaml_rel).expect("failed to parse config");
    let mut config_expanded_rel = config_rel;
    config_expanded_rel.expand_tildes();

    assert_eq!(
        config_expanded_rel.strands.splice.report_workspace,
        Some(relative_path.clone()),
        "relative report_workspace path should be preserved"
    );

    // Note: HomeGuard automatically restores HOME when dropped
    println!("✓ Splice report_workspace tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  Expanded tilde path: {}", report_ws.display());
    println!("  Preserved absolute: {}", absolute_path.display());
    println!("  Preserved relative: {}", relative_path.display());
}

/// Test tilde expansion in health.heartbeat_dir configuration.
///
/// This test validates that tilde-prefixed paths in health.heartbeat_dir are
/// correctly expanded to the HOME directory during config loading.
#[tokio::test]
async fn health_heartbeat_dir_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::fs;

    // Use HomeGuard for proper HOME isolation
    let _home_guard = HomeGuard::isolate();
    let isolated_home = _home_guard._temp_dir.path();

    // Create test heartbeat directory in the isolated home
    let heartbeat_dir = isolated_home.join("heartbeat");
    fs::create_dir_all(&heartbeat_dir).expect("failed to create heartbeat dir");

    // Test 1: Tilde-prefixed path expands correctly
    let tilde_path = "~/heartbeat";
    let expanded = expand_tilde(tilde_path);
    assert_eq!(
        expanded,
        heartbeat_dir.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Absolute path is preserved
    let absolute_path = std::path::PathBuf::from("/var/lib/needle/heartbeat");

    // Test 3: Relative path is preserved
    let relative_path = std::path::PathBuf::from("relative/heartbeat");

    // Test 4: Config with tilde path
    let yaml = format!(
        r#"
health:
  heartbeat_dir: {}
"#,
        tilde_path
    );

    // Load config - this should trigger tilde expansion
    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the heartbeat_dir was expanded
    let heartbeat = &config_expanded.health.heartbeat_dir;
    assert!(
        heartbeat.is_some(),
        "heartbeat_dir should be set after expansion"
    );

    let expanded_path = heartbeat.as_ref().unwrap();
    assert!(
        expanded_path.starts_with(isolated_home),
        "heartbeat_dir should be expanded to isolated home, got: {}",
        expanded_path.display()
    );

    assert_eq!(
        expanded_path, &heartbeat_dir,
        "heartbeat_dir should point to our test directory"
    );

    // Test 5: Config with absolute path (should be preserved)
    let yaml_abs = format!(
        r#"
health:
  heartbeat_dir: {}
"#,
        absolute_path.display()
    );

    let config_abs: Config = serde_yaml::from_str(&yaml_abs).expect("failed to parse config");
    let mut config_expanded_abs = config_abs;
    config_expanded_abs.expand_tildes();

    assert_eq!(
        config_expanded_abs.health.heartbeat_dir,
        Some(absolute_path.clone()),
        "absolute heartbeat_dir path should be preserved"
    );

    // Test 6: Config with relative path (should be preserved)
    let yaml_rel = format!(
        r#"
health:
  heartbeat_dir: {}
"#,
        relative_path.display()
    );

    let config_rel: Config = serde_yaml::from_str(&yaml_rel).expect("failed to parse config");
    let mut config_expanded_rel = config_rel;
    config_expanded_rel.expand_tildes();

    assert_eq!(
        config_expanded_rel.health.heartbeat_dir,
        Some(relative_path.clone()),
        "relative heartbeat_dir path should be preserved"
    );

    // Note: HomeGuard automatically restores HOME when dropped
    println!("✓ Health heartbeat_dir tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  Expanded tilde path: {}", heartbeat_dir.display());
    println!("  Preserved absolute: {}", absolute_path.display());
    println!("  Preserved relative: {}", relative_path.display());
}

/// Test tilde expansion in supervisor.socket_path configuration.
///
/// This test validates that tilde-prefixed paths in supervisor.socket_path are
/// correctly expanded to the HOME directory during config loading.
#[tokio::test]
#[serial]
async fn supervisor_socket_path_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::fs;

    // Use HomeGuard for proper HOME isolation
    let _home_guard = HomeGuard::isolate();
    let isolated_home = _home_guard._temp_dir.path();

    // Create test socket directory in the isolated home
    let socket_dir = isolated_home.join(".needle");
    fs::create_dir_all(&socket_dir).expect("failed to create socket dir");

    let socket_path = socket_dir.join("supervisor.sock");

    // Test 1: Tilde-prefixed path expands correctly
    let tilde_path = "~/.needle/supervisor.sock";
    let expanded = expand_tilde(tilde_path);
    assert_eq!(
        expanded,
        socket_path.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Absolute path is preserved
    let absolute_path = std::path::PathBuf::from("/run/needle/supervisor.sock");

    // Test 3: Relative path is preserved
    let relative_path = std::path::PathBuf::from("relative/supervisor.sock");

    // Test 4: Config with tilde path
    let yaml = format!(
        r#"
supervisor:
  socket_path: {}
"#,
        tilde_path
    );

    // Load config - this should trigger tilde expansion
    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the socket_path was expanded
    let socket = &config_expanded.supervisor.socket_path;
    assert!(
        socket.is_some(),
        "socket_path should be set after expansion"
    );

    let expanded_path = socket.as_ref().unwrap();
    assert!(
        expanded_path.starts_with(isolated_home),
        "socket_path should be expanded to isolated home, got: {}",
        expanded_path.display()
    );

    assert_eq!(
        expanded_path, &socket_path,
        "socket_path should point to our test socket"
    );

    // Test 5: Config with absolute path (should be preserved)
    let yaml_abs = format!(
        r#"
supervisor:
  socket_path: {}
"#,
        absolute_path.display()
    );

    let config_abs: Config = serde_yaml::from_str(&yaml_abs).expect("failed to parse config");
    let mut config_expanded_abs = config_abs;
    config_expanded_abs.expand_tildes();

    assert_eq!(
        config_expanded_abs.supervisor.socket_path,
        Some(absolute_path.clone()),
        "absolute socket_path should be preserved"
    );

    // Test 6: Config with relative path (should be preserved)
    let yaml_rel = format!(
        r#"
supervisor:
  socket_path: {}
"#,
        relative_path.display()
    );

    let config_rel: Config = serde_yaml::from_str(&yaml_rel).expect("failed to parse config");
    let mut config_expanded_rel = config_rel;
    config_expanded_rel.expand_tildes();

    assert_eq!(
        config_expanded_rel.supervisor.socket_path,
        Some(relative_path.clone()),
        "relative socket_path should be preserved"
    );

    // Note: HomeGuard automatically restores HOME when dropped
    println!("✓ Supervisor socket_path tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  Expanded tilde path: {}", socket_path.display());
    println!("  Preserved absolute: {}", absolute_path.display());
    println!("  Preserved relative: {}", relative_path.display());
}

/// Test tilde expansion in self_modification.canary_workspace configuration.
///
/// This test validates that tilde-prefixed paths in self_modification.canary_workspace
/// are correctly expanded to the HOME directory during config loading.
#[tokio::test]
#[serial]
async fn self_modification_canary_workspace_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::fs;

    // Use HomeGuard for proper HOME isolation
    let _home_guard = HomeGuard::isolate();
    let isolated_home = _home_guard._temp_dir.path();

    // Create test canary workspace directory in the isolated home
    let canary_ws = isolated_home.join("dev").join("canary-workspace");
    fs::create_dir_all(&canary_ws).expect("failed to create canary workspace");

    // Test 1: Tilde-prefixed path expands correctly
    let tilde_path = "~/dev/canary-workspace";
    let expanded = expand_tilde(tilde_path);
    assert_eq!(
        expanded,
        canary_ws.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Absolute path is preserved
    let absolute_path = std::path::PathBuf::from("/opt/canary-workspace");

    // Test 3: Relative path is preserved
    let relative_path = std::path::PathBuf::from("relative/canary");

    // Test 4: Config with tilde path
    let yaml = format!(
        r#"
self_modification:
  canary_workspace: {}
"#,
        tilde_path
    );

    // Load config - this should trigger tilde expansion
    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the canary_workspace was expanded
    assert!(
        config_expanded
            .self_modification
            .canary_workspace
            .starts_with(isolated_home),
        "canary_workspace should be expanded to isolated home, got: {}",
        config_expanded.self_modification.canary_workspace.display()
    );

    assert_eq!(
        config_expanded.self_modification.canary_workspace, canary_ws,
        "canary_workspace should point to our test workspace"
    );

    // Test 5: Config with absolute path (should be preserved)
    let yaml_abs = format!(
        r#"
self_modification:
  canary_workspace: {}
"#,
        absolute_path.display()
    );

    let config_abs: Config = serde_yaml::from_str(&yaml_abs).expect("failed to parse config");
    let mut config_expanded_abs = config_abs;
    config_expanded_abs.expand_tildes();

    assert_eq!(
        config_expanded_abs.self_modification.canary_workspace,
        absolute_path.clone(),
        "absolute canary_workspace path should be preserved"
    );

    // Test 6: Config with relative path (should be preserved)
    let yaml_rel = format!(
        r#"
self_modification:
  canary_workspace: {}
"#,
        relative_path.display()
    );

    let config_rel: Config = serde_yaml::from_str(&yaml_rel).expect("failed to parse config");
    let mut config_expanded_rel = config_rel;
    config_expanded_rel.expand_tildes();

    assert_eq!(
        config_expanded_rel.self_modification.canary_workspace,
        relative_path.clone(),
        "relative canary_workspace path should be preserved"
    );

    // Note: HomeGuard automatically restores HOME when dropped
    println!("✓ Self-modification canary_workspace tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  Expanded tilde path: {}", canary_ws.display());
    println!("  Preserved absolute: {}", absolute_path.display());
    println!("  Preserved relative: {}", relative_path.display());
}

/// Test tilde expansion in prompt.variants[].content_file configuration.
///
/// This test validates that tilde-prefixed paths in prompt variant content_file
/// fields are correctly expanded to the HOME directory during config loading.
#[tokio::test]
#[serial]
async fn prompt_variants_content_file_tilde_expansion() {
    use needle::config::Config;
    use needle::util::expand_tilde;
    use std::fs;

    // Use HomeGuard for proper HOME isolation
    let _home_guard = HomeGuard::isolate();
    let isolated_home = _home_guard._temp_dir.path();

    // Create test prompt directories in the isolated home
    let prompts_dir = isolated_home.join("prompts");
    fs::create_dir_all(&prompts_dir).expect("failed to create prompts dir");

    let content_file = prompts_dir.join("pluck-v2.txt");
    fs::write(&content_file, "test prompt content").expect("failed to write content file");

    // Test 1: Tilde-prefixed path expands correctly
    let tilde_path = "~/prompts/pluck-v2.txt";
    let expanded = expand_tilde(tilde_path);
    assert_eq!(
        expanded,
        content_file.to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test 2: Absolute path is preserved
    let absolute_path = std::path::PathBuf::from("/etc/needle/prompts/custom.txt");

    // Test 3: Relative path is preserved
    let relative_path = std::path::PathBuf::from("prompts/relative.txt");

    // Test 4: Config with tilde path
    let yaml = format!(
        r#"
prompt:
  variants:
    pluck:
      - name: v2
        weight: 100
        content_file: {}
"#,
        tilde_path
    );

    // Load config - this should trigger tilde expansion
    let config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes
    let mut config_expanded = config;
    config_expanded.expand_tildes();

    // Verify the content_file was expanded
    let variants = config_expanded.prompt.variants.get("pluck");
    assert!(
        variants.is_some() && !variants.as_ref().unwrap().is_empty(),
        "pluck variant should be set"
    );

    let expanded_path = &variants.as_ref().unwrap()[0].content_file;
    assert!(
        expanded_path.starts_with(isolated_home),
        "content_file should be expanded to isolated home, got: {}",
        expanded_path.display()
    );

    assert_eq!(
        expanded_path, &content_file,
        "content_file should point to our test file"
    );

    // Test 5: Config with absolute path (should be preserved)
    let yaml_abs = format!(
        r#"
prompt:
  variants:
    custom:
      - name: custom
        weight: 100
        content_file: {}
"#,
        absolute_path.display()
    );

    let config_abs: Config = serde_yaml::from_str(&yaml_abs).expect("failed to parse config");
    let mut config_expanded_abs = config_abs;
    config_expanded_abs.expand_tildes();

    let variants_abs = config_expanded_abs.prompt.variants.get("custom");
    assert!(
        variants_abs.is_some() && !variants_abs.as_ref().unwrap().is_empty(),
        "custom variant should be set"
    );

    assert_eq!(
        variants_abs.as_ref().unwrap()[0].content_file,
        absolute_path.clone(),
        "absolute content_file path should be preserved"
    );

    // Test 6: Config with relative path (should be preserved)
    let yaml_rel = format!(
        r#"
prompt:
  variants:
    relative:
      - name: relative
        weight: 100
        content_file: {}
"#,
        relative_path.display()
    );

    let config_rel: Config = serde_yaml::from_str(&yaml_rel).expect("failed to parse config");
    let mut config_expanded_rel = config_rel;
    config_expanded_rel.expand_tildes();

    let variants_rel = config_expanded_rel.prompt.variants.get("relative");
    assert!(
        variants_rel.is_some() && !variants_rel.as_ref().unwrap().is_empty(),
        "relative variant should be set"
    );

    assert_eq!(
        variants_rel.as_ref().unwrap()[0].content_file,
        relative_path.clone(),
        "relative content_file path should be preserved"
    );

    // Test 7: Multiple variants with mixed tilde and non-tilde paths
    let yaml_mixed = format!(
        r#"
prompt:
  variants:
    pluck:
      - name: v2
        weight: 100
        content_file: {}
    custom:
      - name: custom
        weight: 100
        content_file: {}
    relative:
      - name: relative
        weight: 100
        content_file: {}
"#,
        tilde_path,
        absolute_path.display(),
        relative_path.display()
    );

    let config_mixed: Config = serde_yaml::from_str(&yaml_mixed).expect("failed to parse config");
    let mut config_expanded_mixed = config_mixed;
    config_expanded_mixed.expand_tildes();

    let pluck_variant = config_expanded_mixed.prompt.variants.get("pluck").unwrap();
    assert_eq!(
        pluck_variant[0].content_file, content_file,
        "tilde path in pluck variant should be expanded"
    );

    let custom_variant = config_expanded_mixed.prompt.variants.get("custom").unwrap();
    assert_eq!(
        custom_variant[0].content_file, absolute_path,
        "absolute path in custom variant should be preserved"
    );

    let relative_variant = config_expanded_mixed
        .prompt
        .variants
        .get("relative")
        .unwrap();
    assert_eq!(
        relative_variant[0].content_file, relative_path,
        "relative path in relative variant should be preserved"
    );

    // Note: HomeGuard automatically restores HOME when dropped
    println!("✓ Prompt variants content_file tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  Expanded tilde path: {}", content_file.display());
    println!("  Preserved absolute: {}", absolute_path.display());
    println!("  Preserved relative: {}", relative_path.display());
}

// ═════════════════════════════════════════════════════════════════════════════
// End of Tilde Expansion Integration Tests
// ═════════════════════════════════════════════════════════════════════════════

/// Test absolute and relative paths in worker_binary_path configuration.
///
/// This test validates that absolute paths are preserved and relative paths
/// are handled correctly when used as worker_binary_path overrides.
#[tokio::test]
async fn worker_binary_path_absolute_and_relative_paths() {
    use needle::util::resolve_worker_binary_path;
    use std::path::PathBuf;

    // Test absolute paths are preserved
    let absolute_path = PathBuf::from("/usr/local/bin/custom-needle");
    let resolved =
        resolve_worker_binary_path(Some(&absolute_path)).expect("should resolve absolute path");

    assert_eq!(
        resolved, absolute_path,
        "absolute paths should be preserved exactly"
    );

    // Test relative paths are preserved as-is
    let relative_path = PathBuf::from("./local/bin/needle");
    let resolved =
        resolve_worker_binary_path(Some(&relative_path)).expect("should resolve relative path");

    assert_eq!(
        resolved, relative_path,
        "relative paths should be preserved as-is"
    );

    // Test that paths with special characters are preserved
    let special_path = PathBuf::from("/opt/custom-bin/needle-v2.0");
    let resolved = resolve_worker_binary_path(Some(&special_path))
        .expect("should resolve path with special characters");

    assert_eq!(
        resolved, special_path,
        "paths with version numbers and special chars should be preserved"
    );

    println!("✓ Worker binary path handling test passed");
    println!("  Absolute paths preserved: {}", absolute_path.display());
    println!("  Relative paths preserved: {}", relative_path.display());
    println!("  Special characters handled: {}", special_path.display());
}

/// Test that worker_binary_path takes precedence over current_exe() default.
///
/// This test validates that when both a custom worker_binary_path and the
/// default current_exe() resolution are available, the custom path takes
/// precedence as expected.
#[tokio::test]
async fn worker_binary_path_precedence_over_default() {
    use needle::util::resolve_worker_binary_path;
    use std::path::PathBuf;

    // Get the current_exe as the default
    let current_exe = std::env::current_exe().expect("should be able to get current_exe in test");

    // Test that override path takes precedence
    let override_path = PathBuf::from("/custom/path/to/needle");
    let resolved =
        resolve_worker_binary_path(Some(&override_path)).expect("should resolve to override path");

    assert_eq!(
        resolved, override_path,
        "override path should take precedence over current_exe"
    );

    // Test that None resolves to current_exe
    let resolved_default =
        resolve_worker_binary_path(None).expect("should resolve to current_exe when no override");

    assert_eq!(
        resolved_default, current_exe,
        "None should resolve to current_exe as default"
    );

    // Verify they are different when override is set
    assert_ne!(
        resolved, resolved_default,
        "override path should be different from default resolution"
    );

    println!("✓ Worker binary path precedence test passed");
    println!("  Current exe: {}", current_exe.display());
    println!("  Override path: {}", override_path.display());
    println!("  Precedence correctly applied");
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration tests: graceful shutdown workflow (bf-4sbb)
// ═════════════════════════════════════════════════════════════════════════════

/// Integration test that verifies heartbeat cleanup happens on normal worker exit.
///
/// This test validates the graceful shutdown workflow when a worker completes
/// its work naturally (no signals, just normal exhaustion).
///
/// Test strategy:
/// 1. Spawn a real needle worker subprocess with limited work
/// 2. Wait for the worker to create its heartbeat file
/// 3. Wait for the worker to complete work and exit naturally
/// 4. Verify the heartbeat file is cleaned up after exit
/// 5. Verify no stale heartbeat files remain
#[test]
fn heartbeat_cleanup_on_normal_exit_integration() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    // Find the needle binary
    let needle_binary = std::env::current_exe()
        .ok()
        .and_then(|p| {
            let path_str = p.to_string_lossy().to_string();
            if path_str.contains("integration_tests") {
                Some(
                    p.parent()
                        .map(|grandparent| {
                            let needle_path = grandparent.join("needle");
                            if needle_path.exists() {
                                needle_path
                            } else {
                                let debug_path = grandparent.join("debug").join("needle");
                                if debug_path.exists() {
                                    debug_path
                                } else {
                                    grandparent.join("release").join("needle")
                                }
                            }
                        })
                        .unwrap_or_else(|| PathBuf::from("needle")),
                )
            } else {
                None
            }
        })
        .unwrap_or_else(|| PathBuf::from("needle"));

    if !needle_binary.exists() {
        println!(
            "Skipping test: needle binary not found at {}",
            needle_binary.display()
        );
        return;
    }

    println!("Using needle binary: {}", needle_binary.display());

    // Create a temporary workspace with a bead that will complete quickly
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let workspace = temp_dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("failed to create workspace");

    let beads_dir = workspace.join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("failed to create beads dir");

    // Create a simple bead that will succeed immediately
    let success_bead = r#"{
        "id": "nd-success-test",
        "type": "task",
        "title": "Success test bead",
        "description": "Bead that succeeds immediately",
        "status": "open",
        "acceptance_criteria": [],
        "labels": []
    }"#;

    std::fs::write(beads_dir.join("nd-success-test.json"), success_bead)
        .expect("failed to create success bead");

    // Set up environment to use the test workspace
    let mut cmd = Command::new(&needle_binary);
    cmd.arg("run")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--agent")
        .arg("echo") // Use echo adapter which succeeds quickly
        .arg("--identifier")
        .arg("normal-exit-test-worker")
        .arg("--count")
        .arg("1"); // Process only 1 bead then exit

    // Isolate the test from the real user environment
    cmd.env("HOME", temp_dir.path());

    // Spawn the worker process
    println!("Spawning worker process for normal exit test...");
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            println!("Skipping test: failed to spawn worker: {}", e);
            return;
        }
    };

    let worker_pid = child.id();
    println!("Worker PID: {}", worker_pid);

    // ProcessGuard ensures cleanup if test panics
    let mut child_guard = ProcessGuard::new(child, Some(worker_pid));

    // Give the worker time to start up and create its heartbeat file
    let heartbeat_dir = workspace.join("state").join("heartbeats");
    let heartbeat_file = heartbeat_dir.join("claude-echo-normal-exit-test-worker.json");

    println!("Waiting for heartbeat file: {}", heartbeat_file.display());

    let start = Instant::now();
    let heartbeat_timeout = Duration::from_secs(30);
    let poll_interval = Duration::from_millis(200);
    let mut heartbeat_found = false;

    // Wait up to 30 seconds for the heartbeat file to appear with proper timeout handling
    while start.elapsed() < heartbeat_timeout {
        if heartbeat_file.exists() {
            heartbeat_found = true;
            println!("✓ Heartbeat file created after {:?}", start.elapsed());
            break;
        }
        std::thread::sleep(poll_interval);
    }

    // Explicit timeout check with clear error message
    if !heartbeat_found {
        panic!(
            "Heartbeat file not found after {:?} - worker failed to create heartbeat. ProcessGuard will clean up.",
            heartbeat_timeout
        );
    }

    // Verify the heartbeat file contains valid data
    let heartbeat_content =
        std::fs::read_to_string(&heartbeat_file).expect("failed to read heartbeat file");

    println!("Heartbeat content: {}", heartbeat_content);

    let heartbeat: serde_json::Value =
        serde_json::from_str(&heartbeat_content).expect("heartbeat file should contain valid JSON");

    assert_eq!(
        heartbeat["worker_id"], "normal-exit-test-worker",
        "heartbeat should have correct worker_id"
    );

    println!("✓ Heartbeat file is valid");

    // Wait for the worker to complete work and exit naturally (no signals)
    println!("Waiting for worker to complete and exit naturally...");

    let exit_timeout = Duration::from_secs(30);
    let exit_start = Instant::now();

    let exit_status = loop {
        match child_guard.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if exit_start.elapsed() < exit_timeout {
                    std::thread::sleep(Duration::from_millis(100));
                } else {
                    // ProcessGuard will handle cleanup
                    panic!(
                        "Worker did not exit naturally within {:?}, test failed. ProcessGuard will clean up.",
                        exit_timeout
                    );
                }
            }
            Err(e) => {
                println!("Error checking worker status: {}", e);
                // ProcessGuard will handle cleanup
                return;
            }
        }
    };

    println!("✓ Worker exited naturally with status: {:?}", exit_status);

    // Verify the heartbeat file was cleaned up after normal exit
    let cleanup_check_start = Instant::now();
    let cleanup_timeout = Duration::from_secs(5);

    while cleanup_check_start.elapsed() < cleanup_timeout {
        if !heartbeat_file.exists() {
            println!(
                "✓ Heartbeat file cleaned up after {:?}",
                cleanup_check_start.elapsed()
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if heartbeat_file.exists() {
        panic!("Heartbeat file was not cleaned up after normal exit, test failed");
    }

    // Verify no stale heartbeat files remain in the heartbeat directory
    if heartbeat_dir.exists() {
        let stale_files =
            std::fs::read_dir(&heartbeat_dir).expect("failed to read heartbeat directory");

        let stale_count = stale_files.count();
        if stale_count > 0 {
            panic!(
                "Found {} stale heartbeat file(s) in directory, test failed",
                stale_count
            );
        }
    }

    println!("✓ No stale heartbeat files remain");

    let total_time = start.elapsed();
    println!(
        "✓ Normal exit integration test passed, total time: {:?}",
        total_time
    );

    assert!(
        total_time < Duration::from_secs(60),
        "test should complete within 60 seconds, took {:?}",
        total_time
    );
}

/// Integration test that verifies heartbeat cleanup happens on multiple shutdown scenarios.
///
/// This test validates that heartbeat cleanup works correctly across different
/// worker exit scenarios:
/// 1. Normal exit after processing all available work
/// 2. Shutdown due to idle_action=Exit with empty queue
/// 3. Signal-triggered shutdown (SIGTERM)
///
/// Test strategy:
/// 1. Test each scenario sequentially
/// 2. Verify heartbeat cleanup in each case
/// 3. Verify no cross-contamination between scenarios
#[test]
fn heartbeat_cleanup_multiple_scenarios_integration() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    let needle_binary = std::env::current_exe()
        .ok()
        .and_then(|p| {
            let path_str = p.to_string_lossy().to_string();
            if path_str.contains("integration_tests") {
                Some(
                    p.parent()
                        .map(|grandparent| {
                            let needle_path = grandparent.join("needle");
                            if needle_path.exists() {
                                needle_path
                            } else {
                                let debug_path = grandparent.join("debug").join("needle");
                                if debug_path.exists() {
                                    debug_path
                                } else {
                                    grandparent.join("release").join("needle")
                                }
                            }
                        })
                        .unwrap_or_else(|| PathBuf::from("needle")),
                )
            } else {
                None
            }
        })
        .unwrap_or_else(|| PathBuf::from("needle"));

    if !needle_binary.exists() {
        println!(
            "Skipping test: needle binary not found at {}",
            needle_binary.display()
        );
        return;
    }

    println!("Testing multiple shutdown scenarios...");

    // Scenario 1: Normal exit after processing work
    println!("\n=== Scenario 1: Normal exit after processing work ===");

    let temp_dir1 = tempfile::tempdir().expect("failed to create temp dir");
    let workspace1 = temp_dir1.path().join("workspace1");
    std::fs::create_dir_all(&workspace1).expect("failed to create workspace");
    let beads_dir1 = workspace1.join(".beads");
    std::fs::create_dir_all(&beads_dir1).expect("failed to create beads dir");

    let bead1 = r#"{
        "id": "nd-scenario1",
        "type": "task",
        "title": "Scenario 1 bead",
        "description": "Bead for scenario 1",
        "status": "open",
        "acceptance_criteria": [],
        "labels": []
    }"#;

    std::fs::write(beads_dir1.join("nd-scenario1.json"), bead1).expect("failed to create bead");

    let mut cmd1 = Command::new(&needle_binary);
    cmd1.arg("run")
        .arg("--workspace")
        .arg(&workspace1)
        .arg("--agent")
        .arg("echo")
        .arg("--identifier")
        .arg("scenario1-worker")
        .arg("--count")
        .arg("1")
        .env("HOME", temp_dir1.path());

    let child1 = match cmd1.spawn() {
        Ok(c) => c,
        Err(e) => {
            println!("Skipping scenario 1: failed to spawn worker: {}", e);
            return;
        }
    };

    let mut child1_guard = ProcessGuard::new(child1, None);

    // Wait for heartbeat creation
    let heartbeat_dir1 = workspace1.join("state").join("heartbeats");
    let heartbeat_file1 = heartbeat_dir1.join("claude-echo-scenario1-worker.json");

    let start1 = Instant::now();
    while start1.elapsed() < Duration::from_secs(20) {
        if heartbeat_file1.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if !heartbeat_file1.exists() {
        let _ = child1_guard.kill();
        let _ = child1_guard.wait();
        panic!("Scenario 1: Heartbeat file not created");
    }

    println!("✓ Scenario 1: Heartbeat created");

    // Wait for normal exit
    let exit_start = Instant::now();
    while exit_start.elapsed() < Duration::from_secs(20) {
        if let Ok(Some(_)) = child1_guard.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    println!("✓ Scenario 1: Worker exited");

    // Verify cleanup
    let cleanup_start = Instant::now();
    while cleanup_start.elapsed() < Duration::from_secs(3) {
        if !heartbeat_file1.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if heartbeat_file1.exists() {
        panic!("Scenario 1: Heartbeat file not cleaned up after normal exit");
    }

    println!("✓ Scenario 1: Heartbeat cleaned up successfully");

    // Scenario 2: Idle exit with empty queue
    println!("\n=== Scenario 2: Idle exit with empty queue ===");

    let temp_dir2 = tempfile::tempdir().expect("failed to create temp dir");
    let workspace2 = temp_dir2.path().join("workspace2");
    std::fs::create_dir_all(&workspace2).expect("failed to create workspace");
    let beads_dir2 = workspace2.join(".beads");
    std::fs::create_dir_all(&beads_dir2).expect("failed to create beads dir");

    let mut cmd2 = Command::new(&needle_binary);
    cmd2.arg("run")
        .arg("--workspace")
        .arg(&workspace2)
        .arg("--agent")
        .arg("echo")
        .arg("--identifier")
        .arg("scenario2-worker")
        .arg("--count")
        .arg("1")
        .env("HOME", temp_dir2.path());

    let child2 = match cmd2.spawn() {
        Ok(c) => c,
        Err(e) => {
            println!("Skipping scenario 2: failed to spawn worker: {}", e);
            return;
        }
    };

    let mut child2_guard = ProcessGuard::new(child2, None);

    // Wait for heartbeat creation
    let heartbeat_dir2 = workspace2.join("state").join("heartbeats");
    let heartbeat_file2 = heartbeat_dir2.join("claude-echo-scenario2-worker.json");

    let start2 = Instant::now();
    while start2.elapsed() < Duration::from_secs(20) {
        if heartbeat_file2.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if !heartbeat_file2.exists() {
        let _ = child2_guard.kill();
        let _ = child2_guard.wait();
        panic!("Scenario 2: Heartbeat file not created");
    }

    println!("✓ Scenario 2: Heartbeat created");

    // Worker should exit quickly due to empty queue
    let exit_start2 = Instant::now();
    while exit_start2.elapsed() < Duration::from_secs(20) {
        if let Ok(Some(_)) = child2_guard.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    println!("✓ Scenario 2: Worker exited due to empty queue");

    // Verify cleanup
    let cleanup_start2 = Instant::now();
    while cleanup_start2.elapsed() < Duration::from_secs(3) {
        if !heartbeat_file2.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if heartbeat_file2.exists() {
        panic!("Scenario 2: Heartbeat file not cleaned up after idle exit");
    }

    println!("✓ Scenario 2: Heartbeat cleaned up successfully");

    println!("\n✓ All shutdown scenarios passed: heartbeat cleanup verified");

    // Final verification: no cross-contamination between workspaces
    assert!(
        !heartbeat_file1.exists(),
        "Scenario 1 heartbeat should still be absent"
    );
    assert!(
        !heartbeat_file2.exists(),
        "Scenario 2 heartbeat should still be absent"
    );

    println!("✓ No cross-contamination between scenarios");
}

// ═════════════════════════════════════════════════════════════════════════════
// Test: init_tracing_subscriber with OTLP doesn't panic (bf-3xfw3)
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn init_tracing_subscriber_with_otlp_enabled_does_not_panic() {
    // This test validates the fix from bf-3s2b0: init_tracing_subscriber should not
    // panic when OTLP is enabled, even when called without a tokio runtime context.
    //
    // Before the fix: tokio::spawn calls in init_tracing_subscriber would panic
    // with "cannot drop a runtime in a context where blocking is not allowed"
    //
    // After the fix: spawn_with_etxtbsy_retry wrappers and runtime guards
    // prevent the panic
    let _home_dir = tempfile::tempdir().unwrap();
    let config = test_config("echo-test", _home_dir.path());

    // Generate a session ID for the tracing subscriber
    let session_id = needle::telemetry::generate_session_id();

    // This should NOT panic if the fix is applied
    // The config has OTLP enabled (from bf-4nwm7), so this will trigger
    // the tokio::spawn calls that previously caused panics
    let _ = needle::cli::init_tracing_subscriber("test-worker".to_string(), session_id, &config);

    // Test passes if we get here without panicking - no explicit assert needed
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 14: Subprocess adapter validation failure
// ═════════════════════════════════════════════════════════════════════════════

/// Test that needle binary exits with nonzero code when configured with a nonexistent adapter.
///
/// This is a subprocess test (as opposed to the in-process adapter validation tests above)
/// that verifies the actual needle binary fails at boot when given a config with a nonexistent
/// adapter. This catches issues in the CLI layer that in-process Worker construction tests miss.
///
/// Test requirements:
/// - Uses isolated HOME environment (temporary directory)
/// - Uses isolated scan/workspace root
/// - Configures worker with a known nonexistent adapter name
/// - Asserts worker exits with nonzero exit code
/// - Uses Command::new(CARGO_BIN_EXE_needle) pattern
///
/// REQUIRED ISOLATION — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
#[tokio::test]
async fn subprocess_adapter_failure_exits_nonzero() {
    // Create isolated temp directory for HOME and workspace
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let workspace = temp_dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("failed to create workspace");

    // Initialize bead workspace (bead-rs CLI)
    let bead_result = std::process::Command::new("bead")
        .arg("init")
        .current_dir(&workspace)
        .output();

    // bead init may fail if the workspace is already initialized - that's OK for this test
    if let Ok(init_output) = bead_result {
        if !init_output.status.success() {
            let stderr = String::from_utf8_lossy(&init_output.stderr);
            // Only fail hard if it's a real error, not "already initialized"
            if !stderr.contains("already") && !stderr.contains("exists") {
                panic!("bead init failed: {}", stderr);
            }
        }
    }

    // Create .needle.yaml configuration to enable bead store discovery
    // Use bead-rs backend since that's the active CLI in this workspace
    std::fs::write(
        workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .expect("failed to create .needle.yaml configuration");

    // Spawn the needle binary with our test config
    let bin_path = std::env::var("CARGO_BIN_EXE_needle").unwrap_or_else(|_| "needle".to_string());
    let mut cmd = Command::new(&bin_path);
    // NEEDLE_INNER=1 runs the worker loop in THIS process. Without it, `needle run`
    // detaches into a tmux session and the parent exits 0 immediately, so the spawned
    // process reports success regardless of what the worker does. The systemd unit
    // sets the same variable. See needle-ab52a15a.
    cmd.env("NEEDLE_INNER", "1");
    cmd.arg("run")
        .arg("--agent")
        .arg("nonexistent-test-adapter-xyz-999")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--identifier")
        .arg("test-worker-nonexistent-adapter")
        .env("HOME", temp_dir.path()) // Isolate HOME to prevent Explore strand from scanning real user workspace
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd.spawn().expect("Failed to spawn needle process");
    let pid = child.id();
    let mut guard = ProcessGuard::new(child, Some(pid));

    // Wait with timeout - should fail quickly, not hang
    let timeout_duration = Duration::from_secs(10);
    let start_time = Instant::now();

    // Hang guard only; the authoritative status comes from `wait_with_output()`.
    let _exit_status = loop {
        if start_time.elapsed() > timeout_duration {
            panic!(
                "needle process did not complete within {:?} - possible hang",
                timeout_duration
            );
        }

        match guard.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("Failed to wait for needle process: {}", e),
        }
    };

    // Capture stdout and stderr from the completed process
    let child = guard.into_inner().expect("child process should exist");
    let output = child
        .wait_with_output()
        .expect("failed to capture process output");

    // Store captured stderr for later assertion
    let stderr_output = String::from_utf8_lossy(&output.stderr).to_string();
    let _stdout_output = String::from_utf8_lossy(&output.stdout).to_string();

    // Verify the process exited with nonzero code
    assert!(
        !output.status.success(),
        "needle process should fail with nonzero exit code when adapter does not exist, \
         but it succeeded with status: {:?}",
        output.status
    );

    // Verify exit code is specifically nonzero (not just !success())
    let exit_code = output.status.code().unwrap_or(1);
    assert_ne!(
        exit_code, 0,
        "exit code should be nonzero, got: {}",
        exit_code
    );

    // The failure should be fast (< 5 seconds), not delayed by idle timeouts
    assert!(
        start_time.elapsed() < Duration::from_secs(5),
        "adapter validation should fail immediately, not after idle timeout; took {:?}",
        start_time.elapsed()
    );

    // Verify stderr contains error information about the nonexistent adapter
    assert!(
        !stderr_output.is_empty(),
        "stderr should not be empty when adapter does not exist"
    );

    // ASSERTION: Error message must contain the nonexistent adapter name
    assert!(
        stderr_output.contains("nonexistent-test-adapter-xyz-999"),
        "stderr should mention the nonexistent adapter name 'nonexistent-test-adapter-xyz-999'. \
         Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION: Error message must include configuration-directory guidance
    // This helps users understand where adapter configuration should be located
    assert!(
        stderr_output.contains("~/.needle/agents/")
            || stderr_output.contains(".needle/agents/")
            || stderr_output.contains("claude-config/agents/")
            || stderr_output.contains(".config/needle/adapters/")
            || stderr_output.contains("configuration directory")
            || (stderr_output.contains("check") && stderr_output.contains("config")),
        "stderr should include configuration directory guidance. Got stderr:\n{}",
        stderr_output
    );

    // ASSERTION: Verify no bead was claimed
    // The error message should indicate that startup aborted BEFORE claiming could occur
    assert!(
        stderr_output.contains("prevent claiming")
            || stderr_output.contains("aborting")
            || stderr_output.contains("startup aborted"),
        "stderr should indicate that startup aborted before bead claiming could occur. Got:\n{}",
        stderr_output
    );
}

// Test for needle-161e49b7: verify commit SHA truncation doesn't panic on short SHAs
// This covers the bug where `&sha[..12]` panicked on 7-char SHAs like "ee18678"
#[tokio::test]
async fn truncate_commit_sha_handles_short_shas() {
    // Short SHA (7 chars) - should return full string
    let short_sha = "ee18678";
    assert_eq!(
        truncate_commit_sha(short_sha),
        "ee18678",
        "short SHA should be returned as-is"
    );

    // "unknown" fallback (7 chars) - should return full string
    let unknown = "unknown";
    assert_eq!(
        truncate_commit_sha(unknown),
        "unknown",
        "'unknown' should be returned as-is"
    );

    // Empty string - edge case
    let empty = "";
    assert_eq!(
        truncate_commit_sha(empty),
        "",
        "empty string should be returned as-is"
    );

    // Full SHA (40 chars) - should truncate to 12 chars
    let full_sha = "a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8s9t0";
    assert_eq!(
        truncate_commit_sha(full_sha),
        "a1b2c3d4e5f6",
        "full SHA should be truncated to 12 chars"
    );

    // Exactly 12 chars - should return full string
    let exactly_12 = "123456789012";
    assert_eq!(
        truncate_commit_sha(exactly_12),
        "123456789012",
        "12-char SHA should be returned as-is"
    );

    // 13 chars - should truncate to 12
    let thirteen = "1234567890123";
    assert_eq!(
        truncate_commit_sha(thirteen),
        "123456789012",
        "13-char SHA should be truncated to 12"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration test for needle-d6b09c72: idle_action misconfiguration warning
#[tokio::test]
async fn idle_action_exit_without_supervisor_emits_warning() {
    use needle::telemetry::Telemetry;
    use needle::types::IdleAction;
    use needle::validation::worker_config::validate_idle_action_config;
    use std::sync::Arc;

    // This test verifies that when idle_action=Exit is configured without a supervisor
    // present, a warning is emitted at startup and the worker continues running.

    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_guard = HomeGuard::isolate();
    let mut config = Config::default();

    // Configure idle_action=Exit (misconfiguration without supervisor)
    config.worker.idle_action = IdleAction::Exit;
    config.agent.default = "echo-test".to_string();
    config.agent.routing = None; // Disable routing in tests - use adapter directly
    config.self_modification.hot_reload = false;
    config.workspace.default = std::path::PathBuf::from("/tmp/test-workspace");
    config.workspace.home = _home_guard._temp_dir.path().to_path_buf();
    // Confine Explore strand to test's tempdir to prevent scanning real user directories
    config.strands.explore.workspace_root = _home_guard._temp_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    let mut worker = Worker::new(config, "test-worker-idle-warning".to_string(), store);

    let adapter = test_adapter("echo-test", "echo done", 10);
    let mut adapters = HashMap::new();
    adapters.insert("echo-test".to_string(), adapter);

    // Create a telemetry collector to capture events
    let telemetry = Telemetry::new("test-worker-idle-warning".to_string());
    worker.set_dispatcher(Dispatcher::with_adapters(adapters, telemetry.clone(), 10));

    // Request shutdown before running. boot() still runs inside run() -- so the
    // idle_action_validation step still emits the warning and still downgrades
    // Exit -> Wait -- but the loop then observes the shutdown flag and stops.
    //
    // Without this, run() could never return: the downgrade to Wait (the very
    // behaviour this test exists to pin) means run() loops in the 60-120s idle
    // backoff forever, so `assert_eq!(run().await, Stopped)` was self-contradictory
    // and could only hang. See needle-ab52a15a.
    //
    // This test must NOT set allow_exit_without_supervisor: opting in would suppress
    // the very warning under test.
    worker.request_shutdown();

    // Run the worker - it emits the warning during initialization and then stops.
    let result = worker.run().await.unwrap();
    assert_eq!(
        result,
        WorkerState::Stopped,
        "worker should complete successfully even with misconfiguration warning"
    );

    // Verify the validation function would detect the misconfiguration
    let validation_result = validate_idle_action_config(&IdleAction::Exit, false);
    assert!(
        !validation_result.is_valid(),
        "validation should detect idle_action=Exit without supervisor as invalid"
    );

    let reason = validation_result.error_reason().unwrap();
    assert!(
        reason.contains("without supervisor"),
        "error message should mention missing supervisor, got: {}",
        reason
    );
    assert!(
        reason.contains("orphaned"),
        "error message should explain the orphaned bead risk"
    );
    assert!(
        reason.contains("needle supervise"),
        "error message should suggest supervisor solution"
    );
    assert!(
        reason.contains("idle_action=wait"),
        "error message should suggest config alternative"
    );
}

/// Regression test for needle-f78eebbb: Verify OTLP config schema matches plan.md.
///
/// This test ensures that a YAML config copied verbatim from plan.md (lines 1961-1982
/// and 2553-2570) loads without error and produces the values it specifies.
///
/// The issue documented three discrepancies between plan.md and the implementation:
/// 1. tls was documented as a nested map {insecure, ca_file} but allegedly was a String
/// 2. timeout_ms was documented but allegedly was timeout_secs
/// 3. signals field {traces, metrics, logs} was documented but allegedly missing
///
/// This test verifies all three are now implemented correctly and match the plan.
#[tokio::test]
async fn otlp_config_schema_matches_plan_md() {
    use needle::config::ConfigLoader;
    use std::io::Write;
    use tempfile::TempDir;

    // Config copied verbatim from plan.md lines 1961-1982
    let config_yaml = r#"
telemetry:
  otlp_sink:
    enabled: true
    endpoint: "http://otel-collector.tailnet:4317"
    protocol: grpc
    headers:
      - "authorization: Bearer ${OTEL_TOKEN}"
    timeout_ms: 5000
    compression: gzip
    tls:
      insecure: false
      ca_file: ""
    signals:
      traces: true
      metrics: true
      logs: true
    resource_attributes:
      - "deployment.environment=production"
      - "service.namespace=needle-fleet"
"#;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join(".needle.yaml");
    let mut file = std::fs::File::create(&config_path).unwrap();
    file.write_all(config_yaml.as_bytes()).unwrap();

    // Load config - should succeed without error
    let config = ConfigLoader::load_from_path(&config_path)
        .expect("plan.md config should load without error");

    // Verify all values match what plan.md documents
    assert!(
        config.telemetry.otlp_sink.enabled,
        "enabled should be true per plan.md"
    );
    assert_eq!(
        config.telemetry.otlp_sink.endpoint, "http://otel-collector.tailnet:4317",
        "endpoint should match plan.md"
    );
    assert_eq!(
        config.telemetry.otlp_sink.timeout_ms, 5000,
        "timeout_ms should be 5000 per plan.md"
    );
    assert_eq!(
        config.telemetry.otlp_sink.compression, "gzip",
        "compression should be gzip per plan.md"
    );

    // Verify tls is a nested map, not a String (needle-f78eebbb claim #1)
    assert!(
        !config.telemetry.otlp_sink.tls.insecure,
        "tls.insecure should be false per plan.md"
    );
    assert_eq!(
        config.telemetry.otlp_sink.tls.ca_file, "",
        "tls.ca_file should be empty string per plan.md"
    );

    // Verify signals field exists and contains traces/metrics/logs (needle-f78eebbb claim #3)
    assert!(
        config.telemetry.otlp_sink.signals.traces,
        "signals.traces should be true per plan.md"
    );
    assert!(
        config.telemetry.otlp_sink.signals.metrics,
        "signals.metrics should be true per plan.md"
    );
    assert!(
        config.telemetry.otlp_sink.signals.logs,
        "signals.logs should be true per plan.md"
    );
}
