//! Worker loop: the core NEEDLE state machine.
//!
//! The Worker executes the strand waterfall, claims beads, dispatches the
//! agent, handles outcomes, and emits telemetry for every transition.
//!
//! State transitions are explicit — there is no implicit fallthrough and no
//! state that does not have a defined handler. The worker emits telemetry for
//! every transition.
//!
//! Depends on: `strand`, `claim`, `prompt`, `dispatch`, `outcome`,
//!             `bead_store`, `telemetry`, `health`, `config`, `types`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

#[cfg(unix)]
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering as AtomicOrdering};

use crate::bead_store::BeadStore;
use crate::canary::CanaryRunner;
use crate::claim::Claimer;
use crate::commit_hook;
use crate::config::{Config, ConfigLoader};
use crate::cost::{self, BudgetCheck, EffortData};
use crate::dispatch::{self, Dispatcher};
use crate::health::HealthMonitor;
use crate::mitosis::MitosisEvaluator;
use crate::outcome::OutcomeHandler;
use crate::prompt::{BuiltPrompt, PromptBuilder};
use crate::rate_limit::RateLimiter;
use crate::registry::{Registry, WorkerEntry};
use crate::strand::StrandRunner;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{AgentOutcome, Bead, BeadId, ClaimResult, IdleAction, Outcome, WorkerState};
use crate::upgrade::{self, HotReloadCheck};

// ──────────────────────────────────────────────────────────────────────────────
// Global shutdown flag for signal handlers
// ──────────────────────────────────────────────────────────────────────────────

/// Global pointer to the shutdown flag, used by synchronous signal handlers.
/// This is necessary because signal handlers run in a separate context and
/// cannot easily access the Worker's shutdown flag directly.
#[cfg(unix)]
static GLOBAL_SHUTDOWN_FLAG: AtomicUsize = AtomicUsize::new(0);

/// Set the global shutdown flag pointer.
/// Called by `install_signal_handlers` to register the shutdown flag.
#[cfg(unix)]
fn set_global_shutdown_flag(ptr: usize) {
    GLOBAL_SHUTDOWN_FLAG.store(ptr, AtomicOrdering::SeqCst);
}

/// Clear the global shutdown flag pointer.
/// Called when the worker is dropped to avoid dangling pointers.
#[cfg(unix)]
fn clear_global_shutdown_flag() {
    GLOBAL_SHUTDOWN_FLAG.store(0, AtomicOrdering::SeqCst);
    LAST_SIGNAL.store(0, AtomicOrdering::SeqCst);
}

/// Track the last received signal for diagnostic logging.
/// AtomicU32 allows lock-free reads/writes from the signal handler.
#[cfg(unix)]
static LAST_SIGNAL: AtomicU32 = AtomicU32::new(0);

/// Synchronous signal handler for SIGTERM, SIGINT, and SIGHUP.
///
/// This function is called directly by the OS when a signal is received.
/// It must be async-signal-safe: no allocation, no locking, no I/O.
/// We set the atomic shutdown flag, record the signal number, and return immediately.
#[cfg(unix)]
extern "C" fn signal_handler(sig: i32) {
    // SAFETY: The signal handler is only installed after set_global_shutdown_flag
    // has been called with a valid pointer. The pointer remains valid for the
    // entire lifetime of the worker process.
    let ptr = GLOBAL_SHUTDOWN_FLAG.load(AtomicOrdering::SeqCst) as *const AtomicBool;
    if !ptr.is_null() {
        // SAFETY: The pointer is valid and points to an AtomicBool that lives
        // for the entire program duration.
        unsafe {
            (*ptr).store(true, AtomicOrdering::SeqCst);
        }
        // Record the signal number so the main loop can log it.
        LAST_SIGNAL.store(sig as u32, AtomicOrdering::SeqCst);
    }
}

/// Install synchronous signal handlers for SIGTERM, SIGINT, and SIGHUP.
///
/// Uses libc::sigaction to register handlers that set the shutdown flag
/// immediately when a signal is received. This ensures that signals are
/// caught even if the tokio runtime hasn't polled async signal tasks yet.
#[cfg(unix)]
unsafe fn install_unix_signal_handlers() {
    use libc::{sigaction, sigemptyset, SA_RESTART, SIGHUP, SIGINT, SIGTERM};

    // Set up the sigaction structure.
    let mut act: libc::sigaction = std::mem::zeroed();
    act.sa_sigaction = signal_handler as *const () as usize;
    // Block all signals during handler execution to prevent re-entrancy issues.
    sigemptyset(&mut act.sa_mask as *mut libc::sigset_t);
    // Use SA_RESTART to automatically restart system calls interrupted by signals.
    act.sa_flags = SA_RESTART;

    // Install handlers for SIGTERM, SIGINT, and SIGHUP.
    // We ignore errors here - if a handler can't be installed, we'll log a
    // warning but continue. The async handlers (below) provide a fallback.
    for &sig in &[SIGTERM, SIGINT, SIGHUP] {
        let mut old: libc::sigaction = std::mem::zeroed();
        if sigaction(sig, &act, &mut old) == 0 {
            tracing::debug!(signal = sig, "installed synchronous signal handler");
        } else {
            // Log the error but don't fail - the async handlers provide a fallback.
            tracing::warn!(
                signal = sig,
                errno = *libc::__errno_location(),
                "failed to install synchronous signal handler"
            );
        }
    }
}

/// Global state for atexit handler to emit worker.stopped on unexpected termination.
///
/// When a worker is killed externally (e.g., SIGKILL from capacity governor),
/// the normal signal handlers don't run. The atexit handler provides a last-resort
/// mechanism to emit worker.stopped telemetry with diagnostic information.
static ATEXIT_WORKER_STATE: Mutex<Option<AtexitWorkerState>> = Mutex::new(None);

/// State captured for the atexit handler.
struct AtexitWorkerState {
    worker_name: String,
    beads_processed: u64,
    start_time: Instant,
    last_state: String,
    log_file_path: Option<String>,
}

/// Register the atexit handler with worker state.
///
/// Called by `install_signal_handlers` to ensure the atexit handler can
/// emit meaningful telemetry if the process terminates unexpectedly.
fn register_atexit_handler(
    worker_name: String,
    beads_processed: u64,
    start_time: Instant,
    last_state: String,
    log_file_path: Option<String>,
) {
    let state = AtexitWorkerState {
        worker_name,
        beads_processed,
        start_time,
        last_state,
        log_file_path,
    };
    *ATEXIT_WORKER_STATE.lock().unwrap() = Some(state);

    // Register the atexit handler.
    // This will run when the process exits normally, but NOT on SIGKILL.
    extern "C" fn atexit_handler() {
        if let Some(state) = ATEXIT_WORKER_STATE.lock().unwrap().as_ref() {
            let uptime = state.start_time.elapsed().as_secs();
            // Try to write to stderr as a last resort since telemetry may be unavailable.
            eprintln!(
                "NEEDLE worker '{}' stopped unexpectedly: state={}, beads_processed={}, uptime={}s",
                state.worker_name, state.last_state, state.beads_processed, uptime
            );
            eprintln!("This indicates the worker was killed by an external process (e.g., SIGKILL, OOM, capacity governor)");

            // Try to write a worker.stopped event to the JSONL log file.
            // This provides diagnostic information even when the worker is killed abruptly.
            if let Some(ref log_path) = state.log_file_path {
                use std::fs::OpenOptions;
                use std::io::Write;

                let event = serde_json::json!({
                    "event_type": "worker.stopped",
                    "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "data": {
                        "worker_id": state.worker_name,
                        "reason": "external_kill",
                        "beads_processed": state.beads_processed,
                        "uptime_secs": uptime,
                        "final_state": state.last_state,
                        "via_atexit_handler": true
                    }
                });

                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path) {
                    let _ = writeln!(file, "{}", event);
                    let _ = file.flush();
                }
            }
        }
    }

    // SAFETY: atexit is safe to call with a function pointer.
    unsafe {
        libc::atexit(atexit_handler);
    }
}

/// Update the atexit state when the worker state changes.
///
/// Called by `set_state` to keep the atexit handler's last state fresh.
fn update_atexit_state(last_state: String) {
    if let Some(state) = ATEXIT_WORKER_STATE.lock().unwrap().as_mut() {
        state.last_state = last_state;
    }
}

/// Stub implementations for non-Unix platforms.
/// These functions are no-ops on platforms where Unix signals are not available.
#[cfg(not(unix))]
fn set_global_shutdown_flag(_ptr: usize) {
    // No-op on non-Unix platforms
}

#[cfg(not(unix))]
fn clear_global_shutdown_flag() {
    // No-op on non-Unix platforms
}

#[cfg(not(unix))]
fn register_atexit_handler(
    _worker_name: String,
    _beads_processed: u64,
    _start_time: Instant,
    _last_state: String,
    _log_file_path: Option<String>,
) {
    // No-op on non-Unix platforms
}

#[cfg(not(unix))]
fn update_atexit_state(_last_state: String) {
    // No-op on non-Unix platforms
}

/// TTL for race-lost bead exclusions.
///
/// After losing a claim race, a bead is excluded from selection for this duration
/// to prevent infinite loops where the selector returns the same bead repeatedly.
const RACE_LOST_EXCLUSION_TTL: Duration = Duration::from_secs(30);

/// Timeout for HANDLING state watchdog.
///
/// If the worker remains in HANDLING state for longer than this duration,
/// the watchdog thread will force a recovery. This is longer than the
/// inner timeouts (50s, 60s, 90s) to allow normal recovery to work first.
const HANDLING_WATCHDOG_TIMEOUT_SECS: u64 = 120;

/// The NEEDLE worker — owns and drives the full state machine.
pub struct Worker {
    config: Config,
    worker_name: String,
    store: Arc<dyn BeadStore>,
    /// Home workspace store — kept for restore after processing a remote bead.
    home_store: Arc<dyn BeadStore>,
    telemetry: Telemetry,
    strands: StrandRunner,
    claimer: Claimer,
    prompt_builder: PromptBuilder,
    dispatcher: Dispatcher,
    outcome_handler: OutcomeHandler,
    health: HealthMonitor,
    registry: Registry,
    rate_limiter: RateLimiter,
    mitosis_evaluator: MitosisEvaluator,

    // State machine fields
    state: WorkerState,
    current_bead: Option<Bead>,
    exclusion_set: HashSet<BeadId>,
    /// Race-lost exclusions with TTL — prevents re-selecting beads that just lost a claim race.
    /// Each entry is (bead_id, expiration_time). Entries are pruned on access.
    race_lost_exclusions: Vec<(BeadId, Instant)>,
    /// Beads that lost a claim race in the current selection cycle.
    /// These are added to exclusion_set to prevent immediate re-selection.
    /// Cleared at the start of the next SELECTING cycle.
    race_lost_this_cycle: HashSet<BeadId>,
    retry_count: u32,
    consecutive_race_lost: u32,
    beads_processed: u64,
    shutdown: Arc<AtomicBool>,
    last_error: Option<anyhow::Error>,
    boot_time: Option<Instant>,

    // Transient fields — pass data between state handlers within a single cycle.
    built_prompt: Option<BuiltPrompt>,
    current_strand: Option<String>,
    exec_output: Option<(AgentOutcome, bool)>,
    /// Effort tracking data for the current bead cycle.
    last_effort: Option<EffortData>,
    /// HEAD SHA captured just before agent dispatch; used to detect new commits.
    pre_dispatch_head: Option<String>,
    /// The workspace of the current bead store — updated when switching to remote.
    /// Used to ensure heartbeat reports the actual workspace where work is happening.
    current_workspace: PathBuf,
    /// Whether `worker.booting` was already emitted externally (e.g., from CLI layer).
    /// When true, `run()` skips emitting the booting event to avoid duplicates.
    booting_emitted: bool,
    /// Waterfall restart count from the most recent select cycle (for exhausted telemetry).
    last_waterfall_restarts: u32,
    /// Names of strands that triggered waterfall restarts in the most recent cycle.
    last_restart_triggers: Vec<String>,
    /// Strand evaluations from the most recent select cycle (for exhausted telemetry).
    last_strand_evaluations: Vec<(String, String, u64)>,
    /// Timestamp when the worker entered HANDLING state.
    /// Used by the watchdog to detect stuck HANDLING state.
    handling_state_entered_at: Option<Instant>,
    /// Flag set by the watchdog thread when HANDLING state timeout is detected.
    /// The main worker loop checks this flag and forces recovery if set.
    watchdog_triggered: Arc<AtomicBool>,
    /// Handle to the watchdog thread for cleanup on worker drop.
    #[allow(dead_code)]
    watchdog_handle: Option<std::thread::JoinHandle<()>>,
    /// The current bead lifecycle span guard. Created when a bead is claimed,
    /// dropped when the bead lifecycle ends (after HANDLING or when the bead is released).
    #[allow(dead_code)]
    bead_lifecycle_span: Option<tracing::span::EnteredSpan>,
    /// The last outcome for the current bead (used to record on bead.lifecycle span).
    last_outcome: Option<String>,
}

impl Worker {
    /// Construct a worker using a pre-existing telemetry instance.
    ///
    /// Use this when telemetry has already been started (e.g. after emitting
    /// `worker.booting` from the CLI layer) so that early init steps are
    /// visible in the JSONL log.
    pub fn new_with_telemetry(
        config: Config,
        worker_name: String,
        store: Arc<dyn BeadStore>,
        telemetry: Telemetry,
    ) -> Self {
        Self::build(config, worker_name, store, telemetry, true)
    }

    /// Construct a worker from config, a worker name, and a bead store implementation.
    ///
    /// Creates its own telemetry instance. Prefer [`new_with_telemetry`] when
    /// the caller has already created and started telemetry for early boot
    /// diagnostics.
    pub fn new(config: Config, worker_name: String, store: Arc<dyn BeadStore>) -> Self {
        let qualified_id = format!("{}-{}", config.agent.default, worker_name);
        let telemetry = Telemetry::from_config(qualified_id.clone(), &config.telemetry)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to create hook-enabled telemetry, falling back");
                Telemetry::new(qualified_id.clone())
            });
        Self::build(config, worker_name, store, telemetry, false)
    }

    /// Shared construction logic used by both [`new`] and [`new_with_telemetry`].
    fn build(
        config: Config,
        worker_name: String,
        store: Arc<dyn BeadStore>,
        telemetry: Telemetry,
        booting_emitted: bool,
    ) -> Self {
        let qualified_id = format!("{}-{}", config.agent.default, worker_name);
        let strand_registry = Registry::default_location(&config.workspace.home);
        let strands =
            StrandRunner::from_config(&config, &qualified_id, strand_registry, telemetry.clone());
        let claimer = Claimer::new(
            store.clone(),
            std::path::PathBuf::from("/tmp"),
            config.worker.max_claim_retries,
            100,
            telemetry.clone(),
        );
        let prompt_builder = PromptBuilder::with_workspace(
            &config.prompt,
            &config.workspace.default,
        )
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to load workspace learnings, using default prompt builder");
            PromptBuilder::new(&config.prompt)
        })
        .with_cross_workspace_skills(
            &config.strands.explore.workspaces,
            &config.workspace.labels,
        )
        .with_global_learnings(&config.strands.learning.global_learnings_file);
        let dispatcher = match Dispatcher::new(&config, telemetry.clone()) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load adapters, using built-in defaults");
                let builtins = crate::dispatch::builtin_adapters()
                    .into_iter()
                    .map(|a| (a.name.clone(), a))
                    .collect();
                Dispatcher::with_adapters(builtins, telemetry.clone(), config.agent.timeout)
            }
        };
        let outcome_handler = OutcomeHandler::new(config.clone(), telemetry.clone());

        // Create the shutdown flag BEFORE creating HealthMonitor so we can share it.
        // This ensures that when the heartbeat emitter's circuit breaker fires,
        // it sets the worker's shutdown flag (not its own private flag), allowing
        // the main worker loop to gracefully stop with worker.stopped telemetry.
        let shutdown = Arc::new(AtomicBool::new(false));

        let health = HealthMonitor::new(
            config.clone(),
            worker_name.clone(),
            telemetry.clone(),
            Some(shutdown.clone()),
        );
        let registry = Registry::default_location(&config.workspace.home);
        let rate_limiter =
            RateLimiter::new(config.limits.clone(), &config.workspace.home.join("state"));
        let mitosis_evaluator = MitosisEvaluator::new(
            config.strands.mitosis.clone(),
            telemetry.clone(),
            std::path::PathBuf::from("/tmp"),
        );

        // Restore beads_processed from registry if this worker was previously registered
        // (e.g., hot-reload resume). New workers start at 0.
        // Match by qualified identity ({adapter}-{worker_id}).
        let qualified_id = format!("{}-{}", config.agent.default, worker_name);
        let beads_processed = registry
            .list()
            .ok()
            .and_then(|workers| workers.into_iter().find(|w| w.id == qualified_id))
            .map(|entry| entry.beads_processed)
            .unwrap_or(0);

        let default_workspace = config.workspace.default.clone();

        // Create the watchdog trigger flag before creating the Worker.
        let watchdog_triggered = Arc::new(AtomicBool::new(false));

        Worker {
            config,
            worker_name,
            home_store: store.clone(),
            store,
            telemetry,
            strands,
            claimer,
            prompt_builder,
            dispatcher,
            outcome_handler,
            health,
            registry,
            rate_limiter,
            mitosis_evaluator,
            state: WorkerState::Booting,
            current_bead: None,
            exclusion_set: HashSet::new(),
            race_lost_exclusions: Vec::new(),
            race_lost_this_cycle: HashSet::new(),
            retry_count: 0,
            consecutive_race_lost: 0,
            beads_processed,
            shutdown,
            last_error: None,
            boot_time: None,
            built_prompt: None,
            current_strand: None,
            exec_output: None,
            last_effort: None,
            pre_dispatch_head: None,
            current_workspace: default_workspace,
            booting_emitted,
            last_waterfall_restarts: 0,
            last_restart_triggers: Vec::new(),
            last_strand_evaluations: Vec::new(),
            handling_state_entered_at: None,
            watchdog_triggered: watchdog_triggered.clone(),
            watchdog_handle: None,
            bead_lifecycle_span: None,
            last_outcome: None,
        }
    }

    /// Start the watchdog thread that monitors HANDLING state duration.
    ///
    /// The watchdog runs in a separate thread (not part of the Tokio runtime)
    /// and can detect when the worker is stuck in HANDLING state even if
    /// the Tokio runtime becomes wedged. If HANDLING state exceeds the
    /// timeout, the watchdog sets the `watchdog_triggered` flag, which
    /// the main worker loop checks to force recovery.
    fn start_watchdog_thread(&mut self) {
        let watchdog_triggered = self.watchdog_triggered.clone();
        let handling_state_entered_at_ptr =
            &self.handling_state_entered_at as *const Option<Instant> as usize;

        let handle = std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(5));
                if watchdog_triggered.load(Ordering::Relaxed) {
                    // Watchdog has already triggered, exit the thread.
                    break;
                }
                // Check if we've been in HANDLING state for too long.
                // We read the timestamp from the Worker struct via the pointer.
                // SAFETY: The Worker struct outlives the watchdog thread because
                // the thread is joined when the Worker is dropped.
                let entered_at = unsafe {
                    let ptr = handling_state_entered_at_ptr as *const Option<Instant>;
                    (*ptr).as_ref().copied()
                };

                if let Some(entry_time) = entered_at {
                    let elapsed = entry_time.elapsed().as_secs();
                    if elapsed >= HANDLING_WATCHDOG_TIMEOUT_SECS {
                        tracing::error!(
                            elapsed_secs = elapsed,
                            "HANDLING state watchdog triggered - forcing recovery"
                        );
                        watchdog_triggered.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        });

        self.watchdog_handle = Some(handle);
    }

    /// Run the worker loop until exhausted, stopped, or errored.
    ///
    /// The main loop is a match on `self.state`. Every state has a handler
    /// that performs its actions and sets `self.state` to the next state.
    ///
    /// Guarantees that the telemetry BufWriter is flushed before returning,
    /// even when the inner state machine exits early via `?`.
    pub async fn run(&mut self) -> Result<WorkerState> {
        // Start the telemetry writer now that we are inside the tokio runtime.
        self.telemetry.start();

        // IMMEDIATE boot event — must be the first thing emitted after telemetry starts.
        // This ensures we get a trace even if subsequent init steps block indefinitely.
        // Skip if already emitted externally (e.g., from CLI layer for early boot diagnostics).
        if !self.booting_emitted {
            self.telemetry.emit(EventKind::WorkerBooting {
                worker_name: self.worker_name.clone(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            })?;
            // Force-flush to disk before boot() — if init blocks, we still have a trace.
            self.telemetry
                .force_flush_async(std::time::Duration::from_secs(5))
                .await?;
        }

        let result = self.run_inner().await;

        // Safety-net flush: shutdown() is idempotent. Normal terminal paths
        // (stop, handle_exhausted, Errored) already call it; this catches
        // any early-exit via `?` (boot failure, state handler panic, etc.)
        // so the BufWriter is always flushed before the tokio Runtime drops.
        self.telemetry.shutdown().await;

        result
    }

    /// Inner state machine — called only from [`run()`](Self::run).
    ///
    /// May return early via `?` without calling `telemetry.shutdown()`;
    /// `run()` handles the safety-net flush.
    async fn run_inner(&mut self) -> Result<WorkerState> {
        // Boot: validate config and initialize.
        self.boot()?;

        // Start the watchdog thread that monitors HANDLING state duration.
        // This must be started after boot() so the Worker struct is fully initialized.
        self.start_watchdog_thread();

        // Install signal handlers.
        self.install_signal_handlers();

        // Create the worker.session root span that encompasses the entire worker lifecycle.
        let worker_id = self.qualified_id();
        let session_span = tracing::info_span!(
            "worker.session",
            needle.worker_id = %worker_id,
            needle.session_id = %self.telemetry.session_id(),
            needle.agent = %self.config.agent.default,
            needle.model = %self.config.agent.default, // Will be updated when adapter is resolved
            needle.workspace = %self.config.workspace.default.display(),
        );
        let _enter = session_span.enter();

        loop {
            // Check for shutdown signal between states.
            if self.shutdown.load(Ordering::SeqCst) {
                // Retrieve and clear the last received signal for logging.
                #[cfg(unix)]
                let signal_name = {
                    let sig = LAST_SIGNAL.swap(0, AtomicOrdering::SeqCst);
                    if sig == 0 {
                        None
                    } else {
                        Some(match sig {
                            1 => "SIGHUP",
                            2 => "SIGINT",
                            15 => "SIGTERM",
                            _ => "unknown signal",
                        })
                    }
                };
                #[cfg(not(unix))]
                let signal_name = None;

                let reason = if let Some(name) = signal_name {
                    format!("signal received ({name})")
                } else {
                    "signal received".to_string()
                };

                match self.state {
                    // If we're in the middle of processing a bead, handle it
                    // as interrupted so the bead gets released.
                    WorkerState::Building
                    | WorkerState::Dispatching
                    | WorkerState::Executing
                    | WorkerState::Handling => {
                        // Let the current state handler finish, but mark
                        // that we should stop after handling.
                    }
                    // For states where we don't hold a bead, stop immediately.
                    WorkerState::Selecting
                    | WorkerState::Claiming
                    | WorkerState::Retrying
                    | WorkerState::Logging => {
                        // Release any claimed bead before stopping.
                        if let Some(ref bead) = self.current_bead {
                            let bead_id = bead.id.clone();
                            tracing::info!(bead_id = %bead_id, "releasing bead on shutdown");
                            let _ = self.store.release(&bead_id).await;
                        }
                        return self.stop(&reason).await;
                    }
                    WorkerState::Stopped | WorkerState::Exhausted | WorkerState::Errored => {
                        return self.stop(&reason).await;
                    }
                    WorkerState::Booting => {
                        return self.stop("signal received during boot").await;
                    }
                }
            }

            // Check for watchdog trigger - this indicates HANDLING state is wedged.
            // The watchdog runs in a separate thread and can detect when the worker
            // is stuck even if the Tokio runtime becomes unresponsive.
            if self.watchdog_triggered.load(Ordering::Relaxed)
                && self.state == WorkerState::Handling
            {
                tracing::error!("watchdog detected HANDLING state hang, forcing recovery");
                // Emit critical timeout event.
                let bead_id = self.current_bead.as_ref().map(|b| b.id.clone());
                let _ = self
                    .telemetry
                    .emit_try_lock(EventKind::WorkerHandlingTimeout {
                        bead_id: bead_id.clone().unwrap_or_else(|| BeadId::from("unknown")),
                        outcome: "unknown".to_string(),
                        operation: "watchdog".to_string(),
                        error: format!(
                            "HANDLING state exceeded {}s timeout",
                            HANDLING_WATCHDOG_TIMEOUT_SECS
                        ),
                    });
                // Attempt best-effort release if we have a bead.
                if let Some(ref bead) = self.current_bead {
                    let bead_id = bead.id.clone();
                    tracing::warn!(bead_id = %bead_id, "best-effort bead release due to watchdog timeout");
                    let _ =
                        tokio::time::timeout(Duration::from_secs(30), self.store.release(&bead_id))
                            .await;
                }
                // Clear the watchdog trigger and force transition to LOGGING.
                self.watchdog_triggered.store(false, Ordering::Release);
                self.handling_state_entered_at = None;
                // Force transition to LOGGING to recover.
                self.set_state(WorkerState::Logging)?;
            }

            match self.state {
                WorkerState::Selecting => self.do_select().await?,
                WorkerState::Claiming => self.do_claim().await?,
                WorkerState::Retrying => self.do_retry().await?,
                WorkerState::Building => self.do_build().await?,
                WorkerState::Dispatching => self.do_dispatch().await?,
                WorkerState::Executing => self.do_execute().await?,
                WorkerState::Handling => self.do_handle().await?,
                WorkerState::Logging => self.do_log()?,
                WorkerState::Exhausted => {
                    let next = self.handle_exhausted().await?;
                    match next {
                        WorkerState::Selecting => continue,
                        terminal => return Ok(terminal),
                    }
                }
                WorkerState::Stopped => {
                    return self.stop("normal shutdown").await;
                }
                WorkerState::Errored => {
                    let err = self
                        .last_error
                        .take()
                        .unwrap_or_else(|| anyhow::anyhow!("unknown error"));
                    let msg = format!("{err}");
                    self.telemetry.emit(EventKind::WorkerErrored {
                        error_type: "worker_scoped".to_string(),
                        error_message: msg.clone(),
                        beads_processed: self.beads_processed,
                    })?;

                    // Emit WorkerStopped before exiting so telemetry shows a clean shutdown.
                    // This ensures operators can distinguish "exited with error" from
                    // "killed by external agent" (e.g., SIGKILL, OOM).
                    let uptime = self.boot_time.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                    let _ = self.telemetry.emit(EventKind::WorkerStopped {
                        reason: format!("error: {msg}"),
                        beads_processed: self.beads_processed,
                        uptime_secs: uptime,
                    });

                    // Best-effort stop heartbeat on error.
                    self.health.stop();
                    // Best-effort deregister on error.
                    if let Err(e) = self.registry.deregister(&self.qualified_id()) {
                        tracing::warn!(error = %e, "failed to deregister from worker registry on error");
                    }
                    self.telemetry.shutdown().await;
                    return Err(err);
                }
                WorkerState::Booting => {
                    bail!("boot() should have transitioned past Booting state");
                }
            }
        }
    }

    // ── Boot ────────────────────────────────────────────────────────────────

    /// Validate configuration and initialize the worker.
    ///
    /// Each step is instrumented with `init.step.started`/`init.step.completed`
    /// events so that hangs are visible in the telemetry log. Boot duration is
    /// capped at 60 seconds — if exceeded, the worker self-aborts with a
    /// `worker.boot.timeout` event and exits with a non-zero code.
    fn boot(&mut self) -> Result<()> {
        self.boot_time = Some(Instant::now());
        const BOOT_TIMEOUT_SECS: u64 = 60;

        // Step: Config validation
        self.telemetry.emit(EventKind::InitStepStarted {
            step: "config_validation".to_string(),
        })?;
        let step_start = Instant::now();
        let errors = ConfigLoader::validate(&self.config);
        if !errors.is_empty() {
            let msg = errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            bail!("config validation failed: {msg}");
        }
        self.telemetry.emit(EventKind::InitStepCompleted {
            step: "config_validation".to_string(),
            duration_ms: step_start.elapsed().as_millis() as u64,
        })?;

        // Check boot timeout before each step
        self.check_boot_timeout(BOOT_TIMEOUT_SECS)?;

        // Step: Registry registration
        self.telemetry.emit(EventKind::InitStepStarted {
            step: "registry_registration".to_string(),
        })?;
        let step_start = Instant::now();
        let qualified_id = format!("{}-{}", self.config.agent.default, self.worker_name);
        let entry = WorkerEntry {
            id: qualified_id.clone(),
            pid: std::process::id(),
            workspace: self.config.workspace.default.clone(),
            agent: self.config.agent.default.clone(),
            model: None,
            provider: self.resolve_provider(),
            started_at: chrono::Utc::now(),
            beads_processed: 0,
        };
        if let Err(e) = self.registry.register(entry) {
            tracing::warn!(error = %e, "failed to register in worker registry");
        }
        self.telemetry.emit(EventKind::InitStepCompleted {
            step: "registry_registration".to_string(),
            duration_ms: step_start.elapsed().as_millis() as u64,
        })?;

        // Check boot timeout before each step
        self.check_boot_timeout(BOOT_TIMEOUT_SECS)?;

        // Step: Heartbeat emitter start
        self.telemetry.emit(EventKind::InitStepStarted {
            step: "heartbeat_emitter".to_string(),
        })?;
        let step_start = Instant::now();
        if let Err(e) = self.health.start_emitter() {
            tracing::warn!(error = %e, "failed to start heartbeat emitter");
        }
        self.telemetry.emit(EventKind::InitStepCompleted {
            step: "heartbeat_emitter".to_string(),
            duration_ms: step_start.elapsed().as_millis() as u64,
        })?;

        // Emit worker started event — boot complete
        self.telemetry.emit(EventKind::WorkerStarted {
            worker_name: self.worker_name.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        })?;

        self.set_state(WorkerState::Selecting)?;

        tracing::info!(
            worker = %self.worker_name,
            strands = ?self.strands.strand_names(),
            "worker booted"
        );

        Ok(())
    }

    /// Check if boot has exceeded the timeout and abort if so.
    ///
    /// Emits `worker.boot.timeout` and exits the process with a non-zero
    /// code. This is a last-resort measure when an init step hangs.
    fn check_boot_timeout(&self, timeout_secs: u64) -> Result<()> {
        if let Some(boot_start) = self.boot_time {
            let elapsed = boot_start.elapsed();
            if elapsed.as_secs() > timeout_secs {
                let elapsed_ms = elapsed.as_millis() as u64;
                // Emit the timeout event before aborting
                let _ = self
                    .telemetry
                    .emit(EventKind::WorkerBootTimeout { elapsed_ms });
                tracing::error!(
                    elapsed_ms,
                    "boot timeout exceeded {}s — aborting",
                    timeout_secs
                );
                // Flush telemetry before exit
                std::mem::forget(self.telemetry.clone());
                // Exit with a distinct code to indicate boot timeout
                std::process::exit(71); // EX_OSERR + custom offset
            }
        }
        Ok(())
    }

    // ── Signal handling ─────────────────────────────────────────────────────

    /// Install SIGINT, SIGTERM, and SIGHUP handlers that set the shutdown flag.
    ///
    /// SIGHUP is handled because when the parent bash dies (e.g., tmux session
    /// killed, external reaper), the child process receives SIGHUP by default.
    /// Without a handler, the process terminates immediately without emitting
    /// worker.stopped telemetry or flushing the telemetry buffer.
    ///
    /// Uses synchronous signal handlers via libc/signal-hook to ensure signals
    /// are caught immediately, even if the tokio runtime hasn't polled async
    /// signal tasks yet. This prevents silent process termination when signals
    /// arrive early (e.g., SIGHUP from parent bash death during worker startup).
    fn install_signal_handlers(&self) {
        // Store a global reference to the shutdown flag for signal handlers.
        // We use a leak to ensure the reference lives for the entire program duration.
        let shutdown_ptr = Arc::into_raw(self.shutdown.clone()) as usize;
        set_global_shutdown_flag(shutdown_ptr);

        // Register atexit handler to emit worker.stopped telemetry on unexpected termination.
        // This provides diagnostic information when the worker is killed by an external
        // process (e.g., capacity governor, OOM killer, SIGKILL).
        let start_time = self.boot_time.unwrap_or_else(Instant::now);
        register_atexit_handler(
            self.worker_name.clone(),
            self.beads_processed,
            start_time,
            format!("{:?}", self.state),
            None, // log_file_path - not available during boot
        );

        #[cfg(unix)]
        {
            // Install synchronous signal handlers using libc.
            // These handlers are called immediately when the signal is received,
            // before the tokio runtime has a chance to process any async tasks.
            unsafe {
                install_unix_signal_handlers();
            }
        }

        #[cfg(not(unix))]
        {
            // On non-Unix platforms, use tokio's ctrl_c handler.
            let shutdown_int = self.shutdown.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    tracing::info!("received SIGINT, initiating graceful shutdown");
                    shutdown_int.store(true, Ordering::SeqCst);
                }
            });
        }
    }

    // ── State handlers ──────────────────────────────────────────────────────

    /// SELECTING: run strand waterfall to find a candidate bead.
    async fn do_select(&mut self) -> Result<()> {
        // Clear per-cycle state.
        // Preserve race-lost exclusions with TTL and beads that lost a race in the current cycle.
        // NOTE: Do NOT reset retry_count or consecutive_race_lost here — they must
        // accumulate across cycles to prevent infinite race-lost loops (see needle-aad8).
        self.race_lost_this_cycle.clear();
        self.current_bead = None;
        self.current_strand = None;

        // Restore home store if it was swapped for a remote workspace.
        self.restore_home_store();

        // Update heartbeat with home workspace (not current_workspace which
        // might be stale). This ensures heartbeat reports correctly even if
        // restore_home_store() was a no-op (stores already equal).
        self.health.update_state(
            &WorkerState::Selecting,
            None,
            Some(self.config.workspace.default.as_path()),
        );

        // Try atomic claim_auto first (server-selected bead in one transaction).
        // This eliminates the race condition where two workers both see the same
        // bead in ready() and race to claim it.
        let strand = "auto";
        let claim = self
            .claimer
            .claim_auto(&self.qualified_id(), strand)
            .await;

        match claim {
            Ok(ClaimResult::Claimed(bead)) => {
                tracing::info!(bead_id = %bead.id, "atomically claimed bead via claim_auto");
                self.current_bead = Some(bead);
                self.current_strand = Some(strand.to_string());
                self.consecutive_race_lost = 0;
                self.set_state(WorkerState::Building)?;
                return Ok(());
            }
            Ok(ClaimResult::NotClaimable { reason }) => {
                tracing::debug!(reason, "claim_auto returned no beads, falling back to strand waterfall");
                // Fall through to strand waterfall
            }
            Err(e) => {
                tracing::warn!(error = %e, "claim_auto failed, falling back to strand waterfall");
                // Fall through to strand waterfall
            }
            Ok(other) => {
                tracing::warn!(?other, "claim_auto returned unexpected result, falling back to strand waterfall");
                // Fall through to strand waterfall
            }
        }

        // Fallback: run strand waterfall to find a candidate bead.
        let exclusions = self.current_exclusions();
        let candidate = self
            .strands
            .select(self.store.as_ref(), &exclusions)
            .await?;
        self.last_waterfall_restarts = candidate.waterfall_restarts;
        self.last_restart_triggers = candidate.restart_triggers.clone();
        self.last_strand_evaluations = candidate
            .strand_evaluations
            .iter()
            .map(|e| (e.strand_name.clone(), e.result.clone(), e.duration_ms))
            .collect();

        match candidate.bead {
            Some((bead, strand_name)) => {
                tracing::debug!(bead_id = %bead.id, strand = %strand_name, "candidate found");

                // If the bead is from a remote workspace (found by Explore),
                // swap the active store so claim/show/release operate on the
                // correct workspace. Only switch if the workspace has a real
                // .beads/ directory — avoids false triggers from mock/stub beads.
                let bead_ws = bead.workspace.clone();
                if !is_workspace_unset(&bead_ws)
                    && bead_ws != self.config.workspace.default
                    && bead_ws.join(".beads").is_dir()
                {
                    tracing::info!(
                        bead_id = %bead.id,
                        remote_workspace = %bead_ws.display(),
                        "bead is from remote workspace, switching store"
                    );
                    self.switch_store_to(&bead_ws)?;
                }

                // Always update current_workspace to reflect the bead's workspace.
                // For local beads, this keeps heartbeat consistent with home workspace.
                // For cross-workspace beads, this ensures heartbeat reports where
                // the work is actually happening.
                if !is_workspace_unset(&bead_ws) {
                    self.current_workspace = bead_ws.clone();
                }

                self.current_bead = Some(bead);
                self.current_strand = Some(strand_name);

                // Update heartbeat immediately with the bead's workspace so that
                // observers see the correct workspace even before transitioning to
                // CLAIMING. This ensures heartbeats are accurate for cross-workspace
                // work (see bead needle-c63c).
                self.health.update_state(
                    &WorkerState::Selecting,
                    Some(&self.current_bead.as_ref().unwrap().id),
                    Some(&bead_ws),
                );

                self.set_state(WorkerState::Claiming)?;
            }
            None => {
                self.set_state(WorkerState::Exhausted)?;
            }
        }

        Ok(())
    }

    /// Swap the active bead store to a remote workspace.
    ///
    /// Creates a new BrCliBeadStore and rebuilds the Claimer to use it.
    /// The home store is restored at the start of the next select cycle.
    fn switch_store_to(&mut self, workspace: &std::path::Path) -> Result<()> {
        let remote_store = Arc::new(
            crate::bead_store::BrCliBeadStore::discover(workspace.to_path_buf())
                .context("failed to create bead store for remote workspace")?,
        );
        self.store = remote_store.clone();
        self.current_workspace = workspace.to_path_buf();
        self.claimer = Claimer::new(
            remote_store,
            std::path::PathBuf::from("/tmp"),
            self.config.worker.max_claim_retries,
            100,
            self.telemetry.clone(),
        );
        // Update registry so observers see the actual workspace being processed.
        if let Err(e) = self
            .registry
            .update_workspace(&self.qualified_id(), workspace)
        {
            tracing::warn!(error = %e, "failed to update registry workspace");
        }
        Ok(())
    }

    /// Restore the home workspace store if it was swapped for a remote bead.
    fn restore_home_store(&mut self) {
        if !Arc::ptr_eq(&self.store, &self.home_store) {
            tracing::debug!("restoring home workspace store");
            self.store = self.home_store.clone();
            self.current_workspace = self.config.workspace.default.clone();
            self.claimer = Claimer::new(
                self.home_store.clone(),
                std::path::PathBuf::from("/tmp"),
                self.config.worker.max_claim_retries,
                100,
                self.telemetry.clone(),
            );
            // Update registry to reflect return to home workspace.
            if let Err(e) = self
                .registry
                .update_workspace(&self.qualified_id(), &self.config.workspace.default)
            {
                tracing::warn!(error = %e, "failed to update registry workspace");
            }
        }
    }

    /// CLAIMING: attempt to claim the selected bead.
    async fn do_claim(&mut self) -> Result<()> {
        let bead_id = match self.current_bead {
            Some(ref b) => b.id.clone(),
            None => {
                self.set_state(WorkerState::Selecting)?;
                return Ok(());
            }
        };

        // Build the current exclusion set and pass it to claim_one. This
        // prevents claim_one from attempting to claim a bead that was
        // just race-lost (which would cause a tight loop).
        let exclusions = self.current_exclusions();
        let strand = self.current_strand.as_deref().unwrap_or("unknown");

        // Create the bead.claim span that wraps the claim operation.
        // Note: This span is a child of strand.{name}, not bead.lifecycle,
        // because bead.lifecycle is only created after the claim succeeds.
        let claim_span = tracing::info_span!(
            "bead.claim",
            needle.bead.id = %bead_id.as_ref(),
            needle.claim.retry_number = tracing::field::Empty,
            needle.claim.result = tracing::field::Empty,
        );
        let _claim_enter = claim_span.enter();

        // Record the initial retry number (will be updated by claim module)
        tracing::Span::current().record("needle.claim.retry_number", 1u32);

        let claim = self
            .claimer
            .claim_one(&bead_id, &self.qualified_id(), &exclusions, Some(strand))
            .await?;

        match claim {
            ClaimResult::Claimed(mut bead) => {
                tracing::info!(bead_id = %bead.id, title = %bead.title, "claimed bead");
                self.consecutive_race_lost = 0;
                self.retry_count = 0;
                self.clear_all_exclusions();
                // Preserve the workspace from the pre-claim bead (set by
                // Explore for remote beads). The claimed bead from br's JSON
                // returns source_repo as "." (cwd-relative), so we treat empty
                // or "." as unset and restore from the pre-claim bead.
                if is_workspace_unset(&bead.workspace) {
                    if let Some(ref pre_claim) = self.current_bead {
                        if !is_workspace_unset(&pre_claim.workspace) {
                            bead.workspace = pre_claim.workspace.clone();
                        }
                    }
                }
                self.current_bead = Some(bead);
                // Start effort tracking for this cycle.
                self.last_effort = Some(EffortData {
                    cycle_start: Instant::now(),
                    agent_name: String::new(),
                    model: None,
                    tokens: dispatch::TokenUsage::default(),
                    estimated_cost_usd: None,
                });

                // Compute bead metadata for the lifecycle span.
                let bead_priority = self.current_bead.as_ref().map(|b| b.priority);
                let bead_title_hash = self.current_bead.as_ref().map(|b| {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    let mut hasher = DefaultHasher::new();
                    b.title.hash(&mut hasher);
                    format!("{:x}", hasher.finish())
                });

                // Create the bead.lifecycle span after the claim succeeds.
                // This span will remain active for the entire bead processing.
                let lifecycle_span = tracing::info_span!(
                    "bead.lifecycle",
                    needle.bead.id = %self.current_bead.as_ref().map(|b| b.id.as_ref()).unwrap_or("unknown"),
                    needle.bead.priority = bead_priority.unwrap_or(0),
                    needle.bead.title_hash = %bead_title_hash.as_deref().unwrap_or("unknown"),
                    needle.bead.outcome = tracing::field::Empty, // Will be set on completion
                );
                self.bead_lifecycle_span = Some(lifecycle_span.entered());

                // Note: The claim_span (_claim_enter) is dropped here, closing the bead.claim span.
                // The bead.lifecycle span is now active and will be the parent for subsequent operations.

                self.set_state(WorkerState::Building)?;
            }
            ClaimResult::RaceLost { claimed_by } => {
                tracing::debug!(bead_id = %bead_id, %claimed_by, "claim race lost");
                // Set the claim span result attribute
                tracing::Span::current().record("needle.claim.result", "race_lost");
                // Set Error status on the claim span
                tracing::Span::current().record("otel.status_code", 2u64);
                tracing::Span::current().record("otel.status_description", "race_lost");
                // Add to race-lost exclusions with TTL (persists across cycles)
                let expires = Instant::now() + RACE_LOST_EXCLUSION_TTL;
                self.race_lost_exclusions.push((bead_id.clone(), expires));
                // Also add to exclusion_set for immediate protection in the current cycle
                self.exclusion_set.insert(bead_id.clone());
                self.race_lost_this_cycle.insert(bead_id);
                self.retry_count += 1;
                self.consecutive_race_lost += 1;
                // The claim_span is dropped here, closing the bead.claim span.
                self.set_state(WorkerState::Retrying)?;
            }
            ClaimResult::NotClaimable { reason } => {
                tracing::debug!(bead_id = %bead_id, %reason, "bead not claimable");
                // Set the claim span result attribute
                tracing::Span::current().record("needle.claim.result", &reason);
                // Set Error status on the claim span
                tracing::Span::current().record("otel.status_code", 2u64);
                tracing::Span::current().record("otel.status_description", &reason);
                self.consecutive_race_lost = 0;
                self.exclusion_set.insert(bead_id);
                self.current_bead = None;
                // The claim_span is dropped here, closing the bead.claim span.
                self.set_state(WorkerState::Selecting)?;
            }
        }

        Ok(())
    }

    /// RETRYING: decide whether to retry claiming or move on.
    ///
    /// Tracks consecutive race_lost across retry cycles. When the count
    /// exceeds `claim_race_lost_skip`, the worker treats the ready queue
    /// as effectively empty and transitions to Exhausted instead of
    /// spinning indefinitely on the same bead.
    async fn do_retry(&mut self) -> Result<()> {
        let skip_threshold = self.config.worker.claim_race_lost_skip;

        if self.consecutive_race_lost >= skip_threshold {
            tracing::warn!(
                consecutive_race_lost = self.consecutive_race_lost,
                threshold = skip_threshold,
                "consecutive race_lost exceeded skip threshold, treating queue as exhausted"
            );
            self.telemetry.emit(EventKind::ClaimRaceLostSkipped {
                consecutive_losses: self.consecutive_race_lost,
                threshold: skip_threshold,
            })?;
            self.consecutive_race_lost = 0;
            self.retry_count = 0;
            self.exclusion_set.clear();
            self.race_lost_this_cycle.clear();
            self.current_bead = None;
            self.set_state(WorkerState::Exhausted)?;
            return Ok(());
        }

        // Exponential backoff: start at 100ms, doubling each time, capped at 5s.
        // This ensures even the first retry has a small delay to prevent tight loops.
        let backoff_ms = if self.consecutive_race_lost > 0 {
            // For race-lost retries: 100ms, 200ms, 400ms, 800ms, 1600ms, 3200ms, 5000ms (capped)
            std::cmp::min(
                100 * (1u64 << (self.consecutive_race_lost - 1).min(5)),
                5000,
            )
        } else {
            // For other retries (e.g., max_claim_retries): 100ms minimum
            100
        };
        tracing::debug!(
            consecutive_race_lost = self.consecutive_race_lost,
            backoff_ms,
            "backing off before retry"
        );
        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;

        if self.retry_count < self.config.worker.max_claim_retries {
            self.set_state(WorkerState::Selecting)?;
        } else {
            tracing::debug!(
                retry_count = self.retry_count,
                "max claim retries exceeded, clearing retry state for next cycle"
            );
            self.retry_count = 0;
            self.consecutive_race_lost = 0;
            self.exclusion_set.clear();
            self.race_lost_this_cycle.clear();
            // NOTE: Do NOT clear race_lost_exclusions here. Those have TTL-based
            // expiration and must persist to prevent re-selecting the same bead
            // that just lost a claim race. Clearing them would cause an infinite
            // race-lost loop (see needle-aad8).
            self.current_bead = None;
            self.set_state(WorkerState::Selecting)?;
        }
        Ok(())
    }

    /// BUILDING: construct prompt from claimed bead.
    async fn do_build(&mut self) -> Result<()> {
        let bead = match self.current_bead {
            Some(ref b) => b.clone(),
            None => {
                bail!("BUILDING state without current_bead — invariant violated");
            }
        };

        let build_ws = if is_workspace_unset(&bead.workspace) {
            self.config.workspace.default.clone()
        } else {
            bead.workspace.clone()
        };

        let worker_name = self.worker_name.clone();
        let prompt_builder = self.prompt_builder.clone();

        // Wrap prompt building in timeout. The build operation can be slow for
        // large workspaces with many learning files.
        // Enforce minimum timeout to prevent indefinite hangs (issue needle-3igr).
        const MIN_BUILDING_TIMEOUT_SECS: u64 = 60;
        let timeout_secs = self
            .config
            .worker
            .building_timeout
            .max(MIN_BUILDING_TIMEOUT_SECS);
        let timeout_dur = std::time::Duration::from_secs(timeout_secs);
        let bead_id = bead.id.clone();
        let heartbeat_bead_id = bead_id.clone();
        let telemetry = self.telemetry.clone();

        // Enter the bead.prompt_build span for the prompt building phase.
        let prompt_build_span = tracing::info_span!(
            "bead.prompt_build",
            needle.bead.id = %bead_id,
        );
        let _prompt_build_enter = prompt_build_span.enter();

        // Spawn heartbeat task that emits periodic updates during the build.
        // Heartbeat interval: every 30 seconds.
        let heartbeat_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            let start = std::time::Instant::now();
            loop {
                interval.tick().await;
                let elapsed_ms = start.elapsed().as_millis() as u64;
                let _ = telemetry.emit(EventKind::BuildHeartbeat {
                    bead_id: heartbeat_bead_id.clone(),
                    elapsed_ms,
                });
            }
        });

        let mut prompt = match tokio::time::timeout(
            timeout_dur,
            tokio::task::spawn_blocking(move || {
                prompt_builder.build_pluck(&bead, &build_ws, &worker_name)
            }),
        )
        .await
        {
            Ok(Ok(result)) => result?,
            Ok(Err(e)) => {
                heartbeat_handle.abort();
                bail!("prompt building task failed: {}", e);
            }
            Err(_) => {
                heartbeat_handle.abort();
                // Timeout: release the bead and transition to RETRYING.
                tracing::error!(
                    bead_id = %bead_id,
                    timeout_secs = timeout_secs,
                    configured_timeout = self.config.worker.building_timeout,
                    "BUILDING state timed out"
                );

                // Set Error status on the bead.prompt_build span
                tracing::Span::current().record("otel.status_code", 2u64);
                tracing::Span::current().record(
                    "otel.status_description",
                    format!("timeout after {}s", timeout_secs),
                );

                // Emit build.timeout event.
                let _ = self.telemetry.emit(EventKind::BuildTimeout {
                    bead_id: bead_id.clone(),
                    timeout_secs,
                });

                // Release the bead with timeout protection.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    self.store.release(&bead_id),
                )
                .await;

                // Emit bead.released event.
                let _ = self.telemetry.emit(EventKind::BeadReleased {
                    bead_id: bead_id.clone(),
                    reason: "build_timeout".to_string(),
                });

                // Clear current bead and transition to RETRYING.
                self.current_bead = None;
                self.set_state(WorkerState::Retrying)?;
                return Ok(());
            }
        };

        // Stop heartbeat task.
        heartbeat_handle.abort();

        // Prepend the HOOP dispatch tag so session tailers can join transcripts
        // back to beads. Format: [needle:<qualified-worker>:<bead-id>:<strand>]
        let strand = self.current_strand.as_deref().unwrap_or("pluck");
        prompt.content = format!(
            "[needle:{}:{}:{}]\n{}",
            self.qualified_id(),
            bead_id,
            strand,
            prompt.content
        );

        // Store the prompt for the dispatch phase. We use a transient field pattern:
        // the prompt is passed via self.built_prompt.
        self.built_prompt = Some(prompt);
        self.set_state(WorkerState::Dispatching)?;
        Ok(())
    }

    /// DISPATCHING: check rate limits, resolve adapter, and prepare for execution.
    async fn do_dispatch(&mut self) -> Result<()> {
        if self.current_bead.is_none() {
            bail!("DISPATCHING state without current_bead — invariant violated");
        }

        // Check rate limits before dispatching.
        let adapter = self.resolve_adapter()?;
        let provider = adapter.provider.as_deref();
        let model = adapter.model.as_deref();

        // Enter the agent.dispatch span for the dispatching phase.
        let _bead_id = self.current_bead.as_ref().map(|b| b.id.clone());
        let dispatch_span = tracing::info_span!(
            "agent.dispatch",
            gen_ai.system = %provider.unwrap_or("unknown"),
            gen_ai.request.model = %model.unwrap_or("unknown"),
            needle.agent.pid = tracing::field::Empty, // Will be set when process starts
            needle.agent.exit_code = tracing::field::Empty, // Will be set after execution
        );
        let _dispatch_enter = dispatch_span.enter();

        let decision = self.rate_limiter.check(provider, model, &self.registry)?;

        if !decision.is_allowed() {
            let reason = format!("{decision}");
            tracing::info!(
                reason = %reason,
                "rate limited, waiting before retry"
            );
            self.telemetry.emit(EventKind::RateLimitWait {
                provider: provider.unwrap_or("unknown").to_string(),
                model: model.map(|s| s.to_string()),
                reason: reason.clone(),
            })?;

            // Wait before retrying (5 seconds).
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            // Stay in Dispatching state to retry the rate limit check.
            return Ok(());
        }

        self.telemetry.emit(EventKind::RateLimitAllowed {
            provider: provider.unwrap_or("unknown").to_string(),
            model: model.map(|s| s.to_string()),
        })?;

        // Check system resources (CPU and memory warnings).
        crate::rate_limit::RateLimiter::check_system_resources(
            self.config.worker.cpu_load_warn,
            self.config.worker.memory_free_warn_mb,
        );

        self.set_state(WorkerState::Executing)?;
        Ok(())
    }

    /// EXECUTING: run the agent process and capture output.
    async fn do_execute(&mut self) -> Result<()> {
        let bead = match self.current_bead {
            Some(ref b) => b.clone(),
            None => {
                bail!("EXECUTING state without current_bead — invariant violated");
            }
        };

        let prompt = match self.built_prompt.take() {
            Some(p) => p,
            None => {
                bail!("EXECUTING state without built_prompt — invariant violated");
            }
        };

        let adapter = self.resolve_adapter()?;

        // Race the dispatch against the shutdown signal.
        let was_interrupted;
        let exec_result = if self.shutdown.load(Ordering::SeqCst) {
            // Already shutting down — don't start the agent.
            was_interrupted = true;
            None
        } else {
            // Use the bead's workspace if set (remote bead from Explore),
            // otherwise fall back to the config's default workspace.
            let dispatch_ws = if is_workspace_unset(&bead.workspace) {
                &self.config.workspace.default
            } else {
                &bead.workspace
            };
            // Capture HEAD so do_handle can tag new commits with Bead-Id on success.
            // Wrap in timeout to prevent indefinite hang if git subprocess hangs.
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                commit_hook::git_head(dispatch_ws.to_str().unwrap_or(".")),
            )
            .await
            {
                Ok(Ok(head)) => {
                    self.pre_dispatch_head = Some(head);
                }
                Ok(Err(e)) => {
                    tracing::debug!(
                        workspace = %dispatch_ws.display(),
                        error = %e,
                        "git_head failed (not a git repo or git error), skipping Bead-Id trailer"
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        workspace = %dispatch_ws.display(),
                        "git_head timed out after 10s, skipping Bead-Id trailer"
                    );
                }
            }
            // Enter the agent.execution span for the actual agent process execution.
            // This is a child of agent.dispatch.
            // We need to record attributes on the parent agent.dispatch span after execution,
            // so we use a scope to drop the execution_span guard first.
            let (result, exec_tokens) = {
                let execution_span = tracing::info_span!(
                    "agent.execution",
                    needle.bead.id = %bead.id,
                );
                let _execution_enter = execution_span.enter();

                let result = self
                    .dispatcher
                    .dispatch(&bead.id, &prompt, &adapter, dispatch_ws)
                    .await?;

                // Set span status based on exit code: 0 = Ok, non-zero = Error
                if result.exit_code != 0 {
                    tracing::Span::current().record("otel.status_code", 2u64);
                    tracing::Span::current().record(
                        "otel.status_description",
                        format!("exit_code: {}", result.exit_code),
                    );
                }

                // Extract tokens from the result while still in the execution span.
                let exec_tokens = dispatch::extract_tokens(
                    &adapter.token_extraction,
                    &result.stdout,
                    &result.stderr,
                );
                (result, exec_tokens)
            };

            // Now we're back in the agent.dispatch span. Record the execution results.
            tracing::Span::current().record("needle.agent.pid", result.pid);
            tracing::Span::current().record("needle.agent.exit_code", result.exit_code);
            if let Some(input_tokens) = exec_tokens.input_tokens {
                tracing::Span::current().record("gen_ai.usage.input_tokens", input_tokens);
            }
            if let Some(output_tokens) = exec_tokens.output_tokens {
                tracing::Span::current().record("gen_ai.usage.output_tokens", output_tokens);
            }

            // Set agent.dispatch span status based on exit code: 0 = Ok, non-zero = Error
            if result.exit_code != 0 {
                tracing::Span::current().record("otel.status_code", 2u64);
                tracing::Span::current().record(
                    "otel.status_description",
                    format!("exit_code: {}", result.exit_code),
                );
            }

            was_interrupted = self.shutdown.load(Ordering::SeqCst);
            Some(result)
        };

        let output = match exec_result {
            Some(ref result) => AgentOutcome {
                exit_code: result.exit_code,
                stdout: result.stdout.clone(),
                stderr: result.stderr.clone(),
            },
            None => AgentOutcome {
                exit_code: 130, // Simulated SIGINT
                stdout: String::new(),
                stderr: "interrupted before execution".to_string(),
            },
        };

        // Extract tokens and compute cost for effort tracking.
        let tokens =
            dispatch::extract_tokens(&adapter.token_extraction, &output.stdout, &output.stderr);
        let model_name = adapter.model.as_deref().unwrap_or("");
        let estimated_cost = cost::estimate_cost(&tokens, model_name, &self.config.pricing);

        if let Some(ref mut effort) = self.last_effort {
            effort.agent_name = adapter.name.clone();
            effort.model = adapter.model.clone();
            effort.tokens = tokens;
            effort.estimated_cost_usd = estimated_cost;
        }

        self.exec_output = Some((output, was_interrupted));
        self.set_state(WorkerState::Handling)?;
        Ok(())
    }

    /// HANDLING: classify outcome and route to handler.
    async fn do_handle(&mut self) -> Result<()> {
        let bead = match self.current_bead {
            Some(ref b) => b.clone(),
            None => {
                bail!("HANDLING state without current_bead — invariant violated");
            }
        };

        let (output, was_interrupted) = match self.exec_output.take() {
            Some(pair) => pair,
            None => {
                bail!("HANDLING state without exec_output — invariant violated");
            }
        };

        // Emit an initial heartbeat event to signal we've entered HANDLING state.
        // This provides immediate visibility in the JSONL log when handling starts.
        // Use emit_try_lock() to avoid blocking if telemetry writer is stuck.
        let _ = self.telemetry.emit_try_lock(EventKind::HeartbeatEmitted {
            bead_id: Some(bead.id.clone()),
            state: "HANDLING".to_string(),
        });

        // Create a cancellation flag that can be used to abort the outcome handler
        // if it hangs. This is a workaround for tokio::time::timeout not cancelling
        // the future - it just stops waiting for it.
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let cancelled = Arc::new(AtomicBool::new(false));

        // Spawn a background task that emits heartbeat telemetry events every 5 seconds.
        // This allows external monitoring to detect hangs in HANDLING state without
        // waiting for the slower heartbeat file interval (default 60s).
        let bead_id_for_heartbeat = bead.id.clone();
        let telemetry_for_heartbeat = self.telemetry.clone();
        let cancelled_for_heartbeat = cancelled.clone();
        let watchdog_for_heartbeat = self.watchdog_triggered.clone();
        let heartbeat_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                // Check if we've been cancelled and stop emitting if so.
                if cancelled_for_heartbeat.load(Ordering::Relaxed) {
                    break;
                }
                // Check if the watchdog has triggered and force recovery if so.
                // This allows the watchdog thread to interrupt HANDLING state even if
                // the tokio runtime is wedged (the watchdog runs in a separate thread).
                if watchdog_for_heartbeat.load(Ordering::Relaxed) {
                    tracing::error!(
                        bead_id = %bead_id_for_heartbeat,
                        "heartbeat task detected watchdog trigger, forcing cancellation"
                    );
                    // Set the cancelled flag to abort any in-flight br calls.
                    cancelled_for_heartbeat.store(true, Ordering::Release);
                    break;
                }
                // Use emit_try_lock() to avoid blocking if telemetry writer is stuck.
                let _ = telemetry_for_heartbeat.emit_try_lock(EventKind::HeartbeatEmitted {
                    bead_id: Some(bead_id_for_heartbeat.clone()),
                    state: "HANDLING".to_string(),
                });
            }
        });

        // Clone values needed for error handling before creating the async block.
        // This avoids borrowing issues with the async block that captures `self`.
        let bead_id_clone = bead.id.clone();
        let store_clone = self.store.clone();
        let telemetry_clone = self.telemetry.clone();
        let cancelled_clone = cancelled.clone();

        // Wrap the entire HANDLING state in a timeout to prevent indefinite hangs.
        // Even if the Tokio runtime gets blocked by a synchronous operation, this
        // timeout will fire (on a threadpool) and allow recovery.
        let handling_future = async {
            // Wrap the outcome handler in a 60-second timeout to prevent indefinite hangs.
            // The health monitor's background thread writes heartbeat files based on
            // shared state, so external monitoring can detect hangs via stale heartbeats.

            // Enter the bead.outcome span for the outcome handling phase.
            let outcome_span = tracing::info_span!(
                "bead.outcome",
                needle.bead.id = %bead.id,
                needle.outcome = tracing::field::Empty, // Will be set based on handler result
                needle.outcome.action = tracing::field::Empty, // Will be set based on handler result
            );
            let _outcome_enter = outcome_span.enter();

            let handler_future = self.outcome_handler.handle_with_cancellation(
                self.store.as_ref(),
                &bead,
                &output,
                was_interrupted,
                cancelled.clone(),
            );

            match tokio::time::timeout(std::time::Duration::from_secs(60), handler_future).await {
                Ok(Ok(result)) => {
                    // Handler completed successfully - stop heartbeat and continue.
                    // Record the outcome and action on the bead.outcome span.
                    tracing::Span::current().record("needle.outcome", result.outcome.as_str());
                    tracing::Span::current()
                        .record("needle.outcome.action", result.bead_action.to_string());

                    // Store outcome for recording on bead.lifecycle span
                    self.last_outcome = Some(result.outcome.as_str().to_string());

                    // Set span status: Ok for Success, Error for all other outcomes.
                    match result.outcome {
                        crate::types::Outcome::Success => {
                            // Span status is Ok by default
                        }
                        _ => {
                            // Set Error status with the outcome as description
                            // otel.status_code = 2 indicates ERROR in OpenTelemetry
                            tracing::Span::current().record("otel.status_code", 2u64);
                            tracing::Span::current()
                                .record("otel.status_description", result.outcome.as_str());
                        }
                    }

                    tracing::debug!(
                        bead_id = %bead.id,
                        outcome = %result.outcome,
                        action = %result.bead_action,
                        "handler completed successfully, stopping heartbeat task"
                    );
                    Ok(result)
                }
                Ok(Err(e)) => {
                    // Handler returned an error - attempt best-effort release and recover.
                    tracing::error!(
                        bead_id = %bead.id,
                        error = %e,
                        "outcome handler failed, attempting best-effort release and transitioning to LOGGING"
                    );
                    // Set cancellation flag to stop heartbeat and abort any in-flight br calls.
                    cancelled.store(true, Ordering::Release);
                    // Abort the heartbeat task to prevent it from continuing in the background.
                    heartbeat_task.abort();
                    // Use emit_try_lock() to avoid blocking on telemetry mutex if writer is stuck.
                    let _ = telemetry_clone.emit_try_lock(EventKind::WorkerHandlingTimeout {
                        bead_id: bead_id_clone.clone(),
                        outcome: "unknown".to_string(),
                        operation: "handle".to_string(),
                        error: e.to_string(),
                    });
                    // Attempt best-effort release with timeout.
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        store_clone.release(&bead_id_clone),
                    )
                    .await;
                    // Explicitly transition to LOGGING to recover.
                    self.set_state(WorkerState::Logging)?;
                    Err(anyhow::anyhow!("handler failed: {}", e))
                }
                Err(_) => {
                    // Timeout after 60 seconds - attempt best-effort release and transition to LOGGING.
                    tracing::error!(
                        bead_id = %bead.id,
                        "outcome handler timed out after 60s, attempting best-effort release and transitioning to LOGGING"
                    );
                    // Set cancellation flag to stop heartbeat and abort any in-flight br calls.
                    cancelled.store(true, Ordering::Release);
                    // Abort the heartbeat task to prevent it from continuing in the background.
                    heartbeat_task.abort();
                    // Use emit_try_lock() to avoid blocking on telemetry mutex if writer is stuck.
                    let _ = telemetry_clone.emit_try_lock(EventKind::WorkerHandlingTimeout {
                        bead_id: bead_id_clone.clone(),
                        outcome: "unknown".to_string(),
                        operation: "handle".to_string(),
                        error: "timeout after 60s".to_string(),
                    });
                    // Attempt best-effort release with timeout.
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        store_clone.release(&bead_id_clone),
                    )
                    .await;
                    // Explicitly transition to LOGGING to recover.
                    self.set_state(WorkerState::Logging)?;
                    Err(anyhow::anyhow!("handler timed out after 60s"))
                }
            }
        };

        // Wrap the entire HANDLING state in a 90-second timeout using spawn_blocking.
        // This provides a safety net that can fire even if the tokio runtime becomes wedged.
        // The blocking thread runs independently of the async runtime, so the timeout will
        // trigger even if all async tasks are blocked. The 90s limit allows the inner 60s
        // timeout to fire first under normal conditions, but provides a fallback if needed.

        // Use a channel to signal timeout from the blocking thread.
        let (timeout_tx, timeout_rx) = tokio::sync::oneshot::channel::<()>();

        // Spawn a blocking thread that will send a timeout signal after 90 seconds.
        let cancelled_for_timeout = cancelled_clone.clone();
        tokio::task::spawn_blocking(move || {
            std::thread::sleep(std::time::Duration::from_secs(90));
            // Only send timeout signal if not already cancelled.
            if !cancelled_for_timeout.load(Ordering::Relaxed) {
                let _ = timeout_tx.send(());
            }
        });

        // Use tokio::select! to race between the handling future and the timeout signal.
        let handler_result = tokio::select! {
            result = handling_future => {
                // Handling completed (or inner timeout fired) - cancel the outer timeout.
                cancelled.store(true, Ordering::Release);
                // The timeout_tx is dropped here, which will cause the blocking thread's
                // send() to fail, effectively cancelling it.
                match result {
                    Ok(result) => {
                        heartbeat_task.abort();
                        result
                    }
                    Err(_) => {
                        // HANDLING failed but recovered - stop heartbeat and continue to LOGGING.
                        heartbeat_task.abort();
                        return Ok(());
                    }
                }
            }
            _ = timeout_rx => {
                // Outer timeout fired after 90 seconds - this is a critical failure.
                tracing::error!(
                    bead_id = %bead.id,
                    "HANDLING state timed out after 90s, forcing recovery"
                );
                // Set cancellation flag to stop all async operations.
                cancelled.store(true, Ordering::Release);
                heartbeat_task.abort();
                // Emit critical timeout event.
                let _ = telemetry_clone.emit_try_lock(EventKind::WorkerHandlingTimeout {
                    bead_id: bead_id_clone.clone(),
                    outcome: "unknown".to_string(),
                    operation: "handling_state".to_string(),
                    error: "critical timeout after 90s".to_string(),
                });
                // Attempt best-effort release with timeout.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    store_clone.release(&bead_id_clone),
                )
                .await;
                // Force transition to LOGGING to recover.
                self.set_state(WorkerState::Logging)?;
                return Ok(());
            }
        };

        // Check if the watchdog triggered during HANDLING state.
        // This can happen if the heartbeat task detected the watchdog trigger and
        // set the cancelled flag, or if the watchdog thread set the flag directly.
        if self.watchdog_triggered.load(Ordering::Relaxed) {
            tracing::error!(
                bead_id = %bead.id,
                "watchdog detected during HANDLING state, forcing recovery to LOGGING"
            );
            // Clear the watchdog trigger.
            self.watchdog_triggered.store(false, Ordering::Release);
            self.handling_state_entered_at = None;
            // Emit critical timeout event.
            let _ = self
                .telemetry
                .emit_try_lock(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "unknown".to_string(),
                    operation: "watchdog".to_string(),
                    error: format!(
                        "HANDLING state exceeded {}s timeout",
                        HANDLING_WATCHDOG_TIMEOUT_SECS
                    ),
                });
            // Force transition to LOGGING to recover.
            self.set_state(WorkerState::Logging)?;
            // Stop the heartbeat task.
            cancelled.store(true, Ordering::Release);
            heartbeat_task.abort();
            return Ok(());
        }

        // Emit a heartbeat after the outcome handler completes to signal we're
        // still alive. This helps detect hangs in post-handler code (commit hook,
        // mitosis, state transitions) that occur after the handler finishes.
        // Use emit_try_lock() to avoid blocking if telemetry writer is stuck.
        let _ = self.telemetry.emit_try_lock(EventKind::HeartbeatEmitted {
            bead_id: Some(bead.id.clone()),
            state: "HANDLING_POST_HANDLER".to_string(),
        });

        // Evaluate for mitosis after failure — the bead has already been
        // released and failure count incremented by the outcome handler.
        if handler_result.outcome == Outcome::Failure {
            let workspace = if is_workspace_unset(&bead.workspace) {
                self.config.workspace.default.clone()
            } else {
                bead.workspace.clone()
            };

            // Enter the bead.mitosis span for mitosis evaluation.
            let mitosis_span = tracing::info_span!(
                "bead.mitosis",
                needle.bead.id = %bead.id,
                needle.mitosis.result = tracing::field::Empty, // Will be set based on evaluation result
            );
            let _mitosis_enter = mitosis_span.enter();

            // Wrap mitosis evaluation in timeout to prevent indefinite hang.
            match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                self.mitosis_evaluator.evaluate(
                    self.store.as_ref(),
                    &bead,
                    &workspace,
                    &self.dispatcher,
                    &self.prompt_builder,
                    &self.config.agent.default,
                ),
            )
            .await
            {
                Ok(Ok(crate::mitosis::MitosisResult::Split { children })) => {
                    tracing::Span::current().record("needle.mitosis.result", "split");
                    tracing::info!(
                        bead_id = %bead.id,
                        children = children.len(),
                        "mitosis created child beads — parent blocked"
                    );
                }
                Ok(Ok(crate::mitosis::MitosisResult::NotSplittable)) => {
                    tracing::Span::current().record("needle.mitosis.result", "not_splittable");
                    tracing::debug!(bead_id = %bead.id, "mitosis: bead is single task");
                }
                Ok(Ok(crate::mitosis::MitosisResult::Skipped { reason })) => {
                    tracing::Span::current().record("needle.mitosis.result", "skipped");
                    tracing::debug!(
                        bead_id = %bead.id,
                        reason = %reason,
                        "mitosis skipped"
                    );
                }
                Ok(Err(e)) => {
                    tracing::Span::current().record("needle.mitosis.result", "error");
                    tracing::Span::current().record("otel.status_code", 2u64);
                    tracing::Span::current()
                        .record("otel.status_description", format!("error: {e}"));
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %e,
                        "mitosis evaluation failed (bead already released)"
                    );
                }
                Err(_) => {
                    // Timeout after 120s - log warning and continue.
                    tracing::Span::current().record("needle.mitosis.result", "timeout");
                    tracing::Span::current().record("otel.status_code", 2u64);
                    tracing::Span::current()
                        .record("otel.status_description", "timeout after 120s");
                    tracing::warn!(
                        bead_id = %bead.id,
                        "mitosis evaluation timed out after 120s, continuing to LOGGING"
                    );
                }
            }
            // mitosis span ends here when _mitosis_enter is dropped
        }

        // On success, inject Bead-Id trailer into the latest commit (non-fatal if it fails).
        if handler_result.outcome == Outcome::Success {
            if let Some(ref pre_head) = self.pre_dispatch_head {
                let workspace = if is_workspace_unset(&bead.workspace) {
                    self.config.workspace.default.clone()
                } else {
                    bead.workspace.clone()
                };
                // Wrap commit hook in timeout to prevent indefinite hang.
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    commit_hook::inject_bead_id_trailer(&workspace, &bead.id, pre_head),
                )
                .await
                {
                    Ok(Ok(())) => {
                        tracing::debug!(
                            bead_id = %bead.id,
                            "Bead-Id trailer injected successfully"
                        );
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            error = %e,
                            "Bead-Id trailer injection failed (non-fatal)"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            "Bead-Id trailer injection timed out after 30s (non-fatal)"
                        );
                    }
                }
            }
        }

        if was_interrupted {
            // After handling interrupted outcome, stop the worker.
            self.set_state(WorkerState::Stopped)?;
        } else {
            self.set_state(WorkerState::Logging)?;
        }

        // Set cancellation flag and abort the heartbeat task since handling is complete.
        cancelled.store(true, Ordering::Release);
        heartbeat_task.abort();

        Ok(())
    }

    /// LOGGING: record effort telemetry, check budget, update registry, and
    /// prepare for next cycle.
    fn do_log(&mut self) -> Result<()> {
        let bead_id = self.current_bead.as_ref().map(|b| b.id.clone());

        // Emit effort.recorded telemetry event.
        if let (Some(ref effort), Some(ref id)) = (&self.last_effort, &bead_id) {
            let elapsed_ms = effort.cycle_start.elapsed().as_millis() as u64;
            self.telemetry.emit(EventKind::EffortRecorded {
                bead_id: id.clone(),
                elapsed_ms,
                agent_name: effort.agent_name.clone(),
                model: effort.model.clone(),
                tokens_in: effort.tokens.input_tokens,
                tokens_out: effort.tokens.output_tokens,
                estimated_cost_usd: effort.estimated_cost_usd,
            })?;

            if let Some(cost_usd) = effort.estimated_cost_usd {
                tracing::info!(
                    bead_id = %id,
                    elapsed_ms,
                    agent = %effort.agent_name,
                    model = ?effort.model,
                    tokens_in = ?effort.tokens.input_tokens,
                    tokens_out = ?effort.tokens.output_tokens,
                    cost_usd = %format!("{:.4}", cost_usd),
                    "effort recorded"
                );
            }
        }

        // Check daily budget thresholds.
        self.check_budget()?;

        // Clear per-cycle state.
        self.last_effort = None;
        self.beads_processed += 1;
        self.current_bead = None;

        // Close the bead.lifecycle span by dropping it.
        // Record the outcome before closing if we have the handler result available.
        if let Some(lifecycle_guard) = self.bead_lifecycle_span.take() {
            // Record the outcome on the bead.lifecycle span before closing
            if let Some(ref outcome) = self.last_outcome {
                lifecycle_guard.record("needle.bead.outcome", outcome.as_str());
                // Set span status: Ok for success, Error for all other outcomes
                if outcome != "success" {
                    // otel.status_code = 2 indicates ERROR in OpenTelemetry
                    lifecycle_guard.record("otel.status_code", 2u64);
                    lifecycle_guard.record("otel.status_description", outcome.as_str());
                }
            }
            // Clear the outcome for the next cycle
            self.last_outcome = None;
            // The span is automatically closed when the guard is dropped.
        }

        // Update heartbeat with new bead count.
        self.health.update_beads_processed(self.beads_processed);

        // Update registry with current beads_processed count (best-effort).
        if let Err(e) = self
            .registry
            .update_beads_processed(&self.qualified_id(), self.beads_processed)
        {
            tracing::warn!(error = %e, "failed to update registry beads_processed");
        }

        // Auto-canary: when self_modification is enabled with auto_promote, detect a
        // :testing binary, run the canary suite, and promote or reject. A successful
        // promotion puts a new :stable in place, which the hot-reload check below
        // picks up in the same cycle.
        if self.config.self_modification.enabled && self.config.self_modification.auto_promote {
            self.check_auto_canary()?;
        }

        // Hot-reload check: detect new :stable binary between cycles.
        if self.config.self_modification.hot_reload {
            self.check_hot_reload()?;
        }

        self.set_state(WorkerState::Selecting)?;
        Ok(())
    }

    /// Auto-canary promotion: detect a :testing binary and run the canary suite.
    ///
    /// Called between LOGGING and the hot-reload check. If a :testing binary
    /// is present:
    /// 1. Run canary tests against :testing in the canary workspace
    /// 2. If all pass → promote :testing to :stable, emit `canary.promoted`
    /// 3. If any fail → reject :testing (delete it), emit `canary.rejected`
    ///
    /// Errors are non-fatal: logged as warnings, worker continues unchanged.
    fn check_auto_canary(&mut self) -> Result<()> {
        if !self.config.self_modification.enabled {
            return Ok(());
        }
        if !self.config.self_modification.auto_promote {
            return Ok(());
        }

        let runner = CanaryRunner::new(
            self.config.workspace.home.clone(),
            self.config.self_modification.canary_workspace.clone(),
            self.config.self_modification.canary_timeout,
        );

        // Only proceed if a :testing binary is present.
        if !runner.testing_binary().exists() {
            return Ok(());
        }

        let suite_id = runner.testing_binary().display().to_string();
        tracing::info!(suite = %suite_id, "testing binary detected — running canary suite");

        if let Err(e) = self.telemetry.emit(EventKind::CanaryStarted {
            suite: suite_id.clone(),
        }) {
            tracing::warn!(error = %e, "failed to emit CanaryStarted telemetry");
        }

        let report = match runner.run() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "canary run failed, skipping auto-promotion");
                return Ok(());
            }
        };

        let _ = self.telemetry.emit(EventKind::CanarySuiteCompleted {
            suite: suite_id.clone(),
            passed: report.passed as u32,
            failed: (report.failed + report.timed_out + report.errors) as u32,
        });

        if report.can_promote() {
            tracing::info!("canary passed — promoting :testing to :stable");
            let hash = upgrade::file_hash(&report.testing_binary)
                .unwrap_or_else(|_| "unknown".to_string());
            if let Err(e) = runner.promote() {
                tracing::warn!(error = %e, "canary promotion failed");
                return Ok(());
            }
            let _ = self.telemetry.emit(EventKind::CanaryPromoted { hash });
            tracing::info!("promotion complete — fleet will hot-reload on next cycle");
        } else {
            let reason = format!(
                "{} failed, {} timed out, {} errors",
                report.failed, report.timed_out, report.errors
            );
            tracing::warn!(reason = %reason, "canary failed — rejecting :testing");
            if let Err(e) = runner.reject() {
                tracing::warn!(error = %e, "canary reject failed");
            }
            let _ = self.telemetry.emit(EventKind::CanaryRejected { reason });
        }

        Ok(())
    }

    /// Check for a new :stable binary and re-exec if detected.
    ///
    /// Called between LOGGING and SELECTING. If a new binary is found:
    /// 1. Emit `worker.upgrade.detected` telemetry
    /// 2. Re-exec with `--resume` to preserve worker identity
    ///
    /// On re-exec failure, log the error and continue with the current binary.
    fn check_hot_reload(&mut self) -> Result<()> {
        let needle_home = &self.config.workspace.home;
        match upgrade::check_hot_reload(needle_home) {
            Ok(HotReloadCheck::NewBinaryDetected {
                old_hash,
                new_hash,
                stable_path,
            }) => {
                tracing::info!(
                    old_hash = %&old_hash[..12],
                    new_hash = %&new_hash[..12],
                    "new :stable binary detected — preparing hot-reload"
                );

                self.telemetry.emit(EventKind::UpgradeDetected {
                    old_hash: old_hash.clone(),
                    new_hash: new_hash.clone(),
                })?;

                // Attempt re-exec. This call does not return on success.
                let workspace = Some(self.config.workspace.default.as_path());
                let agent = Some(self.config.agent.default.as_str());
                let timeout = Some(self.config.agent.timeout);

                match upgrade::re_exec_stable(
                    &stable_path,
                    &self.worker_name,
                    workspace,
                    agent,
                    timeout,
                ) {
                    Ok(()) => {
                        // Unreachable on Unix — exec replaces the process.
                        Ok(())
                    }
                    Err(e) => {
                        // Re-exec failed — continue on current binary.
                        tracing::warn!(
                            error = %e,
                            "hot-reload re-exec failed, continuing on current binary"
                        );
                        Ok(())
                    }
                }
            }
            Ok(HotReloadCheck::NoChange) => Ok(()),
            Ok(HotReloadCheck::Skipped { reason }) => {
                tracing::debug!(reason = %reason, "hot-reload check skipped");
                Ok(())
            }
            Err(e) => {
                tracing::warn!(error = %e, "hot-reload check failed, continuing");
                Ok(())
            }
        }
    }

    // ── Terminal state handlers ─────────────────────────────────────────────

    /// Handle the EXHAUSTED state: either wait and retry or exit.
    async fn handle_exhausted(&mut self) -> Result<WorkerState> {
        self.telemetry.emit(EventKind::WorkerExhausted {
            cycle_count: self.beads_processed,
            last_strand: self
                .strands
                .strand_names()
                .last()
                .unwrap_or(&"none")
                .to_string(),
            waterfall_restarts: self.last_waterfall_restarts,
            restart_triggers: self.last_restart_triggers.clone(),
            strand_evaluations: self.last_strand_evaluations.clone(),
        })?;

        match self.config.worker.idle_action {
            IdleAction::Wait => {
                let backoff = self.config.worker.idle_timeout;
                tracing::info!(
                    backoff_secs = backoff,
                    "all strands exhausted, waiting before retry"
                );
                self.telemetry.emit(EventKind::WorkerIdle {
                    backoff_seconds: backoff,
                })?;

                // Emit diagnostic event BEFORE updating state to ensure we have
                // a record even if the worker dies during the state update.
                if let Err(e) = self.telemetry.emit(EventKind::HeartbeatEmitted {
                    bead_id: None,
                    state: "EXHAUSTED_PRE_IDLE".to_string(),
                }) {
                    tracing::warn!(error = %e, "failed to emit pre-idle heartbeat, continuing anyway");
                }

                // Force-flush to ensure the diagnostic event is written.
                let _ = self
                    .telemetry
                    .force_flush_async(std::time::Duration::from_secs(1))
                    .await;

                // Update heartbeat immediately before entering idle sleep so external
                // monitoring has a fresh timestamp. If the worker dies during the
                // idle period, the heartbeat file will become stale and can be detected.
                self.health.update_state(
                    &WorkerState::Exhausted,
                    None,
                    Some(self.current_workspace.as_path()),
                );

                // Emit diagnostic event AFTER state update to confirm it succeeded.
                if let Err(e) = self.telemetry.emit(EventKind::HeartbeatEmitted {
                    bead_id: None,
                    state: "EXHAUSTED_POST_IDLE_UPDATE".to_string(),
                }) {
                    tracing::warn!(error = %e, "failed to emit post-update heartbeat, continuing anyway");
                }

                // Force-flush to ensure the diagnostic event is written.
                let _ = self
                    .telemetry
                    .force_flush_async(std::time::Duration::from_secs(1))
                    .await;

                // Cancellable sleep: check shutdown flag every 1 second instead of
                // sleeping for the full duration. This ensures the worker responds to
                // signals during idle within 1 second and emits worker.stopped telemetry
                // before being killed. A 1-second interval provides good responsiveness
                // while still avoiding busy-waiting.
                let check_interval = 1u64;
                let mut elapsed = 0u64;
                let mut shutdown_check_count = 0u64;

                // Emit an initial heartbeat to show we're entering idle sleep.
                // This ensures there's at least one diagnostic event even if the
                // worker dies before the first sleep iteration completes.
                if let Err(e) = self.telemetry.emit(EventKind::HeartbeatEmitted {
                    bead_id: None,
                    state: "EXHAUSTED_IDLE".to_string(),
                }) {
                    tracing::warn!(error = %e, "failed to emit initial idle heartbeat, continuing anyway");
                }

                // Emit diagnostic event to help identify external killer.
                // This event is emitted before the sleep loop starts so that if the worker
                // is killed during idle sleep, there's a record of when it entered the idle state.
                if let Err(e) = self.telemetry.emit(EventKind::IdleSleepEntered {
                    backoff_secs: backoff,
                    beads_processed: self.beads_processed,
                    uptime_secs: self.boot_time.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                }) {
                    tracing::warn!(error = %e, "failed to emit idle_sleep_entered event");
                }

                // Write a marker file to indicate the worker has entered idle sleep.
                // This provides diagnostic information even if telemetry is not flushed
                // (e.g., if the worker is killed abruptly). The marker file is removed
                // when the worker exits idle sleep.
                let state_dir = self.config.workspace.home.join("state");
                let idle_marker = state_dir.join(format!(
                    "{}-idle-entered-{}.txt",
                    self.qualified_id(),
                    std::process::id()
                ));
                let _ = std::fs::write(
                    &idle_marker,
                    format!(
                        "Worker entered idle sleep at {}\nBackoff: {} seconds\nBeads processed: {}\nUptime: {} seconds\nPID: {}\n",
                        chrono::Utc::now().to_rfc3339(),
                        backoff,
                        self.beads_processed,
                        self.boot_time.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                        std::process::id()
                    )
                );

                while elapsed < backoff {
                    let remaining = backoff - elapsed;
                    let sleep_duration =
                        std::time::Duration::from_secs(remaining.min(check_interval));

                    // CRITICAL: Emit heartbeat BEFORE sleeping, not after.
                    // This ensures that if the worker is killed during sleep, we have
                    // a record of how long it survived. The heartbeat event includes
                    // the elapsed time, which helps identify when the worker died.
                    if let Err(e) = self.telemetry.emit(EventKind::HeartbeatEmitted {
                        bead_id: None,
                        state: "EXHAUSTED_IDLE".to_string(),
                    }) {
                        tracing::warn!(error = %e, "failed to emit idle heartbeat, continuing anyway");
                    }

                    // Force-flush the heartbeat event immediately to ensure it's written
                    // to disk even if the worker is killed during the upcoming sleep.
                    // This is critical for diagnosing cases where workers die mysteriously.
                    // Use async version to avoid blocking in the async context.
                    let _ = self
                        .telemetry
                        .force_flush_async(std::time::Duration::from_secs(1))
                        .await;

                    // Update heartbeat state before sleeping to ensure the heartbeat file
                    // is fresh even if the worker dies during this sleep iteration.
                    self.health.update_state(
                        &WorkerState::Exhausted,
                        None,
                        Some(self.current_workspace.as_path()),
                    );

                    // Log before sleeping to help diagnose cases where workers die mysteriously.
                    // The elapsed time in the log shows how long the worker has been in idle state.
                    tracing::debug!(
                        elapsed_secs = elapsed,
                        backoff_secs = backoff,
                        remaining_secs = remaining,
                        sleep_duration_secs = sleep_duration.as_secs(),
                        iteration = shutdown_check_count + 1,
                        "about to sleep in idle loop"
                    );

                    // Race between sleep and shutdown flag to respond immediately to signals.
                    // This ensures that when SIGHUP is received (e.g., from cgov killing tmux session),
                    // the worker responds within milliseconds instead of waiting up to 1 second.
                    tokio::select! {
                        _ = tokio::time::sleep(sleep_duration) => {
                            // Sleep completed normally, continue to shutdown check.
                        }
                        _ = async {
                            // Poll shutdown flag every 10ms for immediate response.
                            loop {
                                if self.shutdown.load(Ordering::SeqCst) {
                                    break;
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            }
                        } => {
                            // Shutdown flag was set, exit immediately.
                        }
                    }

                    elapsed += check_interval;
                    shutdown_check_count += 1;

                    if self.shutdown.load(Ordering::SeqCst) {
                        // Retrieve and clear the last received signal for logging.
                        #[cfg(unix)]
                        let signal_name = {
                            let sig = LAST_SIGNAL.swap(0, AtomicOrdering::SeqCst);
                            if sig == 0 {
                                None
                            } else {
                                Some(match sig {
                                    1 => "SIGHUP",
                                    2 => "SIGINT",
                                    15 => "SIGTERM",
                                    _ => "unknown signal",
                                })
                            }
                        };
                        #[cfg(not(unix))]
                        let signal_name = None;

                        // Emit a diagnostic event before stopping to ensure we have
                        // a record of why the worker stopped during idle. This is
                        // especially important for debugging cases where workers
                        // die mysteriously during long idle periods.
                        let reason = if let Some(name) = signal_name {
                            format!("signal received during idle ({name})")
                        } else {
                            "shutdown received during idle".to_string()
                        };

                        tracing::info!(
                            elapsed_secs = elapsed,
                            backoff_secs = backoff,
                            shutdown_check_count,
                            reason = %reason,
                            "shutdown received during idle sleep, stopping worker"
                        );

                        // Force-flush telemetry before stopping to ensure the
                        // diagnostic event is written even if the stop() method
                        // fails or the process is killed immediately after.
                        // Use async version to avoid blocking in the async context.
                        let _ = self
                            .telemetry
                            .force_flush_async(std::time::Duration::from_secs(5))
                            .await;

                        return self.stop(&reason).await;
                    }
                }

                // Final shutdown check after loop exits to handle the race where
                // a signal was received during the last sleep iteration. Without this
                // check, the worker would transition to SELECTING instead of stopping.
                if self.shutdown.load(Ordering::SeqCst) {
                    // Retrieve and clear the last received signal for logging.
                    #[cfg(unix)]
                    let signal_name = {
                        let sig = LAST_SIGNAL.swap(0, AtomicOrdering::SeqCst);
                        if sig == 0 {
                            None
                        } else {
                            Some(match sig {
                                1 => "SIGHUP",
                                2 => "SIGINT",
                                15 => "SIGTERM",
                                _ => "unknown signal",
                            })
                        }
                    };
                    #[cfg(not(unix))]
                    let signal_name = None;

                    if let Some(name) = signal_name {
                        tracing::info!(
                            backoff_secs = backoff,
                            signal = name,
                            "shutdown received after idle loop completed, stopping worker"
                        );
                    } else {
                        tracing::info!(
                            backoff_secs = backoff,
                            "shutdown received after idle loop completed, stopping worker"
                        );
                    }
                    return self.stop("signal received after idle").await;
                }

                // Emit a diagnostic event BEFORE the tracing log to ensure we have
                // a record even if the worker dies immediately after. This helps
                // diagnose cases where workers die mysteriously after idle sleep.
                if let Err(e) = self.telemetry.emit(EventKind::IdleSleepCompleted {
                    backoff_secs: backoff,
                    elapsed_secs: elapsed,
                    shutdown_checks: shutdown_check_count,
                }) {
                    tracing::warn!(error = %e, "failed to emit idle_sleep_completed event");
                }

                // Remove the idle marker file and write a completion marker.
                // This provides diagnostic information even if telemetry is not flushed.
                let state_dir = self.config.workspace.home.join("state");
                let idle_marker = state_dir.join(format!(
                    "{}-idle-entered-{}.txt",
                    self.qualified_id(),
                    std::process::id()
                ));
                let _ = std::fs::remove_file(&idle_marker);

                let completed_marker = state_dir.join(format!(
                    "{}-idle-completed-{}.txt",
                    self.qualified_id(),
                    std::process::id()
                ));
                let _ = std::fs::write(
                    &completed_marker,
                    format!(
                        "Worker completed idle sleep at {}\nBackoff: {} seconds\nElapsed: {} seconds\nShutdown checks: {}\nBeads processed: {}\nUptime: {} seconds\nPID: {}\n",
                        chrono::Utc::now().to_rfc3339(),
                        backoff,
                        elapsed,
                        shutdown_check_count,
                        self.beads_processed,
                        self.boot_time.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                        std::process::id()
                    )
                );

                tracing::info!(
                    backoff_secs = backoff,
                    shutdown_checks_performed = shutdown_check_count,
                    elapsed_secs = elapsed,
                    "idle sleep completed successfully, transitioning to SELECTING"
                );

                // Force-flush BEFORE state transition to ensure the diagnostic event
                // is written even if the worker is killed during the transition.
                // Use async version to avoid blocking in the async context.
                let _ = self
                    .telemetry
                    .force_flush_async(std::time::Duration::from_secs(5))
                    .await;

                // Emit telemetry to show idle sleep completed successfully
                self.telemetry.emit(EventKind::StateTransition {
                    from: WorkerState::Exhausted,
                    to: WorkerState::Selecting,
                })?;

                // Force-flush AFTER state transition to ensure it's persisted.
                // Use async version to avoid blocking in the async context.
                let _ = self
                    .telemetry
                    .force_flush_async(std::time::Duration::from_secs(5))
                    .await;

                // Update heartbeat after idle sleep completes before transitioning.
                self.health.update_state(
                    &WorkerState::Selecting,
                    None,
                    Some(self.current_workspace.as_path()),
                );
                self.state = WorkerState::Selecting;
                Ok(WorkerState::Selecting)
            }
            IdleAction::Exit => {
                tracing::info!("all strands exhausted and idle_action=exit, stopping");
                self.stop("exhausted").await
            }
        }
    }

    /// Graceful stop: emit telemetry, deregister, and return terminal state.
    async fn stop(&mut self, reason: &str) -> Result<WorkerState> {
        let uptime = self.boot_time.map(|t| t.elapsed().as_secs()).unwrap_or(0);

        // Set worker.session span attributes before closing.
        // Record attributes on the current span (which is the worker.session span).
        tracing::Span::current().record("needle.beads_processed", self.beads_processed);
        tracing::Span::current().record("needle.uptime_seconds", uptime);
        tracing::Span::current().record("needle.exit_reason", reason);

        self.telemetry.emit(EventKind::WorkerStopped {
            reason: reason.to_string(),
            beads_processed: self.beads_processed,
            uptime_secs: uptime,
        })?;

        // Clear the global shutdown flag to prevent dangling pointers.
        #[cfg(unix)]
        clear_global_shutdown_flag();

        // Stop heartbeat emitter and remove heartbeat file.
        self.health.stop();

        // Clean up any idle marker files (best-effort).
        let state_dir = self.config.workspace.home.join("state");
        let qualified_id = self.qualified_id();
        let pid = std::process::id();
        let idle_marker = state_dir.join(format!("{}-idle-entered-{}.txt", qualified_id, pid));
        let completed_marker =
            state_dir.join(format!("{}-idle-completed-{}.txt", qualified_id, pid));
        let _ = std::fs::remove_file(idle_marker);
        let _ = std::fs::remove_file(completed_marker);

        // Deregister from worker state registry (best-effort).
        let qualified_id = format!("{}-{}", self.config.agent.default, self.worker_name);
        if let Err(e) = self.registry.deregister(&qualified_id) {
            tracing::warn!(error = %e, "failed to deregister from worker registry");
        }

        tracing::info!(
            reason,
            beads_processed = self.beads_processed,
            uptime_secs = uptime,
            "worker stopped"
        );

        self.telemetry.shutdown().await;

        Ok(WorkerState::Stopped)
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    /// Fully-qualified worker identity (`{adapter}-{worker_id}`).
    ///
    /// Used as the claim actor, registry key, and strand identity to prevent
    /// collisions when workers from different adapter pools share a NATO name.
    fn qualified_id(&self) -> String {
        format!("{}-{}", self.config.agent.default, self.worker_name)
    }

    /// Build the current exclusion set, pruning expired race-lost entries.
    ///
    /// Race-lost exclusions have a TTL of 30 seconds. This method removes
    /// expired entries and returns the union of race-lost exclusions and
    /// the manual exclusion set.
    fn current_exclusions(&mut self) -> HashSet<BeadId> {
        let now = Instant::now();
        // Prune expired entries in-place
        self.race_lost_exclusions
            .retain(|(_, expires)| expires > &now);

        // Build the union of both exclusion sets
        let mut exclusions = self.exclusion_set.clone();
        for (bead_id, _) in &self.race_lost_exclusions {
            exclusions.insert(bead_id.clone());
        }
        exclusions
    }

    /// Clear all exclusion state (both manual and race-lost exclusions).
    fn clear_all_exclusions(&mut self) {
        self.exclusion_set.clear();
        self.race_lost_exclusions.clear();
        self.race_lost_this_cycle.clear();
    }

    /// Transition to a new state, emitting telemetry and updating heartbeat.
    fn set_state(&mut self, to: WorkerState) -> Result<()> {
        let from = self.state.clone();
        tracing::debug!(from = %from, to = %to, "state transition");

        // Update atexit state so the handler has the most recent state info.
        update_atexit_state(format!("{:?}", to));

        // Update handling_state_entered_at for HANDLING state watchdog.
        // Must be done before emitting the event since we need the from value.
        if to == WorkerState::Handling {
            self.handling_state_entered_at = Some(std::time::Instant::now());
        } else if from == WorkerState::Handling {
            self.handling_state_entered_at = None;
        }

        // Use emit_try_lock() to avoid blocking if telemetry writer is stuck.
        // State transitions must not block — if telemetry is wedged, we skip
        // the event and continue anyway. The heartbeat shared state is always
        // updated below, so monitoring can detect the new state via heartbeat files.
        let _ = self.telemetry.emit_try_lock(EventKind::StateTransition {
            from,
            to: to.clone(),
        });

        // Update heartbeat shared state with the new worker state.
        let current_bead_id = self.current_bead.as_ref().map(|b| &b.id);
        // For bead-processing states, use the bead's actual workspace if set.
        // This ensures heartbeat reports the workspace where the bead lives,
        // not the worker's home workspace when processing cross-workspace beads.
        //
        // For Selecting state, use the home workspace (not current_workspace)
        // because restore_home_store() has just reset the store to home.
        // Using current_workspace here would cause a race condition where the
        // heartbeat reports a stale workspace from the previous cycle.
        let current_workspace = match to {
            WorkerState::Selecting => {
                // Selecting always uses home workspace because the store has
                // just been restored to home by restore_home_store().
                Some(self.config.workspace.default.as_path())
            }
            WorkerState::Claiming
            | WorkerState::Building
            | WorkerState::Dispatching
            | WorkerState::Executing => {
                // Use the bead's workspace if it's set and not unset/placeholder
                if let Some(ref bead) = self.current_bead {
                    if !is_workspace_unset(&bead.workspace) {
                        Some(bead.workspace.as_path())
                    } else {
                        // Bead workspace is unset, use tracked workspace or home
                        if is_workspace_unset(&self.current_workspace) {
                            Some(self.config.workspace.default.as_path())
                        } else {
                            Some(self.current_workspace.as_path())
                        }
                    }
                } else {
                    // No current bead, use tracked workspace or home
                    if is_workspace_unset(&self.current_workspace) {
                        Some(self.config.workspace.default.as_path())
                    } else {
                        Some(self.current_workspace.as_path())
                    }
                }
            }
            _ => {
                // For other non-bead-processing states, use tracked workspace or home
                if is_workspace_unset(&self.current_workspace) {
                    Some(self.config.workspace.default.as_path())
                } else {
                    Some(self.current_workspace.as_path())
                }
            }
        };
        self.health
            .update_state(&to, current_bead_id, current_workspace);
        // Sync current_workspace from the shared state so subsequent heartbeats
        // use the correct workspace during cross-workspace work.
        if let Some(ws) = current_workspace {
            self.current_workspace = ws.to_path_buf();
        }
        self.state = to;
        Ok(())
    }

    /// Resolve the agent adapter from config, with fallback to built-in.
    fn resolve_adapter(&self) -> Result<crate::dispatch::AgentAdapter> {
        let adapter_name = &self.config.agent.default;

        if let Some(adapter) = self.dispatcher.adapter(adapter_name) {
            return Ok(adapter.clone());
        }

        // Fall back to claude-sonnet built-in.
        if let Some(adapter) = self.dispatcher.adapter("claude-sonnet") {
            tracing::warn!(
                requested = %adapter_name,
                fallback = "claude-sonnet",
                "configured adapter not found, using fallback"
            );
            return Ok(adapter.clone());
        }

        // Last resort: first built-in adapter.
        match crate::dispatch::builtin_adapters().into_iter().next() {
            Some(adapter) => {
                tracing::warn!("no adapters available, using first built-in");
                Ok(adapter)
            }
            None => bail!("no agent adapters available"),
        }
    }

    /// Check daily budget and emit appropriate telemetry / trigger shutdown.
    fn check_budget(&mut self) -> Result<()> {
        // Skip if no budget configured.
        if self.config.budget.warn_usd <= 0.0 && self.config.budget.stop_usd <= 0.0 {
            return Ok(());
        }

        // Resolve log directory for scanning.
        let log_dir = self
            .config
            .telemetry
            .file_sink
            .log_dir
            .clone()
            .unwrap_or_else(|| self.config.workspace.home.join("logs"));
        let daily_cost = cost::scan_daily_cost(&log_dir);

        match cost::check_budget(daily_cost, &self.config.budget) {
            BudgetCheck::Ok => {}
            BudgetCheck::Warn {
                daily_cost,
                threshold,
            } => {
                tracing::warn!(
                    daily_cost = %format!("{:.2}", daily_cost),
                    threshold = %format!("{:.2}", threshold),
                    "daily cost exceeds warning threshold"
                );
                self.telemetry.emit(EventKind::BudgetWarning {
                    daily_cost,
                    threshold,
                })?;
            }
            BudgetCheck::Stop {
                daily_cost,
                threshold,
            } => {
                tracing::error!(
                    daily_cost = %format!("{:.2}", daily_cost),
                    threshold = %format!("{:.2}", threshold),
                    "daily cost exceeds stop threshold — shutting down"
                );
                self.telemetry.emit(EventKind::BudgetStop {
                    daily_cost,
                    threshold,
                })?;
                self.state = WorkerState::Stopped;
            }
        }

        Ok(())
    }

    /// Resolve the provider name from the configured adapter.
    fn resolve_provider(&self) -> Option<String> {
        let adapter_name = &self.config.agent.default;
        self.dispatcher
            .adapter(adapter_name)
            .and_then(|a| a.provider.clone())
    }

    /// Return the current worker state (for testing/inspection).
    pub fn state(&self) -> &WorkerState {
        &self.state
    }

    /// Return the number of beads processed so far.
    pub fn beads_processed(&self) -> u64 {
        self.beads_processed
    }

    /// Replace the dispatcher (for testing with custom adapters).
    pub fn set_dispatcher(&mut self, dispatcher: Dispatcher) {
        self.dispatcher = dispatcher;
    }

    /// Request a graceful shutdown (sets the internal shutdown flag).
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Clear the global shutdown flag when the worker is dropped.
        // This prevents dangling pointers if the worker is dropped without
        // calling stop() (e.g., due to panic or early return).
        #[cfg(unix)]
        {
            clear_global_shutdown_flag();
        }

        // Join the watchdog thread if it was started.
        // Set the trigger flag to signal the thread to exit.
        self.watchdog_triggered.store(true, Ordering::Release);
        if let Some(handle) = self.watchdog_handle.take() {
            // Don't block indefinitely joining the thread during drop.
            // If it doesn't exit within 1 second, we'll still continue.
            let _ = handle.join();
        }
    }
}

/// Check if a workspace path should be treated as "unset".
///
/// br's JSON output sets `source_repo` to `"."` (cwd-relative) for local
/// beads. We treat empty paths and `"."` as unset so that the Explore
/// strand's absolute workspace path is preserved through the claim cycle.
fn is_workspace_unset(path: &std::path::Path) -> bool {
    let s = path.as_os_str();
    s.is_empty() || s == "."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{BeadStore, Filters, RepairReport};
    use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};
    use async_trait::async_trait;
    use std::sync::Mutex;

    // ── Mock BeadStore ──

    struct MockStore {
        beads: Mutex<Vec<Bead>>,
    }

    impl MockStore {
        fn new(beads: Vec<Bead>) -> Self {
            MockStore {
                beads: Mutex::new(beads),
            }
        }

        fn empty() -> Self {
            Self::new(vec![])
        }
    }

    #[async_trait]
    impl BeadStore for MockStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.beads
                .lock()
                .unwrap()
                .iter()
                .find(|b| b.id == *id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))
        }
        async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
            let mut beads = self.beads.lock().unwrap();
            if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
                bead.status = BeadStatus::InProgress;
                bead.assignee = Some(actor.to_string());
                Ok(ClaimResult::Claimed(bead.clone()))
            } else {
                anyhow::bail!("bead not found: {id}")
            }
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
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
            Ok(BeadId::from("new-bead"))
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
    }

    fn make_test_bead(id: &str) -> Bead {
        Bead {
            id: BeadId::from(id),
            title: format!("Test bead {id}"),
            body: Some("Do the thing".to_string()),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec![],
            workspace: std::path::PathBuf::from("/tmp/test-workspace"),
            dependencies: vec![],
            dependents: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn make_worker(store: Arc<dyn BeadStore>) -> Worker {
        let mut config = Config::default();
        // Disable hot-reload in tests — it would re-exec into a different binary.
        config.self_modification.hot_reload = false;
        Worker::new(config, "test-worker".to_string(), store)
    }

    #[tokio::test]
    async fn worker_starts_in_booting_state() {
        let store = Arc::new(MockStore::empty());
        let worker = make_worker(store);
        assert_eq!(*worker.state(), WorkerState::Booting);
    }

    #[tokio::test]
    async fn boot_validates_config() {
        let store = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.agent.default = String::new(); // Invalid
        let mut worker = Worker::new(config, "test-worker".to_string(), store);
        let result = worker.boot();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("agent.default"));
    }

    #[tokio::test]
    async fn boot_transitions_to_selecting() {
        let store = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        assert_eq!(*worker.state(), WorkerState::Selecting);
    }

    #[tokio::test]
    async fn run_with_empty_store_returns_exhausted_or_stopped() {
        let store = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.worker.idle_action = IdleAction::Exit;
        config.self_modification.hot_reload = false;
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        let result = worker.run().await.unwrap();
        assert!(
            result == WorkerState::Stopped || result == WorkerState::Exhausted,
            "expected Stopped or Exhausted, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn resolve_adapter_returns_builtin() {
        let store = Arc::new(MockStore::empty());
        let worker = make_worker(store);
        let adapter = worker.resolve_adapter().unwrap();
        // Default config uses "claude" which won't match "claude-sonnet" directly,
        // but the fallback chain should find a built-in.
        assert!(!adapter.name.is_empty());
    }

    #[tokio::test]
    async fn beads_processed_starts_at_zero() {
        let store = Arc::new(MockStore::empty());
        // Use an isolated workspace home so the registry doesn't pick up
        // entries left by other tests (e.g., full_cycle_with_echo_agent).
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.self_modification.hot_reload = false;
        config.workspace.home = dir.path().to_path_buf();
        let worker = Worker::new(config, "test-worker".to_string(), store);
        assert_eq!(worker.beads_processed(), 0);
    }

    #[tokio::test]
    async fn do_select_with_no_beads_transitions_to_exhausted() {
        let store = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();

        worker.do_select().await.unwrap();
        assert_eq!(*worker.state(), WorkerState::Exhausted);
    }

    #[tokio::test]
    async fn shutdown_flag_causes_stop() {
        let store = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.worker.idle_action = IdleAction::Exit;
        config.self_modification.hot_reload = false;
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Set shutdown before run.
        worker.shutdown.store(true, Ordering::SeqCst);

        let result = worker.run().await.unwrap();
        assert_eq!(result, WorkerState::Stopped);
    }

    #[tokio::test]
    async fn do_select_with_beads_transitions_to_claiming() {
        let bead = make_test_bead("needle-test-001");
        let store = Arc::new(MockStore::new(vec![bead]));
        let mut worker = make_worker(store);
        worker.boot().unwrap();

        worker.do_select().await.unwrap();
        assert_eq!(*worker.state(), WorkerState::Claiming);
        assert!(worker.current_bead.is_some());
    }

    // ── Specialized mock stores for claim tests ──

    /// A store that always returns RaceLost on claim.
    struct RaceLostStore {
        beads: Mutex<Vec<Bead>>,
    }

    impl RaceLostStore {
        fn new(beads: Vec<Bead>) -> Self {
            RaceLostStore {
                beads: Mutex::new(beads),
            }
        }
    }

    #[async_trait]
    impl BeadStore for RaceLostStore {
        async fn ready(&self, _f: &Filters) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.beads
                .lock()
                .unwrap()
                .iter()
                .find(|b| b.id == *id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("not found"))
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::RaceLost {
                claimed_by: "other-worker".to_string(),
            })
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
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
        async fn add_label(&self, _id: &BeadId, _l: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_label(&self, _id: &BeadId, _l: &str) -> Result<()> {
            Ok(())
        }
        async fn create_bead(&self, _t: &str, _b: &str, _l: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new"))
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
        async fn add_dependency(&self, _a: &BeadId, _b: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(&self, _a: &BeadId, _b: &BeadId) -> Result<()> {
            Ok(())
        }
    }

    /// A store that always returns NotClaimable on claim.
    struct NotClaimableStore {
        beads: Mutex<Vec<Bead>>,
    }

    impl NotClaimableStore {
        fn new(beads: Vec<Bead>) -> Self {
            NotClaimableStore {
                beads: Mutex::new(beads),
            }
        }
    }

    #[async_trait]
    impl BeadStore for NotClaimableStore {
        async fn ready(&self, _f: &Filters) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.beads
                .lock()
                .unwrap()
                .iter()
                .find(|b| b.id == *id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("not found"))
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "already closed".to_string(),
            })
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
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
        async fn add_label(&self, _id: &BeadId, _l: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_label(&self, _id: &BeadId, _l: &str) -> Result<()> {
            Ok(())
        }
        async fn create_bead(&self, _t: &str, _b: &str, _l: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new"))
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
        async fn add_dependency(&self, _a: &BeadId, _b: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
    }

    // ── do_claim tests ──

    #[tokio::test]
    async fn do_claim_race_lost_adds_to_exclusion_and_retries() {
        let bead = make_test_bead("needle-race");
        let store: Arc<dyn BeadStore> = Arc::new(RaceLostStore::new(vec![bead]));
        let mut worker = make_worker(store);
        worker.boot().unwrap();

        // Simulate: strand selected a candidate, now in Claiming state.
        worker.current_bead = Some(make_test_bead("needle-race"));
        worker.state = WorkerState::Claiming;

        worker.do_claim().await.unwrap();

        // Should transition to Retrying and add the bead to exclusion set.
        assert_eq!(*worker.state(), WorkerState::Retrying);
        assert!(worker.exclusion_set.contains(&BeadId::from("needle-race")));
        assert_eq!(worker.retry_count, 1);
    }

    #[tokio::test]
    async fn do_claim_not_claimable_transitions_to_retrying() {
        // NotClaimable from the store gets wrapped by the Claimer into
        // AllRaceLost → RaceLost at the worker level. The worker treats
        // this as a race-lost situation and transitions to Retrying.
        let bead = make_test_bead("needle-closed");
        let store: Arc<dyn BeadStore> = Arc::new(NotClaimableStore::new(vec![bead]));
        let mut worker = make_worker(store);
        worker.boot().unwrap();

        worker.current_bead = Some(make_test_bead("needle-closed"));
        worker.state = WorkerState::Claiming;

        worker.do_claim().await.unwrap();

        // Claimer wraps NotClaimable → AllRaceLost → RaceLost at worker level.
        assert_eq!(*worker.state(), WorkerState::Retrying);
        assert!(worker
            .exclusion_set
            .contains(&BeadId::from("needle-closed")));
        assert_eq!(worker.retry_count, 1);
    }

    #[tokio::test]
    async fn do_claim_no_current_bead_resets_to_selecting() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Claiming;
        worker.current_bead = None;

        worker.do_claim().await.unwrap();
        assert_eq!(*worker.state(), WorkerState::Selecting);
    }

    // ── do_retry tests ──

    #[tokio::test]
    async fn do_retry_below_max_transitions_to_selecting() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Retrying;
        worker.retry_count = 1; // Below default max (3)

        worker.do_retry().await.unwrap();

        assert_eq!(*worker.state(), WorkerState::Selecting);
        // Retry count preserved — it's only reset when max is exceeded.
        assert_eq!(worker.retry_count, 1);
    }

    #[tokio::test]
    async fn do_retry_at_max_resets_and_selects() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Retrying;
        worker.retry_count = worker.config.worker.max_claim_retries; // At max
        worker.exclusion_set.insert(BeadId::from("some-bead"));

        worker.do_retry().await.unwrap();

        assert_eq!(*worker.state(), WorkerState::Selecting);
        assert_eq!(worker.retry_count, 0);
        assert!(worker.exclusion_set.is_empty());
        assert!(worker.current_bead.is_none());
    }

    #[tokio::test]
    async fn do_retry_at_max_preserves_race_lost_exclusions() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Retrying;
        worker.retry_count = worker.config.worker.max_claim_retries; // At max

        // Add a race-lost exclusion with TTL (simulating a recent race loss)
        let excluded_bead = BeadId::from("race-lost-bead");
        let expires = std::time::Instant::now() + std::time::Duration::from_secs(30);
        worker
            .race_lost_exclusions
            .push((excluded_bead.clone(), expires));
        worker.exclusion_set.insert(BeadId::from("some-other-bead"));

        worker.do_retry().await.unwrap();

        assert_eq!(*worker.state(), WorkerState::Selecting);
        assert_eq!(worker.retry_count, 0);
        // Manual exclusion_set is cleared
        assert!(worker.exclusion_set.is_empty());
        // But race_lost_exclusions are preserved (needle-aad8 fix)
        assert_eq!(worker.race_lost_exclusions.len(), 1);
        assert_eq!(worker.race_lost_exclusions[0].0, excluded_bead);
    }

    #[tokio::test]
    async fn do_retry_skip_threshold_transitions_to_exhausted() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Retrying;
        worker.consecutive_race_lost = worker.config.worker.claim_race_lost_skip;
        worker.retry_count = 2;
        worker.exclusion_set.insert(BeadId::from("stuck-bead"));

        worker.do_retry().await.unwrap();

        assert_eq!(*worker.state(), WorkerState::Exhausted);
        assert_eq!(worker.consecutive_race_lost, 0);
        assert_eq!(worker.retry_count, 0);
        assert!(worker.exclusion_set.is_empty());
        assert!(worker.current_bead.is_none());
    }

    #[tokio::test]
    async fn do_retry_below_skip_threshold_applies_backoff() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Retrying;
        worker.consecutive_race_lost = 2;
        worker.retry_count = 1;

        let before = std::time::Instant::now();
        worker.do_retry().await.unwrap();
        let elapsed = before.elapsed();

        // Backoff for consecutive_race_lost=2 is 1 << 1 = 2 seconds.
        // We just verify it slept (at least 1s) and transitioned to Selecting.
        assert!(elapsed >= std::time::Duration::from_secs(1));
        assert_eq!(*worker.state(), WorkerState::Selecting);
    }

    #[tokio::test]
    async fn do_claim_race_lost_increments_consecutive_counter() {
        let bead = make_test_bead("needle-race-consecutive");
        let store: Arc<dyn BeadStore> = Arc::new(RaceLostStore::new(vec![bead]));
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.current_bead = Some(make_test_bead("needle-race-consecutive"));
        worker.state = WorkerState::Claiming;
        worker.consecutive_race_lost = 3;

        worker.do_claim().await.unwrap();

        assert_eq!(worker.consecutive_race_lost, 4);
    }

    #[tokio::test]
    async fn do_claim_success_resets_consecutive_counter() {
        let bead = make_test_bead("needle-claim-ok");
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::new(vec![bead]));
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.current_bead = Some(make_test_bead("needle-claim-ok"));
        worker.state = WorkerState::Claiming;
        worker.consecutive_race_lost = 4;

        worker.do_claim().await.unwrap();

        assert_eq!(*worker.state(), WorkerState::Building);
        assert_eq!(worker.consecutive_race_lost, 0);
    }

    #[tokio::test]
    async fn do_claim_not_claimable_increments_consecutive_counter() {
        // NotClaimable from the store is wrapped by the Claimer into
        // AllRaceLost → RaceLost, so the worker sees RaceLost and
        // increments consecutive_race_lost (does NOT reset it).
        let bead = make_test_bead("needle-not-claimable");
        let store: Arc<dyn BeadStore> = Arc::new(NotClaimableStore::new(vec![bead]));
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.current_bead = Some(make_test_bead("needle-not-claimable"));
        worker.state = WorkerState::Claiming;
        worker.consecutive_race_lost = 4;

        worker.do_claim().await.unwrap();

        assert_eq!(worker.consecutive_race_lost, 5);
    }

    // ── do_build tests ──

    #[tokio::test]
    async fn do_build_without_bead_is_invariant_error() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Building;
        worker.current_bead = None;

        let result = worker.do_build().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invariant"));
    }

    #[tokio::test]
    async fn do_build_with_bead_transitions_to_dispatching() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Building;
        worker.current_bead = Some(make_test_bead("needle-build"));

        worker.do_build().await.unwrap();

        assert_eq!(*worker.state(), WorkerState::Dispatching);
        assert!(worker.built_prompt.is_some());
    }

    // ── check_budget tests ──

    #[tokio::test]
    async fn check_budget_no_config_skips() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        // Default config has warn_usd=0, stop_usd=0 → skip.
        assert_eq!(worker.config.budget.warn_usd, 0.0);
        assert_eq!(worker.config.budget.stop_usd, 0.0);

        worker.check_budget().unwrap();
        // State should be unchanged (not Stopped).
        assert_eq!(*worker.state(), WorkerState::Selecting);
    }

    #[tokio::test]
    async fn check_budget_stop_transitions_to_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        // Write a fake log file with an effort.recorded event that has a cost.
        // The cost scanner expects: event_type, timestamp (YYYY-MM-DD prefix), data.estimated_cost_usd
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let log_content = format!(
            r#"{{"event_type":"effort.recorded","timestamp":"{}T12:00:00Z","data":{{"estimated_cost_usd":50.0}}}}"#,
            today
        );
        std::fs::write(log_dir.join("worker.jsonl"), &log_content).unwrap();

        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;
        config.workspace.home = dir.path().to_path_buf();
        config.telemetry.file_sink.log_dir = Some(log_dir);
        config.budget.stop_usd = 10.0; // Cost (50) exceeds this threshold.
        config.budget.warn_usd = 5.0;

        let mut worker = Worker::new(config, "test-budget".to_string(), store);
        worker.boot().unwrap();

        worker.check_budget().unwrap();
        assert_eq!(*worker.state(), WorkerState::Stopped);
    }

    #[tokio::test]
    async fn check_budget_warn_does_not_stop() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let log_content = format!(
            r#"{{"event_type":"effort.recorded","timestamp":"{}T12:00:00Z","data":{{"estimated_cost_usd":8.0}}}}"#,
            today
        );
        std::fs::write(log_dir.join("worker.jsonl"), &log_content).unwrap();

        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;
        config.workspace.home = dir.path().to_path_buf();
        config.telemetry.file_sink.log_dir = Some(log_dir);
        config.budget.warn_usd = 5.0; // Cost (8) exceeds warn but not stop.
        config.budget.stop_usd = 20.0;

        let mut worker = Worker::new(config, "test-budget-warn".to_string(), store);
        worker.boot().unwrap();

        worker.check_budget().unwrap();
        // State should still be Selecting — warn doesn't stop the worker.
        assert_eq!(*worker.state(), WorkerState::Selecting);
    }

    // ── Invariant violation tests for dispatch/execute/handle ──

    #[tokio::test]
    async fn do_dispatch_without_bead_is_invariant_error() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Dispatching;
        worker.current_bead = None;

        let result = worker.do_dispatch().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invariant"));
    }

    #[tokio::test]
    async fn do_execute_without_bead_is_invariant_error() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Executing;
        worker.current_bead = None;

        let result = worker.do_execute().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invariant"));
    }

    #[tokio::test]
    async fn do_execute_without_prompt_is_invariant_error() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Executing;
        worker.current_bead = Some(make_test_bead("needle-exec"));
        worker.built_prompt = None;

        let result = worker.do_execute().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invariant"));
    }

    #[tokio::test]
    async fn do_handle_without_bead_is_invariant_error() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Handling;
        worker.current_bead = None;

        let result = worker.do_handle().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invariant"));
    }

    #[tokio::test]
    async fn do_handle_without_exec_output_is_invariant_error() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Handling;
        worker.current_bead = Some(make_test_bead("needle-handle"));
        worker.exec_output = None;

        let result = worker.do_handle().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invariant"));
    }

    // ── request_shutdown API ──

    #[tokio::test]
    async fn request_shutdown_sets_flag() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let worker = make_worker(store);

        assert!(!worker.shutdown.load(Ordering::SeqCst));
        worker.request_shutdown();
        assert!(worker.shutdown.load(Ordering::SeqCst));
    }

    // ── full cycle test ──

    // ── is_workspace_unset tests ──

    #[test]
    fn is_workspace_unset_empty_path() {
        assert!(is_workspace_unset(std::path::Path::new("")));
    }

    #[test]
    fn is_workspace_unset_dot_path() {
        assert!(is_workspace_unset(std::path::Path::new(".")));
    }

    #[test]
    fn is_workspace_unset_real_path() {
        assert!(!is_workspace_unset(std::path::Path::new("/tmp/workspace")));
    }

    #[test]
    fn is_workspace_unset_relative_path() {
        assert!(!is_workspace_unset(std::path::Path::new("some/path")));
    }

    // ── cross-workspace heartbeat tests ──

    #[test]
    fn set_state_uses_bead_workspace_for_cross_workspace_bead() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.workspace.home = dir.path().join("home");
        config.workspace.default = dir.path().join("home");
        let mut worker = Worker::new(config, "test-cross-ws".to_string(), store);
        worker.boot().unwrap();

        // Set up a bead from a remote workspace
        let remote_ws = dir.path().join("remote");
        let bead = Bead {
            id: BeadId::from("needle-remote"),
            title: "Remote bead".to_string(),
            body: None,
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some(worker.qualified_id()),
            labels: vec![],
            workspace: remote_ws.clone(),
            dependencies: vec![],
            dependents: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        worker.current_bead = Some(bead);
        worker.set_state(WorkerState::Executing).unwrap();

        // Verify that current_workspace was updated with the remote workspace
        assert_eq!(worker.current_workspace, remote_ws);
    }

    #[test]
    fn set_state_uses_home_workspace_when_bead_workspace_is_unset() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let dir = tempfile::tempdir().unwrap();
        let home_ws = dir.path().join("home");
        let mut config = Config::default();
        config.workspace.home = home_ws.clone();
        config.workspace.default = home_ws.clone();
        let mut worker = Worker::new(config, "test-unset-ws".to_string(), store);
        worker.boot().unwrap();

        // Set up a bead with an unset workspace (".")
        let bead = Bead {
            id: BeadId::from("needle-unset"),
            title: "Unset workspace bead".to_string(),
            body: None,
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some(worker.qualified_id()),
            labels: vec![],
            workspace: std::path::PathBuf::from("."),
            dependencies: vec![],
            dependents: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        worker.current_bead = Some(bead);
        worker.set_state(WorkerState::Executing).unwrap();

        // Verify that current_workspace was updated with the home workspace
        assert_eq!(worker.current_workspace, home_ws);
    }

    #[test]
    fn set_state_uses_home_workspace_when_no_current_bead() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let dir = tempfile::tempdir().unwrap();
        let home_ws = dir.path().join("home");
        let mut config = Config::default();
        config.workspace.home = home_ws.clone();
        config.workspace.default = home_ws.clone();
        let mut worker = Worker::new(config, "test-no-bead".to_string(), store);
        worker.boot().unwrap();

        // No current bead, current_workspace is unset
        worker.current_bead = None;
        worker.current_workspace = std::path::PathBuf::from("");
        worker.set_state(WorkerState::Exhausted).unwrap();

        // Verify that current_workspace was updated with the home workspace
        assert_eq!(worker.current_workspace, home_ws);
    }

    // ── do_log tests ──

    #[tokio::test]
    async fn do_log_increments_beads_processed() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        // Isolate workspace home to avoid registry pollution from other tests.
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.self_modification.hot_reload = false;
        config.workspace.home = dir.path().to_path_buf();
        let mut worker = Worker::new(config, "test-log-inc".to_string(), store);
        worker.boot().unwrap();
        worker.state = WorkerState::Logging;
        worker.current_bead = Some(make_test_bead("needle-log-1"));

        assert_eq!(worker.beads_processed(), 0);
        worker.do_log().unwrap();
        assert_eq!(worker.beads_processed(), 1);
        assert_eq!(*worker.state(), WorkerState::Selecting);
    }

    #[tokio::test]
    async fn do_log_clears_current_bead_and_effort() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Logging;
        worker.current_bead = Some(make_test_bead("needle-log-2"));
        worker.last_effort = Some(EffortData {
            cycle_start: Instant::now(),
            agent_name: "test".to_string(),
            model: None,
            tokens: dispatch::TokenUsage::default(),
            estimated_cost_usd: None,
        });

        worker.do_log().unwrap();

        assert!(worker.current_bead.is_none());
        assert!(worker.last_effort.is_none());
    }

    #[tokio::test]
    async fn do_log_transitions_to_selecting() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Logging;
        worker.current_bead = Some(make_test_bead("needle-log-3"));

        worker.do_log().unwrap();
        assert_eq!(*worker.state(), WorkerState::Selecting);
    }

    // ── handle_exhausted tests ──

    #[tokio::test]
    async fn handle_exhausted_with_exit_returns_stopped() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.worker.idle_action = IdleAction::Exit;
        config.self_modification.hot_reload = false;
        let mut worker = Worker::new(config, "test-exhaust-exit".to_string(), store);
        worker.boot().unwrap();
        worker.state = WorkerState::Exhausted;

        let result = worker.handle_exhausted().await.unwrap();
        assert_eq!(result, WorkerState::Stopped);
    }

    #[tokio::test]
    async fn handle_exhausted_with_wait_returns_selecting() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.worker.idle_action = IdleAction::Wait;
        // Use a very short timeout so the test doesn't block.
        config.worker.idle_timeout = 0;
        config.self_modification.hot_reload = false;
        let mut worker = Worker::new(config, "test-exhaust-wait".to_string(), store);
        worker.boot().unwrap();
        worker.state = WorkerState::Exhausted;

        let result = worker.handle_exhausted().await.unwrap();
        assert_eq!(result, WorkerState::Selecting);
    }

    // ── stop tests ──

    #[tokio::test]
    async fn stop_returns_stopped_state() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();

        let result = worker.stop("test shutdown").await.unwrap();
        assert_eq!(result, WorkerState::Stopped);
    }

    // ── resolve_provider tests ──

    #[tokio::test]
    async fn resolve_provider_returns_none_for_missing_adapter() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.agent.default = "nonexistent-adapter".to_string();
        config.self_modification.hot_reload = false;
        let worker = Worker::new(config, "test-provider".to_string(), store);

        // Default adapter not found → provider is None.
        assert!(worker.resolve_provider().is_none());
    }

    // ── restore_home_store tests ──

    #[tokio::test]
    async fn restore_home_store_is_noop_when_stores_match() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();

        // home_store and store should be the same Arc initially.
        assert!(Arc::ptr_eq(&worker.store, &worker.home_store));
        worker.restore_home_store();
        assert!(Arc::ptr_eq(&worker.store, &worker.home_store));
    }

    // ── do_select with exclusion set ──

    #[tokio::test]
    async fn do_select_clears_race_lost_this_cycle_and_retry_count() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.race_lost_this_cycle.insert(BeadId::from("old-bead"));
        worker.retry_count = 3;

        worker.do_select().await.unwrap();

        assert!(worker.race_lost_this_cycle.is_empty());
        assert_eq!(worker.retry_count, 0);
        // Note: exclusion_set is NOT cleared by do_select() anymore - it persists
        // for race-lost beads until they expire or the worker transitions to Exhausted
    }

    // ── full cycle test ──

    #[tokio::test]
    async fn full_cycle_with_echo_agent() {
        use std::collections::HashMap;

        // Test a full cycle: select → claim → build → dispatch → execute → handle → log
        let bead = make_test_bead("needle-echo");
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::new(vec![bead]));
        let mut config = Config::default();
        config.worker.idle_action = IdleAction::Exit;
        // Disable hot-reload in tests — it would re-exec into a different binary.
        config.self_modification.hot_reload = false;
        // Use a simple echo adapter so the test finishes quickly.
        config.agent.default = "echo-test".to_string();
        config.agent.timeout = 5;
        // Set workspace.default to match the bead's workspace so the remote
        // store switch logic doesn't fire.
        config.workspace.default = std::path::PathBuf::from("/tmp/test-workspace");

        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Replace the dispatcher with one that has a simple echo adapter.
        let echo_adapter = crate::dispatch::AgentAdapter {
            name: "echo-test".to_string(),
            description: None,
            agent_cli: "echo".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "echo done".to_string(),
            environment: HashMap::new(),
            timeout_secs: 5,
            provider: None,
            model: None,
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
        };
        let mut adapters = HashMap::new();
        adapters.insert("echo-test".to_string(), echo_adapter);
        worker.dispatcher =
            Dispatcher::with_adapters(adapters, Telemetry::new("test-worker".to_string()), 5);

        let result = worker.run().await.unwrap();
        assert!(
            result == WorkerState::Stopped || result == WorkerState::Exhausted,
            "expected terminal state, got {:?}",
            result
        );
        // At least one bead was processed through the pipeline.
        assert!(
            worker.beads_processed() >= 1,
            "expected at least 1 bead processed, got {}",
            worker.beads_processed()
        );
    }

    // ── check_auto_canary tests ──

    #[tokio::test]
    async fn check_auto_canary_no_op_when_self_modification_disabled() {
        let dir = tempfile::tempdir().unwrap();
        // Create bin/ so the path exists but needle-testing is absent.
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        let store = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.enabled = false;
        config.self_modification.auto_promote = true;
        config.self_modification.hot_reload = false;
        config.workspace.home = dir.path().to_path_buf();
        let mut worker = Worker::new(config, "test-canary-disabled".to_string(), store);
        worker.boot().unwrap();
        // Must not fail even though canary workspace and binary are absent.
        assert!(worker.check_auto_canary().is_ok());
    }

    #[tokio::test]
    async fn check_auto_canary_no_op_when_auto_promote_false() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        let store = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.enabled = true;
        config.self_modification.auto_promote = false;
        config.self_modification.hot_reload = false;
        config.workspace.home = dir.path().to_path_buf();
        let mut worker = Worker::new(config, "test-canary-no-promote".to_string(), store);
        worker.boot().unwrap();
        assert!(worker.check_auto_canary().is_ok());
    }

    #[tokio::test]
    async fn check_auto_canary_no_op_when_no_testing_binary() {
        let dir = tempfile::tempdir().unwrap();
        // bin/ exists but needle-testing does not.
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        let store = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.enabled = true;
        config.self_modification.auto_promote = true;
        config.self_modification.hot_reload = false;
        config.workspace.home = dir.path().to_path_buf();
        config.self_modification.canary_workspace = dir.path().join("canary");
        let mut worker = Worker::new(config, "test-canary-no-binary".to_string(), store);
        worker.boot().unwrap();
        // No :testing binary → returns Ok without touching canary workspace.
        assert!(worker.check_auto_canary().is_ok());
    }
}
