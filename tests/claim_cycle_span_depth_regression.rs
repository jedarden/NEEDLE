//! Claim-cycle span-depth regression coverage (bead needle-833ad3a3, bf-3uj6i).
//!
//! On 2026-08-05 a worker's stderr grew to 33.7 GB at ~159 GB/hr. Every claim
//! cycle left one `bead.claim` plus one `bead.lifecycle` entry on the tracing
//! thread-local span stack (an `EnteredSpan` guard was held across `.await`
//! points and stored in the `Worker` struct), and because the `fmt` layer
//! re-serializes the whole span stack on every event, each leaked entry made
//! every subsequent line longer: depth 18 / 4,983-byte lines early in the
//! session, depth 2,488 / 629,829-byte lines late.
//!
//! These tests guard both halves of that failure:
//!
//! 1. [`claim_cycles_keep_bead_span_depth_constant`] drives a real worker
//!    through 200 consecutive full claim cycles (select → claim → build →
//!    dispatch → execute → handle → log) against an in-memory store, with
//!    every path that could touch the real home directory pinned to a
//!    tempdir. It then asserts that no formatted line carries more than one
//!    `bead.claim` or one `bead.lifecycle` entry, and that the longest line
//!    stays under the 64 KiB production line cap — bounds that are
//!    independent of the cycle count.
//!
//!    The worker future is `tokio::spawn`ed, which makes the guard-lifetime
//!    defect class a *compile* error as well: storing an `EnteredSpan` (which
//!    is deliberately `!Send`) in the `Worker` struct would make the
//!    state-machine future `!Send`, and this test would no longer build.
//!
//! 2. [`depth_probe_detects_leaked_span_entries`] proves the depth probe used
//!    in (1) is not vacuous: when span entries genuinely leak — the residue a
//!    guard leaves when its `Drop` runs somewhere the entry was never pushed,
//!    which is the only leak shape `tracing-subscriber` 0.3.x has (same-thread
//!    out-of-order exits pop by reverse search) — the probe reports depth
//!    growing linearly, and when spans unwind normally it reports a constant
//!    depth of one. If a future tracing upgrade stops rendering the span
//!    stack, or the probe stops counting it, this test fails instead of
//!    letting (1) pass vacuously.
//!
//! # Isolation
//!
//! Per the test-isolation policy (ADR-006, `docs/testing-isolation-patterns.md`)
//! every filesystem touchpoint is pinned into a tempdir: the process `HOME`
//! itself, plus `workspace.home`/`workspace.default` (which also pins the
//! worker registry), the Explore strand's scan root, the telemetry file-sink
//! directory, and the heartbeat directory. The Explore strand is disabled
//! outright.
//!
//! Pinning the process `HOME` — not just the config fields — is required, not
//! belt-and-braces: pluck's per-workspace diagnostics are written to
//! `~/.needle/diagnostics/<slug>/` by `needle_diagnostics_dir`, which reads
//! `$HOME` directly and has no config field, so on 2026-09-10 an unpinned run
//! appended ~21k diagnostic lines into the operator's real
//! `~/.needle/diagnostics/`. Any other code that falls back to the process
//! HOME for an unpinned path (`resolve_heartbeat_dir` for relative paths, the
//! global config lookup) lands in the tempdir for the same reason. `HOME` is
//! restored by a drop guard, so a panic cannot leave it pointing at a deleted
//! directory.
//!
//! The claim-path circuit gate is disabled because it consults live CI state,
//! and the launch-admission gate's override is set so a busy host cannot hold
//! selection. Nothing here scans or mutates the real home directory.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use chrono::Utc;

use needle::bead_store::{BeadStore, RepairReport};
use needle::config::Config;
use needle::dispatch::{AgentAdapter, Dispatcher, TokenExtraction};
use needle::log_writer::DEFAULT_MAX_LINE_BYTES;
use needle::telemetry::Telemetry;
use needle::types::{Bead, BeadId, BeadStatus, ClaimResult, IdleAction, InputMethod, WorkerState};
use needle::worker::Worker;

/// Number of consecutive claim cycles the worker must complete.
const CLAIM_CYCLES: usize = 200;

/// Upper bound on one `run()` — a safety net so a worker that never reaches a
/// terminal state fails the test with a readable message instead of hanging.
const RUN_TIMEOUT: Duration = Duration::from_secs(300);

// ─── Output capture ──────────────────────────────────────────────────────────

/// Shared in-memory sink for formatted `tracing` output.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Largest number of `name{` occurrences on any single captured line.
fn max_span_depth(captured: &Captured, span_name: &str) -> usize {
    let needle = format!("{span_name}{{");
    String::from_utf8_lossy(&captured.bytes())
        .lines()
        .map(|line| line.matches(&needle).count())
        .max()
        .unwrap_or(0)
}

/// Longest captured line, in bytes.
fn max_line_bytes(captured: &Captured) -> usize {
    String::from_utf8_lossy(&captured.bytes())
        .lines()
        .map(|line| line.len())
        .max()
        .unwrap_or(0)
}

/// Whether any captured line rendered the given span at all. Guards the depth
/// assertions against passing vacuously because nothing was captured.
fn saw_span(captured: &Captured, span_name: &str) -> bool {
    let needle = format!("{span_name}{{");
    String::from_utf8_lossy(&captured.bytes())
        .lines()
        .any(|line| line.contains(&needle))
}

// ─── Test store ──────────────────────────────────────────────────────────────

/// In-memory store serving a fixed queue of claimable beads, one claim cycle
/// each. Distinct bead ids per cycle matter: the claim path circuit-breaks a
/// single bead after `MAX_CLAIM_EVENTS_PER_BEAD` claim events, so reusing one
/// id would quarantine it halfway through and turn this into a test of the
/// breaker rather than of span depth.
///
/// The store models the production division of labour, in which the *agent*
/// subprocess closes its bead and the worker only verifies:
///
/// - [`BeadStore::claim_status`] reports the live claim state — this is what
///   the dispatcher re-checks immediately before spawning, and it must still
///   say in_progress there or every dispatch is aborted.
/// - [`BeadStore::show`] *performs* the closure the stub agent cannot: a
///   shell stub has no `bead close` to run, so the first post-dispatch read
///   of an in-progress bead records that the agent finished it. The
///   transition has to be a real store mutation, not just a projected value.
///   Returning `Closed` while leaving the row in_progress (the original
///   fixture) satisfied the outcome handler's "confirmed closed" check but
///   left every bead in_progress and assigned forever; the mend strand then
///   reaped them as assignee overlap and released the whole queue back into
///   `ready()` — 10,775 claim cycles in 305 s and the queue never drained.
struct CycleStore {
    beads: Mutex<Vec<Bead>>,
    claims: AtomicUsize,
    releases: AtomicUsize,
    claimed_ids: Mutex<HashSet<String>>,
}

impl CycleStore {
    fn new(beads: Vec<Bead>) -> Self {
        Self {
            beads: Mutex::new(beads),
            claims: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
            claimed_ids: Mutex::new(HashSet::new()),
        }
    }

    fn claim_count(&self) -> usize {
        self.claims.load(Ordering::SeqCst)
    }
    fn release_count(&self) -> usize {
        self.releases.load(Ordering::SeqCst)
    }
    fn distinct_claimed(&self) -> usize {
        self.claimed_ids.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl BeadStore for CycleStore {
    async fn ready(&self, _filters: &needle::bead_store::Filters) -> anyhow::Result<Vec<Bead>> {
        Ok(self
            .beads
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b.status == BeadStatus::Open && b.assignee.is_none())
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> anyhow::Result<Vec<Bead>> {
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn show(&self, id: &BeadId) -> anyhow::Result<Bead> {
        let mut bead = self
            .beads
            .lock()
            .unwrap()
            .iter()
            .find(|b| &b.id == id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))?;
        // The stub agent "closed" its bead while the worker was busy
        // dispatching it. Record that closure in the store itself — see the
        // CycleStore doc for why a projected value is not enough. No read
        // between the claim and the outcome lands here while a bead is
        // in_progress (the dispatcher's pre-spawn check goes through
        // `claim_status`), so this first post-dispatch read is exactly the
        // moment the agent's work is observed.
        if bead.status == BeadStatus::InProgress {
            bead.status = BeadStatus::Closed;
            bead.assignee = None;
            if let Some(stored) = self.beads.lock().unwrap().iter_mut().find(|b| &b.id == id) {
                stored.status = BeadStatus::Closed;
                stored.assignee = None;
            }
        }
        Ok(bead)
    }

    async fn claim_status(&self, id: &BeadId) -> anyhow::Result<needle::types::ClaimStatus> {
        // Live claim state, distinct from the `show` projection above: the
        // dispatcher's pre-spawn verification must see this bead as in_progress
        // and assigned to this worker, exactly as the real store would.
        let bead = self
            .beads
            .lock()
            .unwrap()
            .iter()
            .find(|b| &b.id == id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))?;
        Ok(needle::types::ClaimStatus {
            status: bead.status,
            assignee: bead.assignee,
            revision: None,
        })
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> anyhow::Result<ClaimResult> {
        let mut beads = self.beads.lock().unwrap();
        let Some(bead) = beads.iter_mut().find(|b| &b.id == id) else {
            return Ok(ClaimResult::NotClaimable {
                reason: "not found".to_string(),
            });
        };
        bead.status = BeadStatus::InProgress;
        bead.assignee = Some(actor.to_string());
        let claimed = bead.clone();
        drop(beads);

        self.claims.fetch_add(1, Ordering::SeqCst);
        self.claimed_ids
            .lock()
            .unwrap()
            .insert(claimed.id.to_string());
        Ok(ClaimResult::Claimed(claimed))
    }

    async fn claim_auto(&self, _actor: &str) -> anyhow::Result<ClaimResult> {
        Ok(ClaimResult::NotClaimable {
            reason: "claim_auto is not part of the claim-cycle path under test".to_string(),
        })
    }

    async fn release(&self, id: &BeadId) -> anyhow::Result<()> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        if let Some(bead) = self.beads.lock().unwrap().iter_mut().find(|b| &b.id == id) {
            bead.status = BeadStatus::Open;
            bead.assignee = None;
        }
        Ok(())
    }

    async fn block(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, id: &BeadId) -> anyhow::Result<()> {
        if let Some(bead) = self.beads.lock().unwrap().iter_mut().find(|b| &b.id == id) {
            bead.assignee = None;
        }
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn close(&self, id: &BeadId, _reason: &str) -> anyhow::Result<()> {
        if let Some(bead) = self.beads.lock().unwrap().iter_mut().find(|b| &b.id == id) {
            bead.status = BeadStatus::Closed;
            bead.assignee = None;
        }
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn create_bead(
        &self,
        title: &str,
        _body: &str,
        _labels: &[&str],
    ) -> anyhow::Result<BeadId> {
        // Deliberately does NOT enqueue new work: a strand that creates beads
        // must not be able to keep this test running forever. The fixed id is
        // not in the queue, so `show` on it fails and the cycle moves on.
        Ok(BeadId::from(format!("span-depth-created-{title}").as_str()))
    }

    async fn add_dependency(
        &self,
        _blocker_id: &BeadId,
        _blocked_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_dependency(
        &self,
        _blocked_id: &BeadId,
        _blocker_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> anyhow::Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn doctor_check(&self) -> anyhow::Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn full_rebuild(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

// ─── Fixtures ────────────────────────────────────────────────────────────────

/// Pins the process `HOME` into the test's tempdir and restores the previous
/// value on drop — including through a panic, so a failing run can neither
/// leak this test's writes into the real home nor leave `HOME` pointing into
/// a directory that is about to be deleted.
///
/// Config pins alone are not enough: pluck writes per-workspace diagnostics
/// under `$HOME/.needle/diagnostics/` via a path that no config field
/// redirects (see the module isolation notes).
struct HomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn pin(home: &Path) -> Self {
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn make_bead(id: &str, workspace: &Path) -> Bead {
    Bead {
        id: BeadId::from(id),
        title: format!("span-depth cycle bead {id}"),
        body: Some("Drive one claim cycle for the span-depth regression.".to_string()),
        priority: 2,
        status: BeadStatus::Open,
        assignee: None,
        labels: vec![],
        workspace: workspace.to_path_buf(),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn stub_adapter(name: &str) -> AgentAdapter {
    AgentAdapter {
        name: name.to_string(),

        description: None,
        agent_cli: "bash".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: "exit 0".to_string(),
        environment: HashMap::new(),
        timeout_secs: 10,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    }
}

/// Config whose every filesystem touchpoint lives in the tempdir.
fn isolated_config(home: &Path) -> Config {
    let mut config = Config::default();
    config.agent.default = "claude-sonnet".to_string();
    // Adapter resolution and routing must see only this test's fixtures —
    // never the operator's installed adapters under $HOME/.config/needle.
    config.agent.adapters_dir = home.join("adapters");
    config.agent.routing = None;
    config.workspace.home = home.to_path_buf();
    config.workspace.default = home.to_path_buf();
    config.worker.idle_action = IdleAction::Exit;
    // boot() downgrades Exit to Wait without an opt-in (see needle-ab52a15a),
    // and this test has no supervisor to find.
    config.worker.allow_exit_without_supervisor = true;
    // The stub agent ships no commits, so the shipped-work gate would turn
    // every outcome into a release. Disabling it keeps the cycle on the plain
    // success path this test exists to measure.
    config.worker.enforce_shipped_work = false;
    config.self_modification.hot_reload = false;
    // Test isolation policy: pin the Explore scan root and disable the strand.
    config.strands.explore.enabled = false;
    config.strands.explore.workspace_root = home.to_path_buf();
    config.strands.explore.workspaces = Vec::new();
    // The telemetry file sink defaults to $HOME/.needle/logs.
    config.telemetry.file_sink.log_dir = Some(home.join("logs"));
    // Heartbeats default to $HOME/.needle/state/heartbeats — resolved from the
    // process HOME, not from workspace.home (health::resolve_heartbeat_dir), so
    // pinning the workspaces alone does not isolate them. Absolute path wins
    // over the HOME fallback.
    config.health.heartbeat_dir = Some(home.join("state/heartbeats"));
    // The claim-path circuit gate consults live CI state for the workspace;
    // this store is in-memory and the workspace is a tempdir, so the gate has
    // nothing truthful to consult and must not run here.
    config.strands.pluck.circuit_breaker.enabled = false;
    config
}

/// Print how far the run got and the tail of the capture, and write the whole
/// capture to a tempdir file for post-mortem. Runs on the `RUN_TIMEOUT` path so
/// a hang is diagnosable from CI logs without re-running anything.
fn dump_on_hang(captured: &Captured, store: &CycleStore, elapsed: &tokio::time::error::Elapsed) {
    eprintln!("=== span-depth hang diagnostics after {elapsed:?} ===");
    eprintln!(
        "claims: {} releases: {} distinct_beads: {}",
        store.claim_count(),
        store.release_count(),
        store.distinct_claimed()
    );
    let bytes = captured.bytes();
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    eprintln!("captured {} tracing lines; last 60:", lines.len());
    for line in lines.iter().rev().take(60).rev() {
        eprintln!("| {line}");
    }
    let path = std::env::temp_dir().join("span-depth-hang-capture.log");
    if std::fs::write(&path, &bytes).is_ok() {
        eprintln!("full capture written to {}", path.display());
    }
}

// ─── Test 1: depth stays O(1) across 200 claim cycles ────────────────────────

#[test]
fn claim_cycles_keep_bead_span_depth_constant() {
    // The launch-admission gate probes real host load before every selection;
    // a busy CI host would hold the worker and hang this test through no fault
    // of the span path under test. This override is read by the gate itself and
    // is inert wherever the gate does not exist.
    std::env::set_var("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK", "1");

    let home = tempfile::tempdir().expect("tempdir");
    // Declared after the tempdir so the guard drops — and restores the real
    // HOME — before the tempdir itself is removed.
    let _home_guard = HomeGuard::pin(home.path());

    let beads: Vec<Bead> = (0..CLAIM_CYCLES)
        .map(|i| make_bead(&format!("needle-span-depth-{i:04}"), home.path()))
        .collect();
    let store = Arc::new(CycleStore::new(beads));

    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    // The state machine is polled on the runtime's worker threads, so the
    // capturing subscriber must be the process default — a thread-local
    // `with_default` on the test thread would never see worker-thread events.
    // This file is its own test binary, so the global slot is ours alone.
    tracing::subscriber::set_global_default(subscriber)
        .expect("capture subscriber must be installed before the worker runs");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");

    runtime.block_on(async {
        let mut worker = Worker::new(
            isolated_config(home.path()),
            "span-depth".to_string(),
            store.clone() as Arc<dyn BeadStore>,
        );
        let mut adapters = HashMap::new();
        adapters.insert("claude-sonnet".to_string(), stub_adapter("claude-sonnet"));
        worker.set_dispatcher(Dispatcher::with_adapters(
            adapters,
            Telemetry::new("span-depth".to_string()),
            10,
        ));

        // Spawned, not block_on'd: the state machine must be free to migrate
        // between worker threads exactly as it does in production. This is
        // also a compile-time guard — a guard-lifetime regression that stores
        // an `EnteredSpan` in the Worker makes this future !Send and the
        // test stops building.
        let handle = tokio::spawn(async move { worker.run().await });

        let final_state = match tokio::time::timeout(RUN_TIMEOUT, handle).await {
            Ok(joined) => joined
                .expect("worker task panicked")
                .expect("worker run failed"),
            Err(elapsed) => {
                // A hang must be diagnosable from the CI log alone: how far the
                // run got, and the last things the worker said.
                dump_on_hang(&captured, &store, &elapsed);
                panic!("worker never reached a terminal state; span-depth run hung: {elapsed:?}");
            }
        };
        // Exit on a drained queue runs the EXHAUSTED → stop() path, whose
        // return value is Stopped (see the worker's own
        // `handle_exhausted_with_exit_returns_stopped`); Exhausted is the
        // mid-machine state, not what run() returns here.
        assert!(
            matches!(final_state, WorkerState::Stopped),
            "worker should stop cleanly after draining the queue, got {final_state:?}"
        );
    });

    // The test really did drive 200 independent claim cycles, and each cycle
    // ran to completion: a claim that ends in a release returns its bead to
    // the ready queue, so any release here means the worker re-claimed beads
    // instead of draining the queue one bead per cycle.
    assert_eq!(
        store.claim_count(),
        CLAIM_CYCLES,
        "expected exactly {CLAIM_CYCLES} claim attempts"
    );
    assert_eq!(
        store.distinct_claimed(),
        CLAIM_CYCLES,
        "each cycle must claim a distinct bead; a repeat means a cycle released its bead and looped"
    );
    assert_eq!(
        store.release_count(),
        0,
        "a clean cycle closes its bead; a release means the outcome path failed and the bead re-entered the queue"
    );

    // The depth probe saw both spans under test, so the bounds below say
    // something rather than passing on an empty capture.
    assert!(
        saw_span(&captured, "bead.claim"),
        "no captured line rendered bead.claim — the depth bound would be vacuous"
    );
    assert!(
        saw_span(&captured, "bead.lifecycle"),
        "no captured line rendered bead.lifecycle — the depth bound would be vacuous"
    );

    // The actual invariant: span depth is bounded by a constant that does not
    // depend on the cycle count. One leaked entry per cycle would push these
    // to CLAIM_CYCLES.
    let claim_depth = max_span_depth(&captured, "bead.claim");
    assert!(
        claim_depth <= 1,
        "bead.claim depth grew with cycle count: observed depth {claim_depth} on a single line (bf-3uj6i leaked one entry per cycle)"
    );
    let lifecycle_depth = max_span_depth(&captured, "bead.lifecycle");
    assert!(
        lifecycle_depth <= 1,
        "bead.lifecycle depth grew with cycle count: observed depth {lifecycle_depth} on a single line (bf-3uj6i leaked one entry per cycle)"
    );

    // Parent-bead acceptance criterion: line length stays bounded across a
    // long run. Depth growth is what made lines grow to 629,829 bytes.
    let longest = max_line_bytes(&captured);
    assert!(
        longest < DEFAULT_MAX_LINE_BYTES,
        "formatted line reached {longest} bytes, past the {DEFAULT_MAX_LINE_BYTES}-byte production cap"
    );
}

// ─── Test 2: the probe is not vacuous ────────────────────────────────────────

/// The main test's assertion is only worth something if the depth probe
/// actually reports leaked entries. This test manufactures the exact residue a
/// leaked guard leaves — an entry pushed onto the thread's span stack and never
/// popped, one per cycle — and asserts the probe reports linear growth, then
/// asserts normally-unwound spans report a constant depth of one.
#[test]
fn depth_probe_detects_leaked_span_entries() {
    const CYCLES: usize = CLAIM_CYCLES;

    let captured = Captured::default();
    let writer = captured.clone();
    // Arc so the probe can drive `enter`/`exit` on the same subscriber the
    // dispatcher uses; `Subscriber` is implemented for `Arc<S>`.
    let subscriber = Arc::new(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish(),
    );

    tracing::subscriber::with_default(Arc::clone(&subscriber), || {
        // Every handle is kept alive: dropping a span whose entry is still on
        // the stack closes it and lets the subscriber pop, which is exactly
        // the cleanup a leaked guard never got to do.
        let mut keepalive = Vec::with_capacity(CYCLES);
        for cycle in 0..CYCLES {
            let span = tracing::info_span!("bead.claim", needle.bead.id = %cycle);
            let id = span
                .id()
                .expect("span has an id under an active subscriber");
            keepalive.push((span, id.clone()));

            // Push and never pop — the residue of a guard whose Drop ran
            // somewhere this entry was not pushed (cross-thread drop of a
            // guard held across an await, the bf-3uj6i mechanism).
            tracing::Subscriber::enter(&subscriber, &id);

            tracing::info!(cycle, "span-depth-leak-probe");
        }
    });

    let lines: Vec<String> = String::from_utf8_lossy(&captured.bytes())
        .lines()
        .filter(|line| line.contains("span-depth-leak-probe"))
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines.len(),
        CYCLES,
        "one probe line per cycle is required to measure growth"
    );

    let depth_on = |line: &str| line.matches("bead.claim{").count();
    assert_eq!(
        depth_on(&lines[0]),
        1,
        "the first cycle should show exactly the one live span"
    );
    assert_eq!(
        depth_on(lines.last().expect("non-empty")),
        CYCLES,
        "one leaked entry per cycle must accumulate: the probe must see depth {CYCLES} by the last cycle, or the main test's O(1) assertion cannot fail against a leak"
    );
    // Strictly growing — the signature of O(N) depth.
    for (earlier, late) in lines.iter().zip(lines.iter().skip(1)) {
        assert!(
            depth_on(late) > depth_on(earlier),
            "probe depth must grow with every leaked entry: {} then {}",
            depth_on(earlier),
            depth_on(late)
        );
    }
    assert!(
        max_line_bytes(&captured) > 0,
        "leaked entries must be visible as growing lines"
    );

    // And the benign control: spans that enter and exit normally — what the
    // fixed claim path does — keep the probe pinned at depth one.
    let clean = Captured::default();
    let clean_writer = clean.clone();
    let clean_subscriber = Arc::new(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || clean_writer.clone())
            .finish(),
    );
    tracing::subscriber::with_default(Arc::clone(&clean_subscriber), || {
        for cycle in 0..CYCLES {
            let span = tracing::info_span!("bead.claim", needle.bead.id = %cycle);
            let id = span
                .id()
                .expect("span has an id under an active subscriber");
            // The probe event fires while the span is current, mirroring how
            // the main test's assertion reads depth off lines emitted inside
            // live spans.
            tracing::Subscriber::enter(&clean_subscriber, &id);
            tracing::info!(cycle, "span-depth-clean-probe");
            tracing::Subscriber::exit(&clean_subscriber, &id);
        }
    });
    let clean_lines: Vec<String> = String::from_utf8_lossy(&clean.bytes())
        .lines()
        .filter(|line| line.contains("span-depth-clean-probe"))
        .map(str::to_string)
        .collect();
    assert_eq!(clean_lines.len(), CYCLES);
    for line in &clean_lines {
        assert_eq!(
            depth_on(line),
            1,
            "normally-unwound spans must not accumulate: {line}"
        );
    }
}
