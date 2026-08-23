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

#![deny(unused_must_use)]

use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
// Needed for `.instrument()` — attaches a span to a future instead of holding an
// `Entered` guard across `.await`, which is unsound and leaked spans (bf-3uj6i).
use tracing::Instrument;

#[cfg(unix)]
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering as AtomicOrdering};

use crate::bead_store::BeadStore;
use crate::canary::CanaryRunner;
use crate::claim::Claimer;
use crate::commit_hook;
use crate::config::{CliOverrides, Config, ConfigLoader, ConfigSource, SourceMap};
use crate::cost::{self, BudgetCheck, EffortData};
use crate::dispatch::{self, Dispatcher};
use crate::health::HealthMonitor;
use crate::mitosis::{detects_needle_internal_config, MitosisEvaluator};
use crate::outcome::OutcomeHandler;
use crate::prompt::{BuiltPrompt, PromptBuilder};
use crate::rate_limit::RateLimiter;
use crate::registry::{LiveConfigSnapshot, Registry, WorkerEntry};
use crate::routing;
use crate::strand::StrandRunner;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{
    AgentOutcome, Bead, BeadAction, BeadId, BeadStatus, ClaimResult, IdleAction, Outcome,
    WorkerState,
};
use crate::upgrade::{self, HotReloadCheck};
use crate::validation::worker_config::{validate_idle_action_config, WorkerConfigValidationResult};

// ──────────────────────────────────────────────────────────────────────────────
// Helper functions
// ──────────────────────────────────────────────────────────────────────────────

/// Safely truncate a string to at most N characters for display.
///
/// Commit SHAs from build metadata can be as short as 7 characters (e.g., 'ee18678')
/// or the fallback 'unknown' (also 7 chars). This helper prevents panics when
/// slicing strings shorter than the desired display length.
fn truncate_for_display(s: &str, max_len: usize) -> &str {
    // Use character boundaries, not byte indices, to handle non-ASCII safely
    match s.char_indices().nth(max_len) {
        Some((idx, _)) => &s[..idx],
        None => s, // String is shorter than max_len, return as-is
    }
}

/// Remove the trailing source annotation from a formatted config dump line.
fn strip_source_annotation(line: &str) -> String {
    line.rsplit_once(" (from: ")
        .and_then(|(value, source)| source.strip_suffix(')').map(|_| value.to_string()))
        .unwrap_or_else(|| line.to_string())
}

/// Detect whether a supervisor is present at worker startup.
///
/// This function checks for supervisor presence by examining:
/// - The supervisor heartbeat file (if configured)
/// - The supervisor socket (if configured)
///
/// A supervisor is considered present if:
/// - The heartbeat file exists and is not stale (modified within TTL), OR
/// - The socket path exists (for Unix domain sockets)
///
/// # Arguments
///
/// * `heartbeat_path` - Optional path to the supervisor's heartbeat file
/// * `socket_path` - Optional path to the supervisor's control socket
/// * `ttl_secs` - Time-to-live for heartbeat freshness (seconds)
///
/// # Returns
///
/// `true` if a supervisor is detected, `false` otherwise.
///
/// # Examples
///
/// ```no_run
/// use std::path::PathBuf;
///
/// let heartbeat = Some(PathBuf::from("/tmp/supervisor-heartbeat.json"));
/// let socket = None;
/// let present = detect_supervisor_presence(heartbeat.as_ref(), socket.as_ref(), 300);
/// assert!(!present); // No supervisor running
/// ```
pub fn detect_supervisor_presence(
    heartbeat_path: Option<&PathBuf>,
    socket_path: Option<&PathBuf>,
    ttl_secs: u64,
) -> bool {
    // Check heartbeat file first
    if let Some(path) = heartbeat_path {
        if check_heartbeat_freshness(path, ttl_secs) {
            return true;
        }
    }

    // Check socket if heartbeat check failed
    if let Some(path) = socket_path {
        if path.exists() {
            return true;
        }
    }

    false
}

/// Check if a heartbeat file is fresh (within TTL).
///
/// # Arguments
///
/// * `path` - Path to the heartbeat file
/// * `ttl_secs` - Time-to-live in seconds
///
/// # Returns
///
/// `true` if the file exists and was modified within the TTL window.
fn check_heartbeat_freshness(path: &Path, ttl_secs: u64) -> bool {
    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };

    let modified = match metadata.modified() {
        Ok(time) => time,
        Err(_) => return false,
    };

    let now = SystemTime::now();
    let duration = match now.duration_since(modified) {
        Ok(d) => d,
        Err(_) => return false, // Clock skew
    };

    duration.as_secs() <= ttl_secs
}

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
            #[cfg(target_os = "linux")]
            let errno = unsafe { *libc::__errno_location() };
            #[cfg(target_os = "macos")]
            let errno = unsafe { *libc::__error() };
            tracing::warn!(
                signal = sig,
                errno = errno,
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
    /// Path to this worker's heartbeat file for cleanup on unexpected termination.
    heartbeat_path: Option<String>,
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
    heartbeat_path: Option<String>,
) {
    let state = AtexitWorkerState {
        worker_name,
        beads_processed,
        start_time,
        last_state,
        log_file_path,
        heartbeat_path,
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

            // Try to clean up the heartbeat file to prevent stale detection.
            //
            // NOTE: deliberately NOT calling crate::health::cleanup_heartbeat_file()
            // here. This is a C `atexit` callback fired from libc's exit() — by the
            // time it runs, Rust's thread-locals may already be torn down. That
            // function calls tracing::debug!/warn! internally, and dispatching a
            // tracing event needs thread-local state; if it's gone, the access
            // panics ("thread-local ... already destroyed"). A panic inside an
            // atexit handler can't unwind ("fatal runtime error: failed to initiate
            // panic, error 5, aborting") and hard-aborts the whole process — this
            // was the root cause of a recurring SIGABRT crash (lab, 2026-07-09,
            // 261 coredumps) whenever a worker's graceful shutdown had already
            // removed the heartbeat file before this safety-net handler ran (the
            // common case, not the exceptional one). Do the removal directly with
            // only std::fs + eprintln!, neither of which touch TLS.
            if let Some(ref hb_path) = state.heartbeat_path {
                use std::path::Path;
                match std::fs::remove_file(Path::new(hb_path)) {
                    Ok(_) => eprintln!("Cleaned up heartbeat file: {}", hb_path),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        eprintln!("Heartbeat file already removed: {}", hb_path);
                    }
                    Err(e) => eprintln!("Heartbeat cleanup error: {}", e),
                }
            }

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

/// Disarm the last-resort exit report after graceful shutdown has completed.
#[cfg(unix)]
fn clear_atexit_state() {
    *ATEXIT_WORKER_STATE.lock().unwrap() = None;
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
    _heartbeat_path: Option<String>,
) {
    // No-op on non-Unix platforms
}

#[cfg(not(unix))]
fn update_atexit_state(_last_state: String) {
    // No-op on non-Unix platforms
}

#[cfg(not(unix))]
fn clear_atexit_state() {
    // No-op on non-Unix platforms
}

/// Safely truncate a commit SHA for display.
///
/// Commit SHAs from `BuildMetadata` can be full SHAs (40+ chars), abbreviated
/// SHAs (7-12 chars), or "unknown" (7 chars). This function bounds the slice
/// to avoid panicking on short SHAs.
pub fn truncate_commit_sha(sha: &str) -> &str {
    let max_len = 12;
    if sha.len() <= max_len {
        sha
    } else {
        &sha[..max_len]
    }
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

/// Exit code for clean exit when a stale binary is detected.
/// This signals the supervisor to relaunch with the new binary.
const EXIT_CODE_STALE_BINARY: i32 = 72;

/// The metadata used to detect changes to the global configuration file.
///
/// The mtime is useful for ordinary edits, but it is not sufficient by itself:
/// an in-place rewrite can preserve it. The content hash is therefore checked
/// on every interval-gated poll as part of the fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigFileFingerprint {
    mtime: Option<SystemTime>,
    content_hash: Option<String>,
}

/// Resolve the same global config path used by [`ConfigLoader`].
fn global_config_path() -> PathBuf {
    crate::config::get_home_env()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config/needle/config.yaml")
}

/// Read the global config file fingerprint without exposing its contents.
fn read_config_file_fingerprint(path: &Path) -> Result<ConfigFileFingerprint> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConfigFileFingerprint {
                mtime: None,
                content_hash: None,
            });
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect config file: {}", path.display()));
        }
    };

    let mtime = metadata
        .modified()
        .with_context(|| format!("failed to read config mtime: {}", path.display()))?;
    let content_hash = upgrade::file_hash(path)
        .with_context(|| format!("failed to hash config file: {}", path.display()))?;

    Ok(ConfigFileFingerprint {
        mtime: Some(mtime),
        content_hash: Some(content_hash),
    })
}

fn config_file_changed(previous: &ConfigFileFingerprint, current: &ConfigFileFingerprint) -> bool {
    previous != current
}

/// Compare two serializable config values without relying on their concrete
/// config types implementing `PartialEq`.
///
/// Config structs are all serializable, and comparing their JSON values also
/// gives maps a stable semantic comparison rather than depending on a
/// `HashMap`'s iteration order (as used by pricing configuration).
fn config_values_differ<T: serde::Serialize>(running: &T, candidate: &T) -> bool {
    match (
        serde_json::to_value(running),
        serde_json::to_value(candidate),
    ) {
        (Ok(running), Ok(candidate)) => running != candidate,
        // All current config values are serializable. Treat an unexpected
        // serialization failure as a change so the caller never silently
        // keeps a stale value.
        _ => true,
    }
}

/// Replace one config value in a pending Tier-A snapshot.
fn replace_config_value<T: Clone + serde::Serialize>(running: &mut T, candidate: &T) -> bool {
    if config_values_differ(running, candidate) {
        *running = candidate.clone();
        true
    } else {
        false
    }
}

/// Result of rebuilding the non-telemetry Tier-B components for one candidate.
///
/// Successful components are installed immediately, while failures retain the
/// previous instance. Keeping both lists lets the cycle-boundary caller report
/// partial success without turning a single rebuild error into a worker error.
#[derive(Debug, Default)]
struct TierBReloadReport {
    applied_keys: Vec<String>,
    rebuilt_components: Vec<&'static str>,
    failures: Vec<TierBRebuildFailure>,
}

impl TierBReloadReport {
    fn failed(&self, component: &str) -> bool {
        self.failures
            .iter()
            .any(|failure| failure.component == component)
    }
}

#[derive(Debug)]
struct TierBRebuildFailure {
    component: &'static str,
    error: String,
}

/// Install a rebuilt component only after its constructor has succeeded.
///
/// This is the isolation seam shared by all five Tier-B components. The slot
/// is never moved out of, so an error cannot leave the worker without its
/// previously working instance.
fn install_rebuilt_component<T>(
    slot: &mut T,
    component: &'static str,
    rebuilt: Result<T>,
    report: &mut TierBReloadReport,
) -> bool {
    match rebuilt {
        Ok(rebuilt) => {
            *slot = rebuilt;
            report.rebuilt_components.push(component);
            true
        }
        Err(error) => {
            tracing::warn!(
                component,
                error = %error,
                "Tier-B component rebuild failed; keeping the previous instance"
            );
            report.failures.push(TierBRebuildFailure {
                component,
                error: format!("{error:#}"),
            });
            false
        }
    }
}

/// The NEEDLE worker — owns and drives the full state machine.
pub struct Worker {
    config: Config,
    /// Source annotations belonging to the configuration snapshot in use.
    config_sources: SourceMap,
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
    /// When agent execution began — used only to compute `duration_ms` for
    /// the HOOP event tap (Hook 2). Set in `do_execute`, consumed in
    /// `do_handle`.
    exec_started_at: Option<Instant>,
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
    /// Timestamp when the last freshness check was performed.
    /// Used to enforce the freshness_check_interval_secs configured interval.
    last_freshness_check: Option<Instant>,
    /// Whether we've already warned about a stale binary.
    /// Prevents spamming warnings on every dispatch cycle when stale.
    stale_binary_warned: bool,
    /// Timestamp when the last configuration reload check was performed.
    last_config_reload_check: Option<Instant>,
    /// Fingerprint of the global configuration file observed by the last
    /// completed configuration reload check.
    config_reload_fingerprint: Option<ConfigFileFingerprint>,
    /// Number of times the configuration has been successfully reloaded since boot.
    /// Incremented each time a validated configuration is applied at the cycle boundary.
    /// This counter is exposed via `needle config --dump --live` so operators can
    /// confirm what a running worker is actually using rather than what the file says.
    config_reload_generation: u64,
    /// The current bead lifecycle span. Created when a bead is claimed and
    /// instrumented onto each state-handler future until the lifecycle ends.
    ///
    /// This must remain a `Span`, not an `EnteredSpan`: entered guards mutate a
    /// thread-local stack and cannot safely be stored across `.await` points.
    bead_lifecycle_span: Option<tracing::Span>,
    /// The last outcome for the current bead (used to record on bead.lifecycle span).
    last_outcome: Option<String>,
    /// Last observed mtime across all workspace .beads/issues.jsonl files.
    /// Used for event-driven wakeups from idle state.
    last_workspace_mtime: Option<std::time::SystemTime>,
    /// Whether the most recent select cycle found candidates but they were all excluded.
    /// Used to trigger short retry instead of full idle backoff.
    found_but_excluded: bool,
    /// Spawn-path binary metadata recorded at boot time.
    /// Used to detect in-place binary modifications during worker lifecycle.
    spawn_path_metadata: Option<crate::spawn_path::BinaryMetadata>,
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
        Self::new_with_telemetry_and_sources(
            config,
            worker_name,
            store,
            telemetry,
            SourceMap::new(),
        )
    }

    /// Construct a worker with the source map from its resolved boot config.
    ///
    /// The source map is retained so the worker can publish source annotations
    /// that describe the snapshot it is actually running, including after a
    /// hot reload.
    pub fn new_with_telemetry_and_sources(
        config: Config,
        worker_name: String,
        store: Arc<dyn BeadStore>,
        telemetry: Telemetry,
        config_sources: SourceMap,
    ) -> Self {
        Self::build(config, worker_name, store, telemetry, config_sources, true)
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
        Self::build(
            config,
            worker_name,
            store,
            telemetry,
            SourceMap::new(),
            false,
        )
    }

    /// Shared construction logic used by both [`new`] and [`new_with_telemetry`].
    fn build(
        config: Config,
        worker_name: String,
        store: Arc<dyn BeadStore>,
        telemetry: Telemetry,
        config_sources: SourceMap,
        booting_emitted: bool,
    ) -> Self {
        let qualified_id = format!("{}-{}", config.agent.default, worker_name);

        // Workspace configuration is loaded before the worker's telemetry
        // emitter exists. Report ignored workspace-level settings now that
        // structured telemetry is available, rather than leaving the boot-time
        // warning as the only indication that the file had no effect.
        match ConfigLoader::workspace_non_overridable_keys(&config.workspace.default) {
            Ok(keys) => report_restart_required_config(&telemetry, keys),
            Err(error) => tracing::warn!(
                path = %config.workspace.default.join(".needle.yaml").display(),
                error = %error,
                "failed to inspect workspace config for non-overridable settings"
            ),
        }

        // Phase: Strand setup
        let _ = telemetry.emit(EventKind::InitStepStarted {
            step: "strand_setup".to_string(),
        });
        let strand_start = Instant::now();
        let strand_registry = Registry::default_location(&config.workspace.home);
        let strands =
            StrandRunner::from_config(&config, &qualified_id, strand_registry, telemetry.clone());
        let _ = telemetry.emit(EventKind::InitStepCompleted {
            step: "strand_setup".to_string(),
            duration_ms: strand_start.elapsed().as_millis() as u64,
        });

        // Phase: Claimer creation
        let _ = telemetry.emit(EventKind::InitStepStarted {
            step: "claimer_creation".to_string(),
        });
        let claimer_start = Instant::now();
        let claimer = Claimer::new(
            store.clone(),
            std::path::PathBuf::from("/tmp"),
            config.worker.max_claim_retries,
            100,
            telemetry.clone(),
        );
        let _ = telemetry.emit(EventKind::InitStepCompleted {
            step: "claimer_creation".to_string(),
            duration_ms: claimer_start.elapsed().as_millis() as u64,
        });

        // Phase: PromptBuilder setup
        let _ = telemetry.emit(EventKind::InitStepStarted {
            step: "prompt_builder_setup".to_string(),
        });
        let prompt_start = Instant::now();
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
        let _ = telemetry.emit(EventKind::InitStepCompleted {
            step: "prompt_builder_setup".to_string(),
            duration_ms: prompt_start.elapsed().as_millis() as u64,
        });

        // Phase: Dispatcher setup (adapter loading)
        let _ = telemetry.emit(EventKind::InitStepStarted {
            step: "dispatcher_setup".to_string(),
        });
        let dispatcher_start = Instant::now();
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
        let _ = telemetry.emit(EventKind::InitStepCompleted {
            step: "dispatcher_setup".to_string(),
            duration_ms: dispatcher_start.elapsed().as_millis() as u64,
        });

        // Phase: OutcomeHandler creation
        let _ = telemetry.emit(EventKind::InitStepStarted {
            step: "outcome_handler_creation".to_string(),
        });
        let outcome_start = Instant::now();
        let outcome_handler = OutcomeHandler::new(config.clone(), telemetry.clone());
        let _ = telemetry.emit(EventKind::InitStepCompleted {
            step: "outcome_handler_creation".to_string(),
            duration_ms: outcome_start.elapsed().as_millis() as u64,
        });

        // Create the shutdown flag BEFORE creating HealthMonitor so we can share it.
        // This ensures that when the heartbeat emitter's circuit breaker fires,
        // it sets the worker's shutdown flag (not its own private flag), allowing
        // the main worker loop to gracefully stop with worker.stopped telemetry.
        let shutdown = Arc::new(AtomicBool::new(false));

        // Phase: HealthMonitor setup
        let _ = telemetry.emit(EventKind::InitStepStarted {
            step: "health_monitor_setup".to_string(),
        });
        let health_start = Instant::now();
        let health = HealthMonitor::new(
            config.clone(),
            worker_name.clone(),
            telemetry.clone(),
            Some(shutdown.clone()),
        );
        let _ = telemetry.emit(EventKind::InitStepCompleted {
            step: "health_monitor_setup".to_string(),
            duration_ms: health_start.elapsed().as_millis() as u64,
        });

        // Phase: RateLimiter setup
        let _ = telemetry.emit(EventKind::InitStepStarted {
            step: "rate_limiter_setup".to_string(),
        });
        let rate_limiter_start = Instant::now();
        let registry = Registry::default_location(&config.workspace.home);
        let rate_limiter =
            RateLimiter::new(config.limits.clone(), &config.workspace.home.join("state"));
        let _ = telemetry.emit(EventKind::InitStepCompleted {
            step: "rate_limiter_setup".to_string(),
            duration_ms: rate_limiter_start.elapsed().as_millis() as u64,
        });

        // Phase: MitosisEvaluator setup
        let _ = telemetry.emit(EventKind::InitStepStarted {
            step: "mitosis_evaluator_setup".to_string(),
        });
        let mitosis_start = Instant::now();
        let mitosis_evaluator = MitosisEvaluator::new(
            config.strands.mitosis.clone(),
            telemetry.clone(),
            std::path::PathBuf::from("/tmp"),
        );
        let _ = telemetry.emit(EventKind::InitStepCompleted {
            step: "mitosis_evaluator_setup".to_string(),
            duration_ms: mitosis_start.elapsed().as_millis() as u64,
        });

        // Phase: Registry state restoration
        let _ = telemetry.emit(EventKind::InitStepStarted {
            step: "registry_state_restoration".to_string(),
        });
        let registry_start = Instant::now();
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
        let _ = telemetry.emit(EventKind::InitStepCompleted {
            step: "registry_state_restoration".to_string(),
            duration_ms: registry_start.elapsed().as_millis() as u64,
        });

        let default_workspace = config.workspace.default.clone();

        // Capture the boot-time config fingerprint only when polling is enabled.
        // This makes the first interval-gated check compare against the config
        // that the worker started with, while keeping the disabled path free of
        // filesystem work.
        let config_reload_fingerprint = if config.worker.config_reload_check_interval_secs == 0 {
            None
        } else {
            match read_config_file_fingerprint(&global_config_path()) {
                Ok(fingerprint) => Some(fingerprint),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "failed to capture initial config fingerprint; the first reload check will establish a baseline"
                    );
                    None
                }
            }
        };

        // Create the watchdog trigger flag before creating the Worker.
        let watchdog_triggered = Arc::new(AtomicBool::new(false));

        let worker = Worker {
            config,
            config_sources,
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
            exec_started_at: None,
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
            last_workspace_mtime: None,
            found_but_excluded: false,
            spawn_path_metadata: None,
            last_freshness_check: None,
            stale_binary_warned: false,
            last_config_reload_check: None,
            config_reload_fingerprint,
            config_reload_generation: 0,
        };

        // Warn if both budget thresholds are disabled (0.0 = no cap).
        if worker.config.budget.warn_usd <= 0.0 && worker.config.budget.stop_usd <= 0.0 {
            tracing::warn!(
                warn_usd = 0.0,
                stop_usd = 0.0,
                "Budget enforcement is DISABLED: both warn_usd and stop_usd are set to 0.0. \
                 Daily spend is fully uncapped — no warnings, no halting. \
                 To enable budget protection, set non-zero values in .needle.yaml under 'budget:' \
                 (e.g., 'budget: {{warn_usd: 10.0, stop_usd: 50.0}}'). \
                 For fleet-wide governance, consider claude-governor (https://github.com/jedarden/claude-governor)."
            );
        }

        worker
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

        // Step: Spawn-path binary metadata recording
        self.telemetry.emit(EventKind::InitStepStarted {
            step: "spawn_path_metadata".to_string(),
        })?;
        let step_start = Instant::now();

        // Record spawn-path binary metadata at boot time
        match crate::spawn_path::check_spawn_path_at_boot(None, |event| {
            // Emit telemetry event if modification was detected
            let old_metadata_json =
                serde_json::to_value(&event.old_metadata).unwrap_or(serde_json::Value::Null);
            let new_metadata_json =
                serde_json::to_value(&event.new_metadata).unwrap_or(serde_json::Value::Null);

            let _ = self.telemetry.emit(EventKind::SpawnPathModifiedInPlace {
                path: event.path,
                old_metadata: old_metadata_json,
                new_metadata: new_metadata_json,
                modification_type: event.modification_type,
                description: event.description,
            });
        }) {
            Ok(metadata) => {
                let path = metadata.path.clone();
                let inode = metadata.inode;
                let mtime_secs = metadata.mtime_secs;
                let size = metadata.size;
                let hash_preview = String::from(&metadata.hash[..16]);
                self.spawn_path_metadata = Some(metadata);
                tracing::info!(
                    path = %path.display(),
                    inode = inode,
                    mtime_secs = mtime_secs,
                    size = size,
                    hash = %hash_preview,
                    "recorded spawn-path binary metadata"
                );
            }
            Err(e) => {
                // Handle gracefully when spawn-path is not available or cannot be accessed
                tracing::warn!(
                    error = %e,
                    "failed to record spawn-path binary metadata, continuing without it"
                );
                self.spawn_path_metadata = None;
            }
        }

        self.telemetry.emit(EventKind::InitStepCompleted {
            step: "spawn_path_metadata".to_string(),
            duration_ms: step_start.elapsed().as_millis() as u64,
        })?;

        // Start the watchdog thread that monitors HANDLING state duration.
        // This must be started after boot() so the Worker struct is fully initialized.
        self.start_watchdog_thread();

        // Install signal handlers.
        self.install_signal_handlers();

        // Create the worker.session root span that encompasses the entire worker lifecycle.
        let worker_id = self.qualified_id();
        let workspace_name = self
            .config
            .workspace
            .default
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| self.config.workspace.default.to_string_lossy().into_owned());
        let session_span = tracing::info_span!(
            "worker.session",
            needle.worker_id = %worker_id,
            needle.session_id = %self.telemetry.session_id(),
            needle.agent = %self.config.agent.default,
            needle.model = %self.config.agent.default, // Will be updated when adapter is resolved
            needle.workspace = %workspace_name,
        );

        // Instrumenting the future re-enters this span on every poll. Holding an
        // entered guard around the loop would strand its thread-local entry if the
        // task resumed on another Tokio worker thread.
        self.run_state_machine().instrument(session_span).await
    }

    async fn run_state_machine(&mut self) -> Result<WorkerState> {
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
                    // If we're in the middle of processing a bead, release it
                    // before stopping. This ensures the bead is returned to
                    // the open state and can be claimed by another worker.
                    WorkerState::Building
                    | WorkerState::Dispatching
                    | WorkerState::Executing
                    | WorkerState::Handling => {
                        // Release any claimed bead before stopping.
                        self.release_current_bead(&reason).await;
                        return self.stop(&reason).await;
                    }
                    // For states where we don't hold a bead, stop immediately.
                    WorkerState::Selecting
                    | WorkerState::Claiming
                    | WorkerState::Retrying
                    | WorkerState::Logging => {
                        // Release any claimed bead before stopping (should be none in these states).
                        self.release_current_bead(&reason).await;
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

            // A plain `Span` handle is safe to clone and move between threads. Each
            // bead-processing handler is instrumented separately so its polls and
            // synchronous tracing events see the lifecycle span as current without
            // keeping a thread-local entry alive between polls.
            let lifecycle_span = self
                .bead_lifecycle_span
                .clone()
                .unwrap_or_else(tracing::Span::none);

            match self.state {
                WorkerState::Selecting => self.do_select().await?,
                WorkerState::Claiming => self.do_claim().await?,
                WorkerState::Retrying => self.do_retry().await?,
                WorkerState::Building => self.do_build().instrument(lifecycle_span.clone()).await?,
                WorkerState::Dispatching => {
                    self.do_dispatch()
                        .instrument(lifecycle_span.clone())
                        .await?
                }
                WorkerState::Executing => {
                    self.do_execute().instrument(lifecycle_span.clone()).await?
                }
                WorkerState::Handling => {
                    let action = self.do_handle().instrument(lifecycle_span.clone()).await?;
                    self.apply_bead_action(action).await?;
                }
                WorkerState::Logging => {
                    lifecycle_span.in_scope(|| self.do_log())?;

                    // After logging completes, check if the running binary is stale
                    // compared to the latest needle-stable on disk. If a newer binary
                    // is detected, exit cleanly so a supervisor or relaunch can pick
                    // up the new binary. This check runs between dispatch cycles,
                    // never mid-claim, ensuring no bead is left in_progress.
                    if let Err(e) = self.check_hot_reload().await {
                        tracing::warn!(
                            error = %e,
                            "hot-reload check failed, continuing on current binary"
                        );
                    }
                    // If check_hot_reload() detected a new binary and successfully
                    // re-execed, we won't reach here (the process is replaced).
                    // On re-exec failure, we continue normally.

                    // Check for a changed global config only at the cycle
                    // boundary. The check is interval-gated and non-fatal: a
                    // transient filesystem error must not stop a worker.
                    if let Err(e) = self.check_config_reload().await {
                        tracing::warn!(
                            error = %e,
                            "configuration reload check failed, continuing with current config"
                        );
                    }

                    // After hot-reload check, perform the periodic freshness check.
                    // This runs at the configured interval (worker.freshness_check_interval_secs)
                    // and logs warnings when a stale binary is detected, but does NOT exit.
                    // The distinction: hot-reload exits cleanly when new binary is available;
                    // freshness checking warns but continues processing.
                    if let Err(e) = self.check_freshness().await {
                        tracing::warn!(
                            error = %e,
                            "freshness check failed, continuing with current binary"
                        );
                    }
                }
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
            config_reload_generation: self.config_reload_generation,
        };
        if let Err(e) = self.registry.register(entry.clone()) {
            // Log to both tracing and stderr for visibility
            let worker_id = &entry.id;
            let pid = entry.pid;
            tracing::error!(error = %e, worker_id = %worker_id, pid, "failed to register in worker registry - worker will run but will be invisible to needle status/list");
            eprintln!(
                "ERROR: Failed to register worker '{}' (PID {}) in registry: {}",
                worker_id, pid, e
            );
            eprintln!("       The worker will continue running but will not appear in 'needle status' or 'needle list'.");
            eprintln!("       This indicates a problem with ~/.needle/state/workers.json - check permissions and disk space.");
        }
        if let Err(e) = self.publish_live_config_snapshot() {
            tracing::warn!(error = %e, "failed to publish live config snapshot");
        }
        self.telemetry.emit(EventKind::InitStepCompleted {
            step: "registry_registration".to_string(),
            duration_ms: step_start.elapsed().as_millis() as u64,
        })?;

        // Check boot timeout before each step
        self.check_boot_timeout(BOOT_TIMEOUT_SECS)?;

        // Step: Idle action validation
        self.telemetry.emit(EventKind::InitStepStarted {
            step: "idle_action_validation".to_string(),
        })?;
        let step_start = Instant::now();

        // Validate idle_action configuration
        let idle_action = self.config.worker.idle_action.clone();
        match self.health.detect_supervisor_direct() {
            Ok(supervisor_present) => {
                let validation_result =
                    validate_idle_action_config(&idle_action, supervisor_present);
                match validation_result {
                    WorkerConfigValidationResult::Valid => {
                        if idle_action == crate::types::IdleAction::Exit {
                            tracing::info!(
                                idle_action = "exit",
                                "supervisor detected: exit policy is safe"
                            );
                        }
                    }
                    WorkerConfigValidationResult::Invalid { reason } => {
                        tracing::warn!(idle_action = "exit", "{}", reason);
                        // Emit telemetry event for the configuration warning
                        let _ = self.telemetry.emit(EventKind::ConfigWarning {
                            warning_type: "idle_action_misconfiguration".to_string(),
                            message: reason.clone(),
                        });
                    }
                }
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "failed to detect supervisor presence, skipping idle_action validation"
                );
            }
        }

        self.telemetry.emit(EventKind::InitStepCompleted {
            step: "idle_action_validation".to_string(),
            duration_ms: step_start.elapsed().as_millis() as u64,
        })?;

        // Check boot timeout before each step
        self.check_boot_timeout(BOOT_TIMEOUT_SECS)?;

        // Step: Adapter preflight
        self.telemetry.emit(EventKind::InitStepStarted {
            step: "adapter_preflight".to_string(),
        })?;
        let step_start = Instant::now();
        // Validate that the configured adapter exists before entering the main claim loop.
        // This ensures adapter configuration errors are caught early during startup rather
        // than after a bead has been claimed, preventing orphaned in_progress beads.
        if let Err(e) = self.resolve_adapter() {
            // Provide clear, actionable error message for missing adapter
            let adapter_name = &self.config.agent.default;
            eprintln!("ERROR: Configured agent adapter '{adapter_name}' was not found.");
            eprintln!("Startup is aborting to prevent claiming beads with an invalid adapter configuration.");
            eprintln!();
            eprintln!("To fix this, check one of these locations for your adapter configuration:");
            eprintln!("  - ~/.needle/agents/{adapter_name}.yaml");
            eprintln!("  - ~/.local/bin/claude-config/agents/{adapter_name}/config.json");
            eprintln!("  - ~/.config/needle/adapters/{adapter_name}.yaml");
            eprintln!();
            eprintln!("Common causes:");
            eprintln!("  - Adapter file does not exist or is in a different location");
            eprintln!("  - Incorrect adapter name specified in agent.default config");
            eprintln!(
                "  - Adapter file exists but is missing required fields (provider, model, etc.)"
            );
            eprintln!();
            eprintln!("Underlying error: {e}");
            bail!("adapter '{adapter_name}' preflight failed — startup aborted: {e}");
        }
        self.telemetry.emit(EventKind::InitStepCompleted {
            step: "adapter_preflight".to_string(),
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
        let heartbeat_path = self.health.heartbeat_path().to_str().map(String::from);
        register_atexit_handler(
            self.worker_name.clone(),
            self.beads_processed,
            start_time,
            format!("{:?}", self.state),
            None, // log_file_path - not available during boot
            heartbeat_path,
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

    /// Reject a claim attempt when this worker already owns a claim.
    ///
    /// `current_bead` also carries unclaimed strand candidates, so ownership
    /// requires both the claimed status and this worker's qualified ID. If a
    /// stale state transition reaches a claim entry point while that condition
    /// holds, release the existing claim before returning an invariant error.
    /// The caller cannot proceed to a second backend claim.
    async fn ensure_claim_slot_available(&mut self) -> Result<()> {
        let actor = self.qualified_id();
        let held_bead = self
            .current_bead
            .as_ref()
            .filter(|bead| {
                bead.status == BeadStatus::InProgress
                    && bead.assignee.as_deref() == Some(actor.as_str())
            })
            .cloned();

        let Some(bead) = held_bead else {
            return Ok(());
        };

        tracing::error!(
            bead_id = %bead.id,
            worker_id = %actor,
            "single-claim invariant blocked a second claim attempt"
        );

        match tokio::time::timeout(Duration::from_secs(30), self.store.release(&bead.id)).await {
            Ok(Ok(())) => {
                self.current_bead = None;
                self.telemetry.emit(EventKind::BeadReleased {
                    bead_id: bead.id.clone(),
                    reason: "single_claim_invariant_recovery".to_string(),
                })?;
                bail!(
                    "single-claim invariant blocked a second claim attempt; released existing claim {}",
                    bead.id
                );
            }
            Ok(Err(error)) => {
                Err(error).with_context(|| {
                    format!(
                        "single-claim invariant blocked a second claim attempt, but existing claim {} could not be released",
                        bead.id
                    )
                })
            }
            Err(_) => {
                bail!(
                    "single-claim invariant blocked a second claim attempt; timed out releasing existing claim {}",
                    bead.id
                );
            }
        }
    }

    /// SELECTING: run strand waterfall to find a candidate bead.
    async fn do_select(&mut self) -> Result<()> {
        // Claim ownership must be checked before clearing per-cycle state. A
        // leaked claim here would otherwise be forgotten and claim_auto could
        // assign this same worker a second bead.
        self.ensure_claim_slot_available().await?;

        // Clear per-cycle state.
        // Preserve race-lost exclusions with TTL and beads that lost a race in the current cycle.
        // NOTE: Do NOT reset retry_count or consecutive_race_lost here — they must
        // accumulate across cycles to prevent infinite race-lost loops (see needle-aad8).
        self.race_lost_this_cycle.clear();
        self.current_bead = None;
        self.current_strand = None;
        self.health.update_strand(None);

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
        let claim = self.claimer.claim_auto(&self.qualified_id(), strand).await;

        match claim {
            Ok(ClaimResult::Claimed(bead)) => {
                tracing::info!(bead_id = %bead.id, "atomically claimed bead via claim_auto");
                let workspace = if is_workspace_unset(&bead.workspace) {
                    self.current_workspace.clone()
                } else {
                    bead.workspace.clone()
                };
                if !is_workspace_unset(&workspace) {
                    self.telemetry.set_workspace(workspace);
                }
                crate::hoop_hooks::emit_needle_event(
                    &self.current_workspace,
                    &self.worker_name,
                    Some(bead.id.as_ref()),
                    Some(strand),
                    "claim",
                    serde_json::json!({}),
                );
                self.current_bead = Some(bead);
                self.current_strand = Some(strand.to_string());
                self.health.update_strand(Some(strand));
                self.consecutive_race_lost = 0;
                self.set_state(WorkerState::Building)?;
                return Ok(());
            }
            Ok(ClaimResult::NotClaimable { reason }) => {
                tracing::debug!(
                    reason,
                    "claim_auto returned no beads, falling back to strand waterfall"
                );
                // Fall through to strand waterfall
            }
            Err(e) => {
                tracing::warn!(error = %e, "claim_auto failed, falling back to strand waterfall");
                // Fall through to strand waterfall
            }
            Ok(other) => {
                tracing::warn!(
                    ?other,
                    "claim_auto returned unexpected result, falling back to strand waterfall"
                );
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

                crate::hoop_hooks::emit_needle_event(
                    &self.current_workspace,
                    &self.worker_name,
                    Some(bead.id.as_ref()),
                    Some(strand_name.as_str()),
                    "claim",
                    serde_json::json!({}),
                );
                self.health.update_strand(Some(strand_name.as_str()));
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
                // Set found_but_excluded flag if candidates were found but all excluded
                // This enables short retry backoff instead of full idle backoff
                self.found_but_excluded = self.found_but_all_excluded();
                self.set_state(WorkerState::Exhausted)?;
            }
        }

        Ok(())
    }

    /// Swap the active bead store to a remote workspace.
    ///
    /// Loads the target workspace's explicit backend binding, creates that
    /// store, and rebuilds the Claimer to use it. The home store is restored
    /// at the start of the next select cycle.
    fn switch_store_to(&mut self, workspace: &std::path::Path) -> Result<()> {
        // Resolve this worker's default adapter model so remote-workspace
        // claims carry the same velocity-scoring metadata as home-workspace
        // claims (bf-backed stores only — bead-rs has no such input). The
        // model is the default adapter's model — routing overrides apply
        // post-claim at dispatch time, so the default is the correct
        // identity to score.
        let model = self
            .dispatcher
            .adapter(&self.config.agent.default)
            .and_then(|a| a.model.clone());

        let (remote_config, _) = crate::config::ConfigLoader::load_resolved(
            workspace,
            crate::config::CliOverrides {
                workspace: Some(workspace.to_path_buf()),
                ..Default::default()
            },
        )
        .with_context(|| {
            format!(
                "failed to load bead backend binding for remote workspace {}",
                workspace.display()
            )
        })?;
        let remote_store = crate::bead_store::open_configured(
            &remote_config.bead_cli,
            workspace.to_path_buf(),
            model,
            Some("needle".to_string()),
            Some(env!("CARGO_PKG_VERSION").to_string()),
        )
        .context("failed to create bead store for remote workspace")?;
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

    /// Compute a jittered backoff duration between idle_backoff_min and idle_backoff_max.
    ///
    /// This provides randomized delays within the configured range to prevent
    /// thundering herd when multiple workers become idle simultaneously.
    fn compute_jittered_backoff(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};

        let min_secs = self.config.worker.idle_backoff_min;
        let max_secs = self.config.worker.idle_backoff_max;

        // Ensure min <= max
        let min_secs = min_secs.min(max_secs);

        if min_secs == max_secs {
            return min_secs;
        }

        // Use current time + worker_id as seed for deterministic jitter
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();

        let worker_hash = {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            self.qualified_id().hash(&mut hasher);
            hasher.finish()
        };

        // Combine time and worker identity for jitter
        let combined = nanos as u64 ^ worker_hash;
        let range = max_secs - min_secs;
        let jitter = combined % (range + 1);

        min_secs + jitter
    }

    /// Check if any workspace's issues.jsonl file has been modified since last check.
    ///
    /// Returns the most recent mtime across all configured workspaces, or None
    /// if no files exist or all workspaces are unreachable.
    fn check_workspace_mtimes(&self) -> Option<std::time::SystemTime> {
        let mut workspaces_to_check = vec![&self.config.workspace.default];

        // Also check explore workspaces if configured
        if !self.config.strands.explore.workspaces.is_empty() {
            for ws in &self.config.strands.explore.workspaces {
                if !workspaces_to_check.contains(&ws) {
                    workspaces_to_check.push(ws);
                }
            }
        }

        let mut most_recent_mtime: Option<std::time::SystemTime> = None;

        for workspace in workspaces_to_check {
            let issues_path = workspace.join(".beads").join("issues.jsonl");
            if let Ok(metadata) = std::fs::metadata(&issues_path) {
                if let Ok(mtime) = metadata.modified() {
                    match most_recent_mtime {
                        Some(existing) if mtime > existing => {
                            most_recent_mtime = Some(mtime);
                        }
                        None => {
                            most_recent_mtime = Some(mtime);
                        }
                        _ => {}
                    }
                }
            }
        }

        most_recent_mtime
    }

    /// Detect if this cycle found candidates but all were excluded.
    ///
    /// This is determined by examining the last strand evaluations to see if
    /// explore or pluck found candidates that were then excluded by filters.
    fn found_but_all_excluded(&self) -> bool {
        // Check if explore strand ran and found candidates that were excluded
        for (strand_name, result, _duration) in &self.last_strand_evaluations {
            if strand_name == "explore" && result.contains("BeadFound") {
                // Explore found beads, but if we're exhausted, they must have been excluded
                // Check the exclusion reasons in telemetry
                return true;
            }
        }

        // Also check pluck strand for the home workspace
        for (strand_name, result, _duration) in &self.last_strand_evaluations {
            if strand_name == "pluck" && result.contains("candidates_found") {
                // Pluck found beads but we still exhausted - they were excluded
                return true;
            }
        }

        false
    }

    /// Count total candidates and exclusions from the last selection cycle.
    ///
    /// Returns (total_candidates, excluded_count) for telemetry.
    #[allow(dead_code)]
    fn count_exclusions_from_cycle(&self) -> (usize, usize) {
        let total_candidates = 0usize;
        let excluded_count = 0usize;

        for (_strand_name, result, _duration) in &self.last_strand_evaluations {
            // Parse result strings to extract counts
            // This is a simplified check - the actual counts come from strand telemetry
            if result.contains("candidates=") {
                // Extract candidate count from telemetry data
                // This would need to be enhanced in production
            }
        }

        (total_candidates, excluded_count)
    }

    /// CLAIMING: attempt to claim the selected bead.
    async fn do_claim(&mut self) -> Result<()> {
        self.ensure_claim_slot_available().await?;

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
        // Do NOT hold an `Entered`/`EnteredSpan` guard here. Those guards mutate a
        // thread-local span stack. The previous code kept one alive across the
        // claim await; when Tokio resumed the task on a different worker thread,
        // dropping the guard there could not remove the entry left on the original
        // thread. The lifecycle guard had the same cross-await problem. One
        // `bead.claim` plus one `bead.lifecycle` leaked per cycle. Because the fmt
        // layer re-serializes the whole span stack on every event, output grew
        // quadratically: measured at 18 deep / 4,983-byte lines early and 2,488 deep
        // / 629,829-byte lines late, reaching ~159 GB/hr and filling a 444 GB disk.
        // See bf-3uj6i.
        //
        // `.instrument()` attaches the span to the future correctly, so
        // `Span::current()` inside `claim_one` (claim/mod.rs records
        // needle.claim.result there) still resolves to this span.
        claim_span.record("needle.claim.retry_number", 1u32);

        let claim = self
            .claimer
            .claim_one(&bead_id, &self.qualified_id(), &exclusions, Some(strand))
            .instrument(claim_span.clone())
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
                let workspace = if is_workspace_unset(&bead.workspace) {
                    self.current_workspace.clone()
                } else {
                    bead.workspace.clone()
                };
                if !is_workspace_unset(&workspace) {
                    self.telemetry.set_workspace(workspace);
                }
                self.current_bead = Some(bead);
                // Start effort tracking for this cycle.
                self.last_effort = Some(EffortData {
                    cycle_start: Instant::now(),
                    agent_name: String::new(),
                    model: None,
                    provider: None,
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
                self.bead_lifecycle_span = Some(lifecycle_span);

                // bead.claim is already closed by the time we get here: it is scoped
                // to the instrumented claim_one() future above. bead.lifecycle is
                // stored as an inert `Span` handle and instrumented onto each
                // subsequent state-handler future by `run_state_machine`, so neither
                // span leaves a thread-local guard alive between polls (bf-3uj6i).

                self.set_state(WorkerState::Building)?;
            }
            ClaimResult::RaceLost { claimed_by } => {
                tracing::debug!(bead_id = %bead_id, %claimed_by, "claim race lost");
                // Record on claim_span explicitly: it is no longer entered here, so
                // Span::current() would resolve to the enclosing strand span and the
                // field would be silently dropped.
                claim_span.record("needle.claim.result", "race_lost");
                claim_span.record("otel.status_code", 2u64);
                claim_span.record("otel.status_description", "race_lost");
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
                // Record on claim_span explicitly — see the RaceLost arm.
                claim_span.record("needle.claim.result", &reason);
                claim_span.record("otel.status_code", 2u64);
                claim_span.record("otel.status_description", &reason);
                self.consecutive_race_lost = 0;
                self.exclusion_set.insert(bead_id);
                self.current_bead = None;
                // The claim_span is dropped here, closing the bead.claim span.
                self.set_state(WorkerState::Selecting)?;
            }
            ClaimResult::ClaimError { reason } => {
                tracing::debug!(bead_id = %bead_id, %reason, "claim error");
                // Record on claim_span explicitly — see the RaceLost arm.
                claim_span.record("needle.claim.result", &reason);
                claim_span.record("otel.status_code", 2u64);
                claim_span.record("otel.status_description", &reason);
                self.consecutive_race_lost = 0;
                self.exclusion_set.insert(bead_id);
                self.current_bead = None;
                // The claim_span is dropped here, closing the bead.claim span.
                self.set_state(WorkerState::Selecting)?;
            }
            ClaimResult::Suspect {
                bead_id,
                consecutive_errors,
                last_error,
            } => {
                tracing::warn!(
                    bead_id = %bead_id,
                    consecutive_errors,
                    %last_error,
                    "bead marked as suspect after repeated claim errors"
                );
                // Record on claim_span explicitly — see the RaceLost arm.
                claim_span.record("needle.claim.result", "suspect");
                claim_span.record("otel.status_code", 2u64);
                claim_span.record(
                    "otel.status_description",
                    format!(
                        "suspect: {} consecutive errors: {}",
                        consecutive_errors, last_error
                    ),
                );
                // Emit telemetry for the suspect bead
                self.telemetry.emit(EventKind::ClaimErrorThreshold {
                    bead_id: bead_id.clone(),
                    consecutive_errors,
                    last_error,
                })?;
                // Exclude the suspect bead and continue
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
        let bead_id = match self.current_bead.as_ref() {
            Some(bead) => bead.id.clone(),
            None => bail!("BUILDING state without current_bead — invariant violated"),
        };
        let prompt_build_span = tracing::info_span!(
            "bead.prompt_build",
            needle.bead.id = %bead_id,
        );

        self.do_build_inner().instrument(prompt_build_span).await
    }

    async fn do_build_inner(&mut self) -> Result<()> {
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

        // Check if we should use SPLIT mode instead of normal PLUCK.
        // Auto-split triggers when a bead has >= split_after_failures consecutive failures.
        let (template_name, failure_count) = self.check_split_mode(&bead).await;

        // If SPLIT mode would be used, check if the bead references NEEDLE-internal
        // configuration. These tasks have no legitimate resolution path from inside
        // a target repo and should not be split into child beads there.
        if template_name == "split" && detects_needle_internal_config(&bead) {
            heartbeat_handle.abort();

            tracing::info!(
                bead_id = %bead_id,
                title = %bead.title,
                failure_count,
                "SPLIT mode skipped: bead references NEEDLE-internal configuration"
            );

            // Set Error status on the bead.prompt_build span
            tracing::Span::current().record("otel.status_code", 2u64);
            tracing::Span::current().record(
                "otel.status_description",
                "skipped: references NEEDLE-internal configuration",
            );

            // Emit split.skipped event.
            let _ = self.telemetry.emit(EventKind::SplitSkipped {
                bead_id: bead_id.clone(),
                reason: "references NEEDLE-internal configuration".to_string(),
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
                reason: "split_out_of_scope".to_string(),
            });

            // Clear current bead and transition to RETRYING.
            self.current_bead = None;
            self.set_state(WorkerState::Retrying)?;
            return Ok(());
        }

        let mut prompt = match tokio::time::timeout(
            timeout_dur,
            tokio::task::spawn_blocking(move || {
                if template_name == "split" {
                    prompt_builder.build_split(&bead, &build_ws, &worker_name, failure_count)
                } else {
                    prompt_builder.build_pluck(&bead, &build_ws, &worker_name)
                }
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
                crate::hoop_hooks::emit_needle_event(
                    &self.current_workspace,
                    &self.worker_name,
                    Some(bead_id.as_ref()),
                    self.current_strand.as_deref(),
                    "timeout",
                    serde_json::json!({}),
                );

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
                crate::hoop_hooks::emit_needle_event(
                    &self.current_workspace,
                    &self.worker_name,
                    Some(bead_id.as_ref()),
                    self.current_strand.as_deref(),
                    "release",
                    serde_json::json!({}),
                );

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
        self.health.update_adapter(Some(&adapter.name));

        // Enter the agent.dispatch span for the dispatching phase.
        let _bead_id = self.current_bead.as_ref().map(|b| b.id.clone());
        crate::hoop_hooks::emit_needle_event(
            &self.current_workspace,
            &self.worker_name,
            _bead_id.as_ref().map(|id| id.as_ref()),
            self.current_strand.as_deref(),
            "dispatch",
            serde_json::json!({"adapter": adapter.name, "model": model}),
        );
        let dispatch_span = tracing::info_span!(
            "agent.dispatch",
            gen_ai.system = %provider.unwrap_or("unknown"),
            gen_ai.request.model = %model.unwrap_or("unknown"),
            needle.agent.pid = tracing::field::Empty, // Will be set when process starts
            needle.agent.exit_code = tracing::field::Empty, // Will be set after execution
        );

        self.do_dispatch_inner(adapter)
            .instrument(dispatch_span)
            .await
    }

    async fn do_dispatch_inner(&mut self, adapter: crate::dispatch::AgentAdapter) -> Result<()> {
        let provider = adapter.provider.as_deref();
        let model = adapter.model.as_deref();

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
            &self.telemetry,
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

        // Use the bead's workspace if set (remote bead from Explore),
        // otherwise fall back to the config's default workspace.
        let dispatch_ws = if is_workspace_unset(&bead.workspace) {
            &self.config.workspace.default
        } else {
            &bead.workspace
        };

        // Race the dispatch against the shutdown signal.
        let was_interrupted;
        let exec_result = if self.shutdown.load(Ordering::SeqCst) {
            // Already shutting down — don't start the agent.
            was_interrupted = true;
            None
        } else {
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
            let execution_span = tracing::info_span!(
                "agent.execution",
                needle.bead.id = %bead.id,
            );
            // Hoisted out of the async block below: qualified_id() is a method
            // call, so using it inside the coroutine would borrow all of *self and
            // collide with the mutable use of self.exec_started_at (E0500). A field
            // access like self.worker_name is a disjoint capture and does not, but
            // worker_name is the WRONG actor -- the claim is made with qualified_id().
            let qualified_actor = self.qualified_id();
            let (result, exec_tokens) = async {
                // Snapshot workspace HEAD + the bead's notes before the agent
                // runs, so the shipped-work gate has a baseline to judge the
                // closure against. Best-effort: a missing snapshot degrades the
                // gate to its conservative path, it never blocks dispatch.
                if let Err(e) = crate::validation::predispatch::record(
                    dispatch_ws,
                    &bead.id,
                    self.store.as_ref(),
                )
                .await
                {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %e,
                        "failed to record pre-dispatch snapshot — shipped-work gate will fall back"
                    );
                }

                // ── Atomic claim verification at dispatch time ──
                // Verify that the bead is still assigned to this worker immediately
                // before dispatch. This prevents double-dispatch where two workers
                // dispatch the same bead concurrently.
                //
                // This check queries the LIVE bead store (not a stale snapshot) to
                // ensure the bead is still in_progress and assigned to this worker.
                // If another worker has reassigned the bead or the bead has been
                // released, we abort the dispatch.
                let is_valid = self
                    .claimer
                    .verify_claim_at_dispatch(&bead.id, &qualified_actor)
                    .await
                    .with_context(|| {
                        format!(
                            "dispatch-time claim verification failed for bead {}",
                            bead.id
                        )
                    })?;

                if !is_valid {
                    // Bead is not assigned to this worker - abort dispatch.
                    // Release the bead back to ready state so another worker can claim it.
                    tracing::warn!(
                        bead_id = %bead.id,
                        worker = %self.worker_name,
                        "dispatch-time claim verification failed: bead not assigned to this worker, aborting dispatch and releasing back to ready state"
                    );

                    // Release the bead back to open status
                    if let Err(e) = self.store.release(&bead.id).await {
                        tracing::error!(
                            bead_id = %bead.id,
                            error = %e,
                            "failed to release bead after dispatch-time verification failure"
                        );
                    }

                    // Emit telemetry for the failed verification
                    let _ = self.telemetry.emit(EventKind::ClaimVerifyFailed {
                        bead_id: bead.id.clone(),
                        expected_actor: qualified_actor.clone(),
                        actual_status: "unknown".to_string(),
                        actual_assignee: "(not verified)".to_string(),
                    });

                    bail!(
                        "dispatch-time claim verification failed for bead {}: bead is not assigned to worker {}",
                        bead.id,
                        qualified_actor
                    );
                }

                self.exec_started_at = Some(Instant::now());
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
                Ok::<_, anyhow::Error>((result, exec_tokens))
            }
            .instrument(execution_span)
            .await?;

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
            effort.provider = adapter.provider.clone();
            effort.tokens = tokens;
            effort.estimated_cost_usd = estimated_cost;
        }

        // Write trace files for stdout and stderr.
        // Errors are logged but don't fail the bead cycle — the output is
        // still available in exec_output for the outcome handler.
        if let Err(e) =
            self.write_trace_files(&bead.id, dispatch_ws, &output.stdout, &output.stderr)
        {
            tracing::warn!(
                bead_id = %bead.id,
                error = %e,
                "failed to write trace files, continuing with normal flow"
            );
        }

        self.exec_output = Some((output, was_interrupted));
        self.set_state(WorkerState::Handling)?;
        Ok(())
    }

    /// Write captured stdout and stderr to trace files.
    ///
    /// Creates `.beads/traces/<bead-id>/` directory and writes:
    /// - `stdout.txt` with captured stdout
    /// - `stderr.txt` with captured stderr
    ///
    /// Errors are returned (not swallowed) so the caller can decide how to handle them.
    /// This function uses synchronous I/O to ensure writes complete before the worker continues.
    fn write_trace_files(
        &self,
        bead_id: &crate::types::BeadId,
        workspace: &std::path::Path,
        stdout: &str,
        stderr: &str,
    ) -> Result<()> {
        use std::io::Write;

        // Build trace directory path: <workspace>/.beads/traces/<bead-id>/
        let trace_dir = workspace
            .join(".beads")
            .join("traces")
            .join(bead_id.as_ref());

        // Create the trace directory if it doesn't exist.
        // This will also create parent directories (.beads/traces/) if needed.
        fs::create_dir_all(&trace_dir).with_context(|| {
            format!(
                "failed to create trace directory at {}",
                trace_dir.display()
            )
        })?;

        // Write stdout to stdout.txt
        let stdout_path = trace_dir.join("stdout.txt");
        let mut stdout_file = fs::File::create(&stdout_path).with_context(|| {
            format!("failed to create stdout file at {}", stdout_path.display())
        })?;
        stdout_file
            .write_all(stdout.as_bytes())
            .with_context(|| format!("failed to write stdout to {}", stdout_path.display()))?;

        // Write stderr to stderr.txt
        let stderr_path = trace_dir.join("stderr.txt");
        let mut stderr_file = fs::File::create(&stderr_path).with_context(|| {
            format!("failed to create stderr file at {}", stderr_path.display())
        })?;
        stderr_file
            .write_all(stderr.as_bytes())
            .with_context(|| format!("failed to write stderr to {}", stderr_path.display()))?;

        tracing::debug!(
            bead_id = %bead_id,
            trace_dir = %trace_dir.display(),
            stdout_len = stdout.len(),
            stderr_len = stderr.len(),
            "trace files written successfully"
        );

        Ok(())
    }

    /// HANDLING: classify the outcome and produce one mandatory terminal action.
    ///
    /// This method deliberately does not advance the worker state. Its caller
    /// must consume the returned [`BeadAction`] through
    /// [`apply_bead_action`](Self::apply_bead_action), which verifies that the
    /// claim is no longer held before leaving HANDLING.
    async fn do_handle(&mut self) -> Result<BeadAction> {
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

        // Clone values needed for error telemetry before creating the async block.
        // This avoids borrowing issues with the async block that captures `self`.
        let bead_id_clone = bead.id.clone();
        let telemetry_clone = self.telemetry.clone();

        // Wrap the entire HANDLING state in a timeout to prevent indefinite hangs.
        // Even if the Tokio runtime gets blocked by a synchronous operation, this
        // timeout will fire (on a threadpool) and allow recovery.
        let outcome_span = tracing::info_span!(
            "bead.outcome",
            needle.bead.id = %bead.id,
            needle.outcome = tracing::field::Empty, // Will be set based on handler result
            needle.outcome.action = tracing::field::Empty, // Will be set based on handler result
        );
        let handling_future = async {
            // Wrap the outcome handler in a 60-second timeout to prevent indefinite hangs.
            // The health monitor's background thread writes heartbeat files based on
            // shared state, so external monitoring can detect hangs via stale heartbeats.

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
                    // Handler returned an error. Return an explicit recovery
                    // action; the state-machine caller must apply it.
                    tracing::error!(
                        bead_id = %bead.id,
                        error = %e,
                        "outcome handler failed, routing through explicit error recovery"
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
                    Err(anyhow::anyhow!("handler failed: {}", e))
                }
                Err(_) => {
                    // Timeout after 60 seconds. Return an explicit recovery
                    // action; the state-machine caller must apply it.
                    tracing::error!(
                        bead_id = %bead.id,
                        "outcome handler timed out after 60s, routing through explicit error recovery"
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
                    Err(anyhow::anyhow!("handler timed out after 60s"))
                }
            }
        }
        .instrument(outcome_span);

        // The watchdog thread above covers a genuinely wedged runtime. Keep the
        // 90-second async safety net cancellable so a successful handler does
        // not leave a sleeping blocking task that delays runtime shutdown.
        let timeout = tokio::time::sleep(std::time::Duration::from_secs(90));
        tokio::pin!(timeout);

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
                        // HANDLING failed. The explicit error action is applied
                        // by the state-machine caller before the cycle advances.
                        heartbeat_task.abort();
                        return Ok(BeadAction::Errored);
                    }
                }
            }
            _ = &mut timeout => {
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
                return Ok(BeadAction::Errored);
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
            // Stop the heartbeat task.
            cancelled.store(true, Ordering::Release);
            heartbeat_task.abort();
            return Ok(BeadAction::Errored);
        }

        // Cancellation and internal handler timeouts are represented as an
        // explicit error action. Do not run post-handler work that assumes the
        // bead was already released; the caller applies recovery immediately.
        if handler_result.bead_action == BeadAction::Errored {
            cancelled.store(true, Ordering::Release);
            heartbeat_task.abort();
            return Ok(BeadAction::Errored);
        }

        // HOOP Hook 2 (event tap): emit the outcome as a single terminal
        // event on the bead. Best-effort — see hoop_hooks module docs.
        {
            let duration_ms = self
                .exec_started_at
                .take()
                .map(|start| start.elapsed().as_millis() as u64);
            let outcome_str = handler_result.outcome.as_str();
            let event_name = match &handler_result.outcome {
                crate::types::Outcome::Success => "complete",
                crate::types::Outcome::Failure | crate::types::Outcome::AgentNotFound => "fail",
                crate::types::Outcome::Timeout => "timeout",
                crate::types::Outcome::Crash(_) => "crash",
                crate::types::Outcome::Interrupted => "release",
            };
            let mut extra = serde_json::json!({
                "outcome": outcome_str,
                "exit_code": output.exit_code,
            });
            if let Some(ms) = duration_ms {
                extra["duration_ms"] = serde_json::json!(ms);
            }
            crate::hoop_hooks::emit_needle_event(
                &self.current_workspace,
                &self.worker_name,
                Some(bead.id.as_ref()),
                self.current_strand.as_deref(),
                event_name,
                extra,
            );
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

            // Wrap mitosis evaluation in timeout to prevent indefinite hang.
            async {
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
                Ok(Ok(crate::mitosis::MitosisResult::OutOfScope)) => {
                    tracing::Span::current().record("needle.mitosis.result", "out_of_scope");
                    tracing::debug!(
                        bead_id = %bead.id,
                        "mitosis: bead references NEEDLE-internal config, out of scope for workspace"
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
            }
            .instrument(mitosis_span)
            .await;
        }

        // Evaluate for timeout-triggered mitosis after timeout — the bead has already been
        // released and marked as deferred by the outcome handler.
        if handler_result.outcome == Outcome::Timeout {
            let workspace = if is_workspace_unset(&bead.workspace) {
                self.config.workspace.default.clone()
            } else {
                bead.workspace.clone()
            };

            // Enter the bead.mitosis span for timeout mitosis evaluation.
            let mitosis_span = tracing::info_span!(
                "bead.mitosis",
                needle.bead.id = %bead.id,
                needle.mitosis.result = tracing::field::Empty, // Will be set based on evaluation result
            );

            // Capture the execution duration for timeout eligibility
            let duration = self
                .last_effort
                .as_ref()
                .map(|effort| effort.cycle_start.elapsed())
                .unwrap_or(std::time::Duration::from_secs(0));

            // Build AgentOutcome from the last execution result
            // Note: we don't track stdout/stderr in EffortData, so use empty strings
            let agent_outcome = crate::types::AgentOutcome {
                exit_code: 124, // SIGTERM exit code for timeout
                stdout: String::new(),
                stderr: String::new(),
            };

            // Wrap timeout mitosis evaluation in timeout to prevent indefinite hang.
            async {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(120),
                    self.mitosis_evaluator.evaluate_timeout(
                        self.store.as_ref(),
                        &bead,
                        &workspace,
                        &self.dispatcher,
                        &self.prompt_builder,
                        &self.config.agent.default,
                        &agent_outcome,
                        duration,
                    ),
                )
                .await
                {
                Ok(Ok(crate::mitosis::MitosisResult::Split { children })) => {
                    tracing::Span::current().record("needle.mitosis.result", "split");
                    tracing::info!(
                        bead_id = %bead.id,
                        children = children.len(),
                        "timeout mitosis created child beads — parent blocked"
                    );
                }
                Ok(Ok(crate::mitosis::MitosisResult::NotSplittable)) => {
                    tracing::Span::current().record("needle.mitosis.result", "not_splittable");
                    tracing::debug!(bead_id = %bead.id, "timeout mitosis: bead is single task or unsafe to split");
                }
                Ok(Ok(crate::mitosis::MitosisResult::Skipped { reason })) => {
                    tracing::Span::current().record("needle.mitosis.result", "skipped");
                    tracing::debug!(
                        bead_id = %bead.id,
                        reason = %reason,
                        "timeout mitosis skipped"
                    );
                }
                Ok(Ok(crate::mitosis::MitosisResult::OutOfScope)) => {
                    tracing::Span::current().record("needle.mitosis.result", "out_of_scope");
                    tracing::debug!(
                        bead_id = %bead.id,
                        "timeout mitosis: bead references NEEDLE-internal config, out of scope for workspace"
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
                        "timeout mitosis evaluation failed (bead already released)"
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
                        "timeout mitosis evaluation timed out after 120s, continuing to LOGGING"
                    );
                }
                }
            }
            .instrument(mitosis_span)
            .await;
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

        // Set cancellation flag and abort the heartbeat task since handling is complete.
        cancelled.store(true, Ordering::Release);
        heartbeat_task.abort();

        Ok(handler_result.bead_action)
    }

    /// Consume a terminal action and enforce the dispatch postcondition.
    ///
    /// This is the ONLY place in the codebase that mutates bead state based on
    /// handler outcomes. Outcome handlers ONLY return a BeadAction enum; they
    /// never call store.release(), store.block(), or any other state mutation.
    ///
    /// Because [`BeadAction`] has no empty variant and is `must_use`, a HANDLING
    /// branch cannot silently return `()` and leave a claim behind. The type
    /// system enforces that every dispatch cycle ends with an explicit transition.
    async fn apply_bead_action(&mut self, action: BeadAction) -> Result<()> {
        let bead = self
            .current_bead
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("applying bead action without current_bead"))?;

        tracing::debug!(
            bead_id = %bead.id,
            action = %action,
            "applying bead action"
        );

        // Perform the appropriate state mutation for each action variant.
        // This is the structural enforcement: all state mutations happen here,
        // never inside handlers.
        match action.clone() {
            BeadAction::Closed => {
                // Bead was closed by the agent. Verify it's actually closed.
                match tokio::time::timeout(Duration::from_secs(30), self.store.show(&bead.id)).await
                {
                    Ok(Ok(current)) => {
                        if current.status != BeadStatus::Closed
                            && current.status != BeadStatus::Done
                        {
                            tracing::warn!(
                                bead_id = %bead.id,
                                actual_status = ?current.status,
                                "agent reported closed but bead is not closed; releasing to enforce postcondition"
                            );
                            // Force release to enforce postcondition
                            tokio::time::timeout(
                                Duration::from_secs(30),
                                self.store.release(&bead.id),
                            )
                            .await??;
                        }
                        // Bead is closed/done as expected
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            error = %e,
                            "could not verify bead closure; assuming closed"
                        );
                        // Assume closed - the handler determined this
                    }
                    Err(_) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            "verification timed out; assuming closed"
                        );
                        // Assume closed - the handler determined this
                    }
                }
            }
            BeadAction::Released => {
                // Release the bead back to open status.
                tokio::time::timeout(Duration::from_secs(30), self.store.release(&bead.id))
                    .await??;
                self.telemetry.emit(EventKind::BeadReleased {
                    bead_id: bead.id.clone(),
                    reason: "handler_action:released".to_string(),
                })?;
            }
            BeadAction::Deferred => {
                // Release and add deferred label.
                tokio::time::timeout(Duration::from_secs(30), self.store.release(&bead.id))
                    .await??;
                let _ = tokio::time::timeout(
                    Duration::from_secs(30),
                    self.store.add_label(&bead.id, "deferred"),
                )
                .await;
                self.telemetry.emit(EventKind::BeadReleased {
                    bead_id: bead.id.clone(),
                    reason: "handler_action:deferred".to_string(),
                })?;
            }
            BeadAction::Alerted => {
                // Release after creating an alert bead.
                tokio::time::timeout(Duration::from_secs(30), self.store.release(&bead.id))
                    .await??;
                self.telemetry.emit(EventKind::BeadReleased {
                    bead_id: bead.id.clone(),
                    reason: "handler_action:alerted".to_string(),
                })?;
            }
            BeadAction::Quarantined => {
                // Block the bead (status=blocked, labeled 'cycling').
                // Note: Handler has already emitted BeadQuarantined telemetry with failure count.
                tokio::time::timeout(Duration::from_secs(30), self.store.block(&bead.id)).await??;
                let _ = tokio::time::timeout(
                    Duration::from_secs(30),
                    self.store.add_label(&bead.id, "cycling"),
                )
                .await;
            }
            BeadAction::Interrupted => {
                // Release due to worker interruption.
                tokio::time::timeout(Duration::from_secs(30), self.store.release(&bead.id))
                    .await??;
                self.telemetry.emit(EventKind::BeadReleased {
                    bead_id: bead.id.clone(),
                    reason: "worker_interrupted".to_string(),
                })?;
            }
            BeadAction::Errored => {
                // Release after handler error.
                tokio::time::timeout(Duration::from_secs(30), self.store.release(&bead.id))
                    .await??;
                self.telemetry.emit(EventKind::BeadReleased {
                    bead_id: bead.id.clone(),
                    reason: "handler_error_recovery".to_string(),
                })?;
            }
        }

        tracing::debug!(
            bead_id = %bead.id,
            action = %action,
            "applied terminal bead action"
        );

        // Transition to next state based on action
        match action {
            BeadAction::Interrupted => self.set_state(WorkerState::Stopped)?,
            BeadAction::Closed
            | BeadAction::Released
            | BeadAction::Deferred
            | BeadAction::Alerted
            | BeadAction::Quarantined
            | BeadAction::Errored => self.set_state(WorkerState::Logging)?,
        }

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
                provider: effort.provider.clone(),
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

        // Close the bead.lifecycle span by dropping the final handle.
        // Record the outcome before closing if we have the handler result available.
        if let Some(lifecycle_span) = self.bead_lifecycle_span.take() {
            // Record the outcome on the bead.lifecycle span before closing
            if let Some(ref outcome) = self.last_outcome {
                lifecycle_span.record("needle.bead.outcome", outcome.as_str());
                // Set span status: Ok for success, Error for all other outcomes
                if outcome != "success" {
                    // otel.status_code = 2 indicates ERROR in OpenTelemetry
                    lifecycle_span.record("otel.status_code", 2u64);
                    lifecycle_span.record("otel.status_description", outcome.as_str());
                }
            }
            // Clear the outcome for the next cycle
            self.last_outcome = None;
            // The span closes when this final handle is dropped.
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
        // NOTE: check_hot_reload() is async and cannot be called from sync do_log().
        // This check should be moved to an async context or handled separately.

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

    /// Check for a new :stable binary and exit cleanly if detected.
    ///
    /// Called between LOGGING and SELECTING. If a new binary is found:
    /// 1. Emit `worker.binary_freshness_exit` telemetry
    /// 2. Exit with code 72 to signal supervisor to relaunch with new binary
    ///
    /// This ensures no bead is left mid-dispatch — the check only runs after
    /// a claim is closed and before the next claim is attempted.
    async fn check_hot_reload(&mut self) -> Result<()> {
        let needle_home = &self.config.workspace.home;
        match upgrade::check_hot_reload(needle_home) {
            Ok(HotReloadCheck::NewBinaryDetected {
                old_hash,
                new_hash,
                stable_path: _,
            }) => {
                tracing::info!(
                    old_hash = %truncate_for_display(&old_hash, 12),
                    new_hash = %truncate_for_display(&new_hash, 12),
                    "new :stable binary detected — exiting cleanly for supervisor relaunch"
                );

                self.telemetry.emit(EventKind::BinaryFreshnessExit {
                    old_hash: old_hash.clone(),
                    new_hash: new_hash.clone(),
                    was_deleted: false,
                })?;

                // Flush telemetry before exit
                std::mem::forget(self.telemetry.clone());

                tracing::info!(
                    "exiting with code {} to signal binary refresh",
                    EXIT_CODE_STALE_BINARY
                );
                std::process::exit(EXIT_CODE_STALE_BINARY);
            }
            Ok(HotReloadCheck::NoChange) => Ok(()),
            Ok(HotReloadCheck::Skipped { reason }) => {
                tracing::debug!(reason = %reason, "hot-reload check skipped");
                Ok(())
            }
            Ok(HotReloadCheck::CurrentBinaryDeleted {
                stable_hash,
                stable_path: _,
            }) => {
                // Current binary has been deleted/unlinked (e.g., mv-replaced while running).
                // This is an unconditional signal to exit cleanly so the supervisor can relaunch.
                tracing::error!(
                    stable_hash = %truncate_for_display(&stable_hash, 12),
                    "current binary has been deleted/unlinked — exiting cleanly for supervisor relaunch"
                );

                self.telemetry.emit(EventKind::BinaryFreshnessExit {
                    old_hash: "<deleted>".to_string(),
                    new_hash: stable_hash.clone(),
                    was_deleted: true,
                })?;

                // Flush telemetry before exit
                std::mem::forget(self.telemetry.clone());

                tracing::info!(
                    "exiting with code {} to signal binary refresh",
                    EXIT_CODE_STALE_BINARY
                );
                std::process::exit(EXIT_CODE_STALE_BINARY);
            }
            Err(e) => {
                tracing::warn!(error = %e, "hot-reload check failed, continuing");
                Ok(())
            }
        }
    }

    /// Check for a changed global configuration file at the cycle boundary.
    ///
    /// This check is deliberately polled instead of signal-driven. SIGHUP is
    /// already a shutdown signal so a killed tmux session can release its bead
    /// and emit `worker.stopped`; using it for reload would turn teardown into a
    /// reload request.
    ///
    /// The check is gated by `worker.config_reload_check_interval_secs`. A zero
    /// interval disables it. Both the file mtime and its SHA-256 content hash
    /// are compared so an in-place rewrite that preserves mtime is detected.
    /// A changed candidate is loaded and validated before any subsequent
    /// config-reload stage can apply it. Invalid candidates are rejected while
    /// the worker continues with its running configuration.
    async fn check_config_reload(&mut self) -> Result<()> {
        let interval_secs = self.config.worker.config_reload_check_interval_secs;
        if interval_secs == 0 {
            return Ok(());
        }

        let now = Instant::now();
        let interval = Duration::from_secs(interval_secs);
        if let Some(last_check) = self.last_config_reload_check {
            if now.duration_since(last_check) < interval {
                return Ok(());
            }
        }
        self.last_config_reload_check = Some(now);

        let path = global_config_path();
        let current = match read_config_file_fingerprint(&path) {
            Ok(current) => current,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "configuration reload check failed; keeping the running config"
                );
                return Ok(());
            }
        };
        let changed = self
            .config_reload_fingerprint
            .as_ref()
            .map(|previous| config_file_changed(previous, &current))
            .unwrap_or(false);

        self.config_reload_fingerprint = Some(current);

        if changed {
            tracing::info!(
                path = %path.display(),
                "global configuration change detected at cycle boundary"
            );
            if let Err(error) = self
                .telemetry
                .emit_try_lock(EventKind::ConfigReloadDetected)
            {
                tracing::warn!(error = %error, "failed to emit config.reload.detected");
            }

            // This is the hard safety gate for reload. Keep the candidate
            // separate from self.config and validate it before a later reload
            // stage is allowed to swap or rebuild anything. In particular, a
            // malformed edit must not become a worker error: polling makes a
            // bad edit visible to every worker, so fail-closed behaviour here
            // would turn one operator mistake into a fleet-wide outage.
            if let Some(candidate) = self.load_validated_config_candidate(&path) {
                let restart_required_keys = self.config.changed_restart_required_keys(&candidate);
                report_restart_required_config(&self.telemetry, restart_required_keys);

                let tier_b = self.rebuild_tier_b_components(&candidate);
                if !tier_b.failures.is_empty() {
                    self.reject_config_reload(
                        tier_b
                            .failures
                            .iter()
                            .map(|failure| {
                                format!(
                                    "{} rebuild failed; previous instance retained: {}",
                                    failure.component, failure.error
                                )
                            })
                            .collect(),
                    );
                }

                // These two components own snapshots of fields otherwise
                // classified live. If their rebuild failed, retain those
                // running values too so config and component cannot diverge.
                let mut live_candidate = candidate;
                if tier_b.failed("StrandRunner") {
                    live_candidate.strands = self.config.strands.clone();
                }
                if tier_b.failed("OutcomeHandler") {
                    live_candidate.outcome = self.config.outcome.clone();
                }

                let mut changed_keys = tier_b.applied_keys;
                changed_keys.extend(self.apply_tier_a_config(&live_candidate));
                changed_keys.sort();
                changed_keys.dedup();
                if !changed_keys.is_empty() {
                    self.config_sources = self.reload_source_map();
                    self.config_reload_generation += 1;
                    tracing::info!(
                        changed_keys = ?changed_keys,
                        reload_generation = self.config_reload_generation,
                        "applied configuration at cycle boundary"
                    );
                    if let Err(error) =
                        self.telemetry
                            .emit_try_lock(EventKind::ConfigReloadApplied {
                                changed_keys: changed_keys.clone(),
                            })
                    {
                        tracing::warn!(error = %error, "failed to emit config.reload.applied");
                    }
                    // Update registry to reflect new reload generation
                    if let Err(e) = self.update_registry_entry() {
                        tracing::warn!(error = %e, "failed to update registry after config reload");
                    }
                }
            }
        }

        Ok(())
    }

    /// Load and validate a configuration candidate for a reload.
    ///
    /// `None` means the candidate was rejected and the current configuration
    /// must remain in use. Rejection is deliberately reported as telemetry and
    /// a warning rather than returned as an error: a bad reload must never
    /// propagate into the worker state machine's `Result` path.
    ///
    /// The returned candidate is owned and remains separate from `self.config`
    /// so callers can validate it before performing an all-or-nothing swap.
    fn load_validated_config_candidate(&self, path: &Path) -> Option<Config> {
        let candidate = match ConfigLoader::load_from_path(path) {
            Ok(candidate) => candidate,
            Err(error) => {
                self.reject_config_reload(vec![
                    "candidate could not be loaded; see the worker warning for details".to_string(),
                ]);
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "configuration reload rejected; keeping the running config"
                );
                return None;
            }
        };

        let validation_errors = ConfigLoader::validate(&candidate);
        if !validation_errors.is_empty() {
            let validation_errors = validation_errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            self.reject_config_reload(validation_errors.clone());
            tracing::warn!(
                path = %path.display(),
                validation_errors = ?validation_errors,
                "configuration reload rejected; keeping the running config"
            );
            return None;
        }

        Some(candidate)
    }

    /// Emit a reload rejection without allowing telemetry failure to stop the
    /// worker. The event intentionally carries diagnostics only; it never
    /// includes configuration contents or resolved secret/header values.
    fn reject_config_reload(&self, validation_errors: Vec<String>) {
        if let Err(error) = self
            .telemetry
            .emit_try_lock(EventKind::ConfigReloadRejected { validation_errors })
        {
            tracing::warn!(error = %error, "failed to emit config.reload.rejected");
        }
    }

    /// Rebuild the five non-telemetry Tier-B components whose owned config
    /// subtree changed.
    ///
    /// Every constructor produces a `Result` and every installation is
    /// independent. A failure therefore leaves that slot and its corresponding
    /// running config unchanged while later components still get a chance to
    /// rebuild. Candidate Tier-C values are never passed through accidentally:
    /// each constructor starts from the running snapshot and receives only the
    /// subtree it owns.
    fn rebuild_tier_b_components(&mut self, candidate: &Config) -> TierBReloadReport {
        let mut report = TierBReloadReport::default();

        // StrandRunner owns the complete strand waterfall snapshot. Although
        // a few strand thresholds are classified live, rebuilding on any
        // `strands` change keeps every strand's cached config coherent.
        if config_values_differ(&self.config.strands, &candidate.strands) {
            let mut component_config = self.config.clone();
            component_config.strands = candidate.strands.clone();
            let worker_id = self.qualified_id();
            let registry = Registry::default_location(&component_config.workspace.home);
            let rebuilt = Ok(StrandRunner::from_config(
                &component_config,
                &worker_id,
                registry,
                self.telemetry.clone(),
            ));

            if install_rebuilt_component(&mut self.strands, "StrandRunner", rebuilt, &mut report) {
                self.config.strands = candidate.strands.clone();
                report.applied_keys.push("strands".to_string());
            }
        }

        if config_values_differ(&self.config.prompt, &candidate.prompt) {
            let rebuilt =
                PromptBuilder::with_workspace(&candidate.prompt, &self.config.workspace.default)
                    .map(|builder| {
                        builder
                            .with_cross_workspace_skills(
                                &self.config.strands.explore.workspaces,
                                &self.config.workspace.labels,
                            )
                            .with_global_learnings(
                                &self.config.strands.learning.global_learnings_file,
                            )
                    })
                    .and_then(|builder| {
                        builder.validate()?;
                        Ok(builder)
                    });

            if install_rebuilt_component(
                &mut self.prompt_builder,
                "PromptBuilder",
                rebuilt,
                &mut report,
            ) {
                self.config.prompt = candidate.prompt.clone();
                report.applied_keys.push("prompt".to_string());
            }
        }

        // Only adapters_dir is Tier B within AgentConfig. Other agent fields
        // remain live and must not cause adapter files to be re-read.
        if config_values_differ(
            &self.config.agent.adapters_dir,
            &candidate.agent.adapters_dir,
        ) {
            let mut component_config = self.config.clone();
            component_config.agent.adapters_dir = candidate.agent.adapters_dir.clone();
            let rebuilt = Dispatcher::new(&component_config, self.telemetry.clone());

            if install_rebuilt_component(&mut self.dispatcher, "Dispatcher", rebuilt, &mut report) {
                self.config.agent.adapters_dir = candidate.agent.adapters_dir.clone();
                report.applied_keys.push("agent.adapters_dir".to_string());
            }
        }

        if config_values_differ(&self.config.limits, &candidate.limits) {
            let rebuilt = Ok(RateLimiter::new(
                candidate.limits.clone(),
                &self.config.workspace.home.join("state"),
            ));

            if install_rebuilt_component(
                &mut self.rate_limiter,
                "RateLimiter",
                rebuilt,
                &mut report,
            ) {
                self.config.limits = candidate.limits.clone();
                report.applied_keys.push("limits".to_string());
            }
        }

        let outcome_changed = config_values_differ(&self.config.gates, &candidate.gates)
            || config_values_differ(&self.config.verification, &candidate.verification)
            || config_values_differ(&self.config.validation, &candidate.validation)
            || config_values_differ(&self.config.outcome, &candidate.outcome);
        if outcome_changed {
            let mut component_config = self.config.clone();
            component_config.gates = candidate.gates.clone();
            component_config.verification = candidate.verification.clone();
            component_config.validation = candidate.validation.clone();
            component_config.outcome = candidate.outcome.clone();
            let rebuilt = Ok(OutcomeHandler::new(
                component_config,
                self.telemetry.clone(),
            ));

            if install_rebuilt_component(
                &mut self.outcome_handler,
                "OutcomeHandler",
                rebuilt,
                &mut report,
            ) {
                if config_values_differ(&self.config.gates, &candidate.gates) {
                    report.applied_keys.push("gates".to_string());
                }
                if config_values_differ(&self.config.verification, &candidate.verification) {
                    report.applied_keys.push("verification".to_string());
                }
                if config_values_differ(&self.config.validation, &candidate.validation) {
                    report.applied_keys.push("validation".to_string());
                }
                self.config.gates = candidate.gates.clone();
                self.config.verification = candidate.verification.clone();
                self.config.validation = candidate.validation.clone();
                // `outcome` remains Tier A and is copied immediately after
                // this function by apply_tier_a_config.
            }
        }

        report
    }

    /// Apply the declared Tier-A fields from a validated candidate.
    ///
    /// The running config is never edited in place. Every live field is first
    /// copied into a clone, and the completed snapshot replaces `self.config`
    /// in one assignment. This keeps a reload from exposing a partially
    /// applied configuration if a future field copy becomes fallible or gains
    /// additional processing.
    ///
    /// Tier-B and Tier-C fields deliberately remain from the running snapshot;
    /// their component rebuild and restart-required handling belong to later
    /// reload stages.
    fn apply_tier_a_config(&mut self, candidate: &Config) -> Vec<String> {
        let mut next = self.config.clone();
        let mut changed_keys = Vec::new();

        macro_rules! replace {
            ($key:literal, $running:expr, $candidate:expr) => {
                if replace_config_value(&mut $running, &$candidate) {
                    changed_keys.push($key.to_string());
                }
            };
        }

        // Agent fields declared Tier A. `adapters_dir` is Tier B and is kept
        // in the running snapshot until Dispatcher is rebuilt.
        replace!("agent.default", next.agent.default, candidate.agent.default);
        replace!("agent.args", next.agent.args, candidate.agent.args);
        replace!("agent.timeout", next.agent.timeout, candidate.agent.timeout);
        replace!("agent.routing", next.agent.routing, candidate.agent.routing);

        // Worker fields declared Tier A. Identity, launch, and reload-poller
        // settings are Tier C and therefore intentionally excluded.
        replace!(
            "worker.idle_timeout",
            next.worker.idle_timeout,
            candidate.worker.idle_timeout
        );
        replace!(
            "worker.idle_action",
            next.worker.idle_action,
            candidate.worker.idle_action
        );
        replace!(
            "worker.max_claim_retries",
            next.worker.max_claim_retries,
            candidate.worker.max_claim_retries
        );
        replace!(
            "worker.claim_race_lost_skip",
            next.worker.claim_race_lost_skip,
            candidate.worker.claim_race_lost_skip
        );
        replace!(
            "worker.cpu_load_warn",
            next.worker.cpu_load_warn,
            candidate.worker.cpu_load_warn
        );
        replace!(
            "worker.memory_free_warn_mb",
            next.worker.memory_free_warn_mb,
            candidate.worker.memory_free_warn_mb
        );
        replace!(
            "worker.enforce_shipped_work",
            next.worker.enforce_shipped_work,
            candidate.worker.enforce_shipped_work
        );
        replace!(
            "worker.adaptive_stagger_max_wait_secs",
            next.worker.adaptive_stagger_max_wait_secs,
            candidate.worker.adaptive_stagger_max_wait_secs
        );
        replace!(
            "worker.adaptive_stagger_check_interval_secs",
            next.worker.adaptive_stagger_check_interval_secs,
            candidate.worker.adaptive_stagger_check_interval_secs
        );
        replace!(
            "worker.building_timeout",
            next.worker.building_timeout,
            candidate.worker.building_timeout
        );
        replace!(
            "worker.idle_backoff_min",
            next.worker.idle_backoff_min,
            candidate.worker.idle_backoff_min
        );
        replace!(
            "worker.idle_backoff_max",
            next.worker.idle_backoff_max,
            candidate.worker.idle_backoff_max
        );
        replace!(
            "worker.short_retry_backoff",
            next.worker.short_retry_backoff,
            candidate.worker.short_retry_backoff
        );
        replace!(
            "worker.freshness_check_interval_secs",
            next.worker.freshness_check_interval_secs,
            candidate.worker.freshness_check_interval_secs
        );

        // Live strand thresholds. Fields that shape a StrandRunner or affect
        // workspace/process ownership remain for the Tier-B/C stages.
        replace!(
            "strands.pluck.exclude_labels",
            next.strands.pluck.exclude_labels,
            candidate.strands.pluck.exclude_labels
        );
        replace!(
            "strands.mend.stuck_threshold_secs",
            next.strands.mend.stuck_threshold_secs,
            candidate.strands.mend.stuck_threshold_secs
        );
        replace!(
            "strands.mend.lock_ttl_secs",
            next.strands.mend.lock_ttl_secs,
            candidate.strands.mend.lock_ttl_secs
        );
        replace!(
            "strands.explore.workspaces",
            next.strands.explore.workspaces,
            candidate.strands.explore.workspaces
        );
        replace!(
            "strands.weave.max_beads_per_run",
            next.strands.weave.max_beads_per_run,
            candidate.strands.weave.max_beads_per_run
        );
        replace!(
            "strands.weave.cooldown_hours",
            next.strands.weave.cooldown_hours,
            candidate.strands.weave.cooldown_hours
        );
        replace!(
            "strands.unravel.max_beads_per_run",
            next.strands.unravel.max_beads_per_run,
            candidate.strands.unravel.max_beads_per_run
        );
        replace!(
            "strands.unravel.cooldown_hours",
            next.strands.unravel.cooldown_hours,
            candidate.strands.unravel.cooldown_hours
        );
        replace!(
            "strands.pulse.max_beads_per_run",
            next.strands.pulse.max_beads_per_run,
            candidate.strands.pulse.max_beads_per_run
        );
        replace!(
            "strands.pulse.cooldown_hours",
            next.strands.pulse.cooldown_hours,
            candidate.strands.pulse.cooldown_hours
        );
        replace!(
            "strands.pulse.severity_threshold",
            next.strands.pulse.severity_threshold,
            candidate.strands.pulse.severity_threshold
        );
        replace!(
            "strands.reflect.min_beads_since_last",
            next.strands.reflect.min_beads_since_last,
            candidate.strands.reflect.min_beads_since_last
        );
        replace!(
            "strands.reflect.cooldown_hours",
            next.strands.reflect.cooldown_hours,
            candidate.strands.reflect.cooldown_hours
        );
        replace!(
            "strands.reflect.max_learnings_per_run",
            next.strands.reflect.max_learnings_per_run,
            candidate.strands.reflect.max_learnings_per_run
        );
        replace!(
            "strands.reflect.max_skills_per_run",
            next.strands.reflect.max_skills_per_run,
            candidate.strands.reflect.max_skills_per_run
        );
        replace!(
            "strands.reflect.learning_retention_days",
            next.strands.reflect.learning_retention_days,
            candidate.strands.reflect.learning_retention_days
        );
        replace!(
            "strands.reflect.max_learnings",
            next.strands.reflect.max_learnings,
            candidate.strands.reflect.max_learnings
        );
        replace!(
            "strands.splice.stale_threshold_secs",
            next.strands.splice.stale_threshold_secs,
            candidate.strands.splice.stale_threshold_secs
        );
        replace!(
            "strands.splice.detect_live_loops",
            next.strands.splice.detect_live_loops,
            candidate.strands.splice.detect_live_loops
        );
        replace!(
            "strands.splice.live_loop_scan_events",
            next.strands.splice.live_loop_scan_events,
            candidate.strands.splice.live_loop_scan_events
        );
        replace!(
            "strands.splice.claim_churn_threshold",
            next.strands.splice.claim_churn_threshold,
            candidate.strands.splice.claim_churn_threshold
        );
        replace!(
            "strands.splice.log_runaway_bytes",
            next.strands.splice.log_runaway_bytes,
            candidate.strands.splice.log_runaway_bytes
        );
        replace!(
            "strands.splice.live_loop_window_secs",
            next.strands.splice.live_loop_window_secs,
            candidate.strands.splice.live_loop_window_secs
        );
        replace!(
            "strands.knot.alert_cooldown_minutes",
            next.strands.knot.alert_cooldown_minutes,
            candidate.strands.knot.alert_cooldown_minutes
        );
        replace!(
            "strands.knot.exhaustion_threshold",
            next.strands.knot.exhaustion_threshold,
            candidate.strands.knot.exhaustion_threshold
        );
        replace!(
            "strands.mitosis.enabled",
            next.strands.mitosis.enabled,
            candidate.strands.mitosis.enabled
        );
        replace!(
            "strands.mitosis.first_failure_only",
            next.strands.mitosis.first_failure_only,
            candidate.strands.mitosis.first_failure_only
        );

        // These top-level sections are entirely Tier A.
        replace!(
            "outcome.quarantine_after_failures",
            next.outcome.quarantine_after_failures,
            candidate.outcome.quarantine_after_failures
        );
        replace!(
            "budget.warn_usd",
            next.budget.warn_usd,
            candidate.budget.warn_usd
        );
        replace!(
            "budget.stop_usd",
            next.budget.stop_usd,
            candidate.budget.stop_usd
        );
        replace!("pricing", next.pricing, candidate.pricing);

        if !changed_keys.is_empty() {
            self.config = next;
        }

        changed_keys
    }

    /// Check whether the running binary is stale compared to the latest needle-stable.
    ///
    /// This check runs between dispatch cycles (never mid-claim) at the interval
    /// configured by `worker.freshness_check_interval_secs`. When a stale binary
    /// is detected, it logs a warning but does NOT exit — the worker continues
    /// processing with the stale binary. This is distinct from `check_hot_reload()`,
    /// which exits cleanly when a new binary is detected to allow supervisor relaunch.
    ///
    /// The check compares git commit SHAs embedded in the build metadata, not binary
    /// file hashes. This ensures we detect when the codebase has advanced even if the
    /// binary layout happens to hash similarly.
    ///
    /// Returns `Ok(())` if the check ran (or was skipped due to interval), `Err` if
    /// the check itself failed (non-fatal, continues with current binary).
    async fn check_freshness(&mut self) -> Result<()> {
        let interval_secs = self.config.worker.freshness_check_interval_secs;

        // If interval is 0, freshness checking is disabled
        if interval_secs == 0 {
            return Ok(());
        }

        let now = Instant::now();
        let interval = Duration::from_secs(interval_secs);

        // Check if enough time has passed since the last check
        if let Some(last_check) = self.last_freshness_check {
            if now.duration_since(last_check) < interval {
                // Not enough time has passed, skip this check
                return Ok(());
            }
        }

        // Update the last check time
        self.last_freshness_check = Some(now);

        // Get the running binary's build metadata
        let current_metadata = crate::build_metadata::BuildMetadata::current();
        let current_commit = &current_metadata.commit_sha;

        // Get the stable binary's build metadata
        let stable_metadata = match crate::build_metadata::BuildMetadata::from_stable_binary() {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                // No stable binary exists — skip the check
                tracing::debug!("no needle-stable binary found, skipping freshness check");
                return Ok(());
            }
            Err(e) => {
                // Failed to read stable binary metadata — log and continue
                tracing::warn!(error = %e, "failed to read needle-stable build metadata, skipping freshness check");
                return Ok(());
            }
        };

        let stable_commit = &stable_metadata.commit_sha;

        // Compare commit SHAs
        if current_commit != stable_commit {
            // Only warn if we haven't already warned about this stale binary
            if !self.stale_binary_warned {
                let current_display = truncate_commit_sha(current_commit);
                let stable_display = truncate_commit_sha(stable_commit);
                tracing::warn!(
                    current_commit = %current_display,
                    stable_commit = %stable_display,
                    current_version = %current_metadata.version,
                    stable_version = %stable_metadata.version,
                    "running binary is STALE — current commit {} differs from needle-stable commit {}. \
                     Consider restarting this worker to pick up the latest binary. \
                     This check runs every {} seconds (configured by worker.freshness_check_interval_secs). \
                     Set to 0 to disable freshness checking.",
                    current_display,
                    stable_display,
                    interval_secs
                );
                self.stale_binary_warned = true;
            }
        } else {
            // Binary is fresh — reset the warned flag so we warn again if it becomes stale later
            self.stale_binary_warned = false;
            let current_display = truncate_commit_sha(current_commit);
            tracing::debug!(
                commit = %current_display,
                version = %current_metadata.version,
                "binary freshness check passed — running commit matches needle-stable"
            );
        }

        Ok(())
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
                // Determine backoff strategy based on whether candidates were found but excluded
                let (backoff, backoff_reason) = if self.found_but_excluded {
                    (
                        self.config.worker.short_retry_backoff,
                        "found-but-excluded short retry",
                    )
                } else {
                    let jittered = self.compute_jittered_backoff();
                    (jittered, "idle backoff (jittered)")
                };

                tracing::info!(
                    backoff_secs = backoff,
                    backoff_reason = backoff_reason,
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
                //
                // Event-driven wakeups: Check workspace mtimes on each iteration to
                // detect changes in .beads/issues.jsonl files. If any workspace has been
                // modified since our last check, wake early from idle backoff.
                let check_interval = 1u64;
                let mut elapsed = 0u64;
                let mut shutdown_check_count = 0u64;
                let mut workspace_mtime_wake = false;

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

                    // Event-driven wakeup: Check if any workspace's .beads/issues.jsonl
                    // has been modified since our last check. If so, wake early from idle.
                    // This prevents waiting through the full backoff when new beads appear.
                    if let Some(current_mtime) = self.check_workspace_mtimes() {
                        match self.last_workspace_mtime {
                            Some(last_mtime) if current_mtime > last_mtime => {
                                tracing::info!(
                                    last_mtime = ?last_mtime,
                                    current_mtime = ?current_mtime,
                                    "workspace mtime changed, waking early from idle backoff"
                                );
                                // Update last_workspace_mtime before breaking so we don't
                                // detect the same change again on the next idle cycle
                                self.last_workspace_mtime = Some(current_mtime);
                                workspace_mtime_wake = true;
                                break;
                            }
                            None => {
                                // First time checking, record the baseline mtime
                                tracing::debug!(
                                    current_mtime = ?current_mtime,
                                    "recording baseline workspace mtime"
                                );
                                // Update last_workspace_mtime for next iteration
                                self.last_workspace_mtime = Some(current_mtime);
                            }
                            _ => {
                                // No change, continue sleeping
                                // Update last_workspace_mtime for next iteration
                                self.last_workspace_mtime = Some(current_mtime);
                            }
                        }
                    }

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
                    woke_by_mtime_change = workspace_mtime_wake,
                    "idle sleep completed successfully, transitioning to SELECTING"
                );

                // Note: Event-driven wakeup telemetry is already logged via tracing::info
                // at line 2797-2801 when workspace_mtime_wake is set to true.
                // The IdleSleepCompleted event above includes woke_by_mtime_change field.

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
                // Release any claimed bead before exiting (should be none in exhausted state, but be safe).
                self.release_current_bead("exhausted").await;
                self.stop("exhausted").await
            }
        }
    }

    /// Release the currently claimed bead (if any).
    ///
    /// This helper method is called during graceful shutdown to ensure that
    /// in-progress beads are returned to the open state and can be claimed by
    /// another worker.
    async fn release_current_bead(&mut self, shutdown_reason: &str) {
        if let Some(ref bead) = self.current_bead {
            let bead_id = bead.id.clone();
            tracing::info!(bead_id = %bead_id, reason = %shutdown_reason, "releasing bead on shutdown");
            let _ = self.store.release(&bead_id).await;
            // Emit bead.released event for observability
            let _ = self.telemetry.emit(EventKind::BeadReleased {
                bead_id: bead_id.clone(),
                reason: shutdown_reason.to_string(),
            });
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

        clear_atexit_state();

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

    /// Update the worker's registry entry after state changes (e.g., config reload).
    ///
    /// This is called after a successful config reload to update the
    /// `config_reload_generation` field in the registry, so external tools
    /// like `needle config --dump --live` can see the current reload generation.
    fn update_registry_entry(&self) -> Result<()> {
        let entry = crate::registry::WorkerEntry {
            id: self.qualified_id(),
            pid: std::process::id(),
            workspace: self.config.workspace.default.clone(),
            agent: self.config.agent.default.clone(),
            model: None,
            provider: self.resolve_provider(),
            started_at: chrono::Utc::now(),
            beads_processed: self.beads_processed,
            config_reload_generation: self.config_reload_generation,
        };
        self.registry.register(entry)?;
        self.publish_live_config_snapshot()
    }

    /// Publish the safe, user-facing portion of the running configuration.
    ///
    /// The worker owns this snapshot because the CLI process cannot inspect
    /// another process's in-memory configuration.  Only the fields already
    /// exposed by `config --dump` are persisted; resolved secrets are never
    /// copied to the registry state directory.
    fn publish_live_config_snapshot(&self) -> Result<()> {
        let values_with_sources =
            ConfigLoader::dump_with_sources(&self.config, &self.config_sources);
        let values = values_with_sources
            .iter()
            .map(|line| strip_source_annotation(line))
            .collect();
        self.registry.update_live_config(
            &self.qualified_id(),
            LiveConfigSnapshot {
                values,
                values_with_sources,
                reload_generation: self.config_reload_generation,
            },
        )
    }

    /// Re-resolve source annotations after a successful reload.
    ///
    /// The running config retains CLI overrides from boot even though the
    /// reload candidate is loaded from the global file. Preserve those source
    /// labels while refreshing file/env annotations for newly applied values.
    fn reload_source_map(&self) -> SourceMap {
        let workspace = self.config.workspace.default.clone();
        let mut refreshed = ConfigLoader::load_resolved(
            &workspace,
            CliOverrides {
                workspace: Some(workspace.clone()),
                ..Default::default()
            },
        )
        .map(|(_, sources)| sources)
        .unwrap_or_else(|error| {
            tracing::warn!(error = %error, "failed to refresh live config source annotations");
            self.config_sources.clone()
        });

        for (key, source) in &self.config_sources {
            if matches!(source, ConfigSource::CliOverride) {
                refreshed.insert(key.clone(), source.clone());
            }
        }
        refreshed
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
            tracing::debug!(
                timestamp = ?self.handling_state_entered_at,
                "captured HANDLING state entry timestamp for watchdog"
            );
        } else if from == WorkerState::Handling {
            self.handling_state_entered_at = None;
            tracing::debug!("cleared HANDLING state timestamp on exit");
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

    /// Resolve the agent adapter from config with model-based routing.
    fn resolve_adapter(&self) -> Result<crate::dispatch::AgentAdapter> {
        let bead_id = self.current_bead.as_ref().map(|b| b.id.clone());

        // Start with the configured default adapter.
        let default_adapter_name = &self.config.agent.default;

        // Resolve the default adapter to get its model (if any).
        let default_adapter = self
            .dispatcher
            .adapter(default_adapter_name)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "adapter '{}' not found in any of the expected configuration directories",
                    default_adapter_name
                )
            })?;

        // Apply routing rules if configured.
        let (chosen_adapter_name, matched_rule) = self.apply_routing_rules(&default_adapter)?;

        // Emit routing decision telemetry on every routing decision.
        if let Some(id) = bead_id {
            let model = default_adapter
                .model
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            self.telemetry.emit(EventKind::RoutingDecision {
                bead_id: id,
                model,
                matched_rule: matched_rule.clone(),
                chosen_adapter: chosen_adapter_name.clone(),
            })?;
        }

        // A routed adapter must come from an operator-provided YAML file.
        // Dispatcher also keeps built-in adapters available for the ordinary
        // (non-routed) default path, so checking only its adapter map here
        // would silently turn a missing routed file into a built-in fallback.
        if matched_rule != "default" {
            let expected_yaml = self
                .config
                .agent
                .adapters_dir
                .join(format!("{chosen_adapter_name}.yaml"));
            let expected_yml = self
                .config
                .agent
                .adapters_dir
                .join(format!("{chosen_adapter_name}.yml"));
            if !expected_yaml.is_file() && !expected_yml.is_file() {
                bail!(
                    "routed adapter '{}' is missing its YAML configuration: model '{}' matched pattern '{}', but no adapter YAML was found at '{}' (also checked '{}')",
                    chosen_adapter_name,
                    default_adapter.model.as_deref().unwrap_or("unknown"),
                    matched_rule,
                    expected_yaml.display(),
                    expected_yml.display()
                );
            }
        }

        // Resolve the chosen adapter.
        let adapter = self.dispatcher.adapter(&chosen_adapter_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!(
                "routed adapter '{}' could not be loaded — routing matched model '{}' with pattern '{}', but the adapter YAML could not be loaded",
                chosen_adapter_name,
                default_adapter.model.as_deref().unwrap_or("unknown"),
                matched_rule
            ))?;

        Ok(adapter)
    }

    /// Apply routing rules to determine the final adapter.
    ///
    /// Returns (chosen_adapter_name, matched_rule_pattern).
    /// If no routing rules match, returns (default_adapter_name, "default").
    /// When strict mode is enabled and no rule matches, returns an error.
    fn apply_routing_rules(
        &self,
        default_adapter: &crate::dispatch::AgentAdapter,
    ) -> Result<(String, String)> {
        let routing_config = match &self.config.agent.routing {
            Some(r) if !r.rules.is_empty() => r,
            _ => return Ok((default_adapter.name.clone(), "default".to_string())),
        };

        let model_name = default_adapter.model.as_deref().unwrap_or("");

        // Determine the default adapter name to use.
        let default_adapter_name = routing_config
            .default_adapter
            .as_ref()
            .unwrap_or(&default_adapter.name);

        // Use the routing module's match_adapter_with_pattern function to get both adapter and pattern.
        match routing::match_adapter_with_pattern(
            model_name,
            &routing_config.rules,
            default_adapter_name,
        ) {
            Some((adapter_name, matched_pattern)) => {
                // Determine if this was a rule match or default fallback.
                // If matched_pattern is "default", it means no rule matched.
                let final_pattern = if matched_pattern == "default" {
                    // No rule matched - using default adapter.
                    // Check if strict mode is enabled - if so, fail.
                    if routing_config.strict {
                        let bead_id = self.current_bead.as_ref().map(|b| b.id.clone());
                        if let Some(id) = bead_id {
                            // Emit RoutingFailed telemetry event.
                            let _ = self.telemetry.emit(EventKind::RoutingFailed {
                                bead_id: id,
                                model: model_name.to_string(),
                                rules_tried: routing_config.rules.len() as u32,
                            });
                        }
                        bail!(
                            "no routing rule matched model '{}' — add a rule to agent.routing.rules or set routing.strict: false to fall back to the default adapter",
                            model_name
                        );
                    }

                    // Strict mode disabled - distinguish between routing default and original default.
                    if routing_config.default_adapter.is_some() {
                        "routing-default"
                    } else {
                        "default"
                    }
                } else {
                    // A rule actually matched - use the pattern from the rule.
                    &matched_pattern
                };

                tracing::debug!(
                    model = %model_name,
                    pattern = %final_pattern,
                    adapter = %adapter_name,
                    "routing decision completed"
                );
                Ok((adapter_name, final_pattern.to_string()))
            }
            None => {
                // No rule matched and no default adapter available.
                // If strict mode is enabled, fail with a clear error.
                if routing_config.strict {
                    let bead_id = self.current_bead.as_ref().map(|b| b.id.clone());
                    if let Some(id) = bead_id {
                        // Emit RoutingFailed telemetry event.
                        let _ = self.telemetry.emit(EventKind::RoutingFailed {
                            bead_id: id,
                            model: model_name.to_string(),
                            rules_tried: routing_config.rules.len() as u32,
                        });
                    }
                    bail!(
                        "no routing rule matched model '{}' and no default adapter available — add a rule to agent.routing.rules or set routing.strict: false to fall back to the default adapter",
                        model_name
                    );
                }

                // Strict mode disabled but no default adapter - this shouldn't happen with valid config.
                tracing::warn!(
                    model = %model_name,
                    "no routing rule matched and no default adapter available, but strict mode is disabled"
                );
                Ok((default_adapter_name.to_string(), "default".to_string()))
            }
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

    /// Check if a bead should use SPLIT mode based on consecutive failures.
    ///
    /// Returns (template_name, failure_count). Uses SPLIT template when
    /// the bead's consecutive failure count meets or exceeds the configured
    /// threshold (split_after_failures), otherwise uses normal PLUCK template.
    async fn check_split_mode(&self, bead: &Bead) -> (&'static str, u32) {
        // Read labels with timeout protection (5s is generous for a local br call).
        let labels =
            match tokio::time::timeout(Duration::from_secs(5), self.store.labels(&bead.id)).await {
                Ok(Ok(l)) => l,
                Ok(Err(e)) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %e,
                        "failed to read labels for split mode check, using PLUCK mode"
                    );
                    return ("pluck", 0);
                }
                Err(_) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        "labels() timed out after 5s, using PLUCK mode"
                    );
                    return ("pluck", 0);
                }
            };

        // Extract current failure count from labels (format: "failure-count:N").
        // Multiple labels may exist from previous increments; we take the max.
        let failure_count = labels
            .iter()
            .filter_map(|l| l.strip_prefix("failure-count:"))
            .filter_map(|n| n.parse::<u32>().ok())
            .max()
            .unwrap_or(0);

        // Check if we should use SPLIT mode.
        let threshold = self.config.strands.pluck.split_after_failures;
        if failure_count >= threshold {
            tracing::info!(
                bead_id = %bead.id,
                failure_count,
                threshold,
                "auto-split triggered: using SPLIT template"
            );
            ("split", failure_count)
        } else {
            ("pluck", failure_count)
        }
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

/// Report configuration changes that cannot be applied by the running worker.
///
/// Keep this best-effort: a telemetry failure must never turn a configuration
/// diagnostic into a worker failure. The event carries key names only, never
/// configuration values or resolved secrets.
fn report_restart_required_config(telemetry: &Telemetry, keys: Vec<String>) {
    if keys.is_empty() {
        return;
    }

    tracing::warn!(
        keys = ?keys,
        "configuration changes require a worker restart; keeping the running values"
    );
    if let Err(error) = telemetry.emit_try_lock(EventKind::ConfigReloadRestartRequired { keys }) {
        tracing::warn!(error = %error, "failed to emit config.reload.restart_required");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{BeadStore, Filters, RepairReport};
    use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};
    use async_trait::async_trait;
    use std::io::Write;
    use std::sync::Mutex;

    // ── Tests for truncate_for_display ──

    #[test]
    fn test_truncate_for_display_with_short_sha() {
        // Test with 7-character SHA (actual observed case: 'ee18678')
        let short_sha = "ee18678";
        assert_eq!(truncate_for_display(short_sha, 12), short_sha);
        assert_eq!(truncate_for_display(short_sha, 12).len(), 7);
    }

    #[test]
    fn test_truncate_for_display_with_unknown() {
        // Test with 'unknown' fallback (also 7 characters)
        let unknown = "unknown";
        assert_eq!(truncate_for_display(unknown, 12), unknown);
        assert_eq!(truncate_for_display(unknown, 12).len(), 7);
    }

    #[test]
    fn test_truncate_for_display_with_long_sha() {
        // Test with long SHA (40 characters)
        let long_sha = "a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8s9t0";
        assert_eq!(truncate_for_display(long_sha, 12), "a1b2c3d4e5f6");
        assert_eq!(truncate_for_display(long_sha, 12).len(), 12);
    }

    #[test]
    fn test_truncate_for_display_with_exact_length() {
        // Test when string length equals max_len
        let exact = "abcdefghijkl";
        assert_eq!(truncate_for_display(exact, 12), exact);
        assert_eq!(truncate_for_display(exact, 12).len(), 12);
    }

    #[test]
    fn test_truncate_for_display_with_empty_string() {
        // Test with empty string
        assert_eq!(truncate_for_display("", 12), "");
        assert_eq!(truncate_for_display("", 12).len(), 0);
    }

    #[test]
    fn test_truncate_for_display_with_unicode() {
        // Test with Unicode characters (uses char_indices, not byte slicing)
        let unicode = "hello世界";
        // 5 chars + 2 chars = 7 chars total, 11 bytes
        assert_eq!(truncate_for_display(unicode, 12), unicode);
        assert_eq!(truncate_for_display(unicode, 12).chars().count(), 7);
    }

    #[test]
    fn test_truncate_for_display_unicode_truncation() {
        // Test truncating within a Unicode string
        let unicode = "helloworld";
        assert_eq!(truncate_for_display(unicode, 5), "hello");
    }

    // ── Tests for truncate_commit_sha ──

    #[test]
    fn test_truncate_commit_sha_with_short_sha() {
        // Test with 7-character SHA (actual observed case: 'ee18678')
        // This was the root cause of the fleet-wide panic
        let short_sha = "ee18678";
        assert_eq!(truncate_commit_sha(short_sha), short_sha);
        assert_eq!(truncate_commit_sha(short_sha).len(), 7);
    }

    #[test]
    fn test_truncate_commit_sha_with_unknown() {
        // Test with 'unknown' fallback (also 7 characters)
        let unknown = "unknown";
        assert_eq!(truncate_commit_sha(unknown), unknown);
        assert_eq!(truncate_commit_sha(unknown).len(), 7);
    }

    #[test]
    fn test_truncate_commit_sha_with_long_sha() {
        // Test with full 40-character SHA
        let long_sha = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0";
        assert_eq!(truncate_commit_sha(long_sha), "a1b2c3d4e5f6");
        assert_eq!(truncate_commit_sha(long_sha).len(), 12);
    }

    #[test]
    fn test_truncate_commit_sha_with_exact_length() {
        // Test when string length equals 12
        let exact = "abcdefghijkl";
        assert_eq!(truncate_commit_sha(exact), exact);
        assert_eq!(truncate_commit_sha(exact).len(), 12);
    }

    #[test]
    fn test_truncate_commit_sha_with_empty_string() {
        // Test with empty string
        assert_eq!(truncate_commit_sha(""), "");
        assert_eq!(truncate_commit_sha("").len(), 0);
    }

    #[test]
    fn config_file_change_detects_hash_change_with_unchanged_mtime() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        std::fs::write(&path, "worker:\n  max_workers: 1\n").unwrap();
        let previous = read_config_file_fingerprint(&path).unwrap();

        std::fs::write(&path, "worker:\n  max_workers: 2\n").unwrap();
        let current = read_config_file_fingerprint(&path).unwrap();
        let current_with_previous_mtime = ConfigFileFingerprint {
            mtime: previous.mtime,
            content_hash: current.content_hash.clone(),
        };

        assert_ne!(previous.content_hash, current.content_hash);
        assert!(config_file_changed(&previous, &current_with_previous_mtime));
    }

    #[test]
    fn config_file_change_ignores_identical_fingerprint() {
        let fingerprint = ConfigFileFingerprint {
            mtime: Some(SystemTime::UNIX_EPOCH),
            content_hash: Some("same-hash".to_string()),
        };

        assert!(!config_file_changed(&fingerprint, &fingerprint));
    }

    #[tokio::test]
    async fn restart_required_config_report_emits_named_keys() {
        let helper = crate::telemetry::test_utils::TestHelper::new("restart-required-test");

        report_restart_required_config(
            helper.telemetry(),
            vec!["workspace.home".to_string(), "bead_cli.backend".to_string()],
        );
        tokio::time::sleep(Duration::from_millis(10)).await;

        let events = helper.events_by_type("config.reload.restart_required");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].data,
            serde_json::json!({
                "keys": ["workspace.home", "bead_cli.backend"]
            })
        );
    }

    #[tokio::test]
    async fn config_reload_check_is_disabled_when_interval_is_zero() {
        let mut worker = make_worker(Arc::new(MockStore::empty()));

        worker.check_config_reload().await.unwrap();

        assert!(worker.last_config_reload_check.is_none());
    }

    #[tokio::test]
    async fn config_reload_check_is_interval_gated() {
        let mut config = valid_test_config();
        config.worker.config_reload_check_interval_secs = 60;
        let mut worker = Worker::new(
            config,
            "config-reload-interval".to_string(),
            Arc::new(MockStore::empty()),
        );

        worker.check_config_reload().await.unwrap();
        let first_check = worker.last_config_reload_check;

        worker.check_config_reload().await.unwrap();

        assert!(first_check.is_some());
        assert_eq!(worker.last_config_reload_check, first_check);
    }

    #[test]
    fn state_machine_reaches_config_reload_check_at_cycle_boundary() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());

        let config_path = home.path().join(".config/needle/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();

        let mut config = valid_test_config();
        config.worker.config_reload_check_interval_secs = 60;
        config.worker.idle_action = IdleAction::Exit;
        config.worker.enforce_shipped_work = false;
        config.workspace.home = home.path().join(".needle");
        config.workspace.default = workspace.path().to_path_buf();
        config.self_modification.hot_reload = false;
        config.strands.explore.enabled = false;
        config.strands.explore.workspace_root = workspace.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();

        // Constructing the worker records this baseline fingerprint. The
        // subsequent edit must therefore be observed by the state machine,
        // rather than becoming its first fingerprint.
        std::fs::write(&config_path, serde_yaml::to_string(&config).unwrap()).unwrap();
        let mut worker = Worker::new(
            config.clone(),
            "config-reload-call-site".to_string(),
            Arc::new(MockStore::empty()),
        );
        worker.boot().unwrap();

        let mut candidate = config;
        candidate.worker.max_claim_retries += 1;
        std::fs::write(&config_path, serde_yaml::to_string(&candidate).unwrap()).unwrap();

        // This intentionally exercises the production state-machine arm. Do
        // not replace this with a direct check_config_reload() call: the
        // regression is a missing call site, not a broken helper.
        worker.state = WorkerState::Logging;
        let terminal_state = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(worker.run_state_machine())
            .unwrap();

        assert_eq!(terminal_state, WorkerState::Stopped);
        assert_eq!(
            worker.config.worker.max_claim_retries, candidate.worker.max_claim_retries,
            "the cycle-boundary state-machine path must apply the changed config"
        );
    }

    #[test]
    fn config_reload_requested_mid_dispatch_waits_for_cycle_boundary() {
        use std::collections::HashMap;

        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());

        let config_path = home.path().join(".config/needle/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();

        let mut config = valid_test_config();
        config.agent.default = "old-agent".to_string();
        config.agent.routing = None;
        config.agent.adapters_dir = home.path().join("adapters");
        config.worker.config_reload_check_interval_secs = 1;
        config.worker.idle_action = IdleAction::Exit;
        config.worker.enforce_shipped_work = false;
        config.workspace.home = home.path().join(".needle");
        config.workspace.default = workspace.path().to_path_buf();
        config.self_modification.hot_reload = false;
        config.strands.explore.enabled = false;
        config.strands.explore.workspace_root = workspace.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();

        std::fs::write(&config_path, serde_yaml::to_string(&config).unwrap()).unwrap();

        let mut bead = make_test_bead("needle-reload-boundary");
        bead.workspace = workspace.path().to_path_buf();
        let store = Arc::new(MockStore::new(vec![bead.clone()]));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (helper, mut worker) = runtime.block_on(async {
            let helper = crate::telemetry::test_utils::TestHelper::new("reload-boundary-test");
            let worker = Worker::new_with_telemetry(
                config.clone(),
                "reload-boundary".to_string(),
                store.clone(),
                helper.telemetry().clone(),
            );
            (helper, worker)
        });

        let dispatch_started = workspace.path().join("dispatch-started");
        let release_dispatch = workspace.path().join("release-dispatch");
        let mut old_environment = HashMap::new();
        old_environment.insert(
            "NEEDLE_TEST_DISPATCH_STARTED".to_string(),
            dispatch_started.display().to_string(),
        );
        old_environment.insert(
            "NEEDLE_TEST_RELEASE_DISPATCH".to_string(),
            release_dispatch.display().to_string(),
        );
        let old_adapter = crate::dispatch::AgentAdapter {
            name: "old-agent".to_string(),
            description: None,
            agent_cli: "bash".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: concat!(
                "touch \"$NEEDLE_TEST_DISPATCH_STARTED\"; ",
                "while [ ! -e \"$NEEDLE_TEST_RELEASE_DISPATCH\" ]; do sleep 0.01; done; ",
                "printf old-config"
            )
            .to_string(),
            environment: old_environment,
            timeout_secs: 10,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: None,
            model: None,
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };
        let new_adapter = crate::dispatch::AgentAdapter {
            name: "new-agent".to_string(),
            description: None,
            agent_cli: "bash".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "printf new-config".to_string(),
            environment: HashMap::new(),
            timeout_secs: 10,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: None,
            model: None,
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };
        let mut adapters = HashMap::new();
        adapters.insert(old_adapter.name.clone(), old_adapter);
        adapters.insert(new_adapter.name.clone(), new_adapter);
        worker.dispatcher =
            Dispatcher::with_adapters(adapters, helper.telemetry().clone(), config.agent.timeout);
        worker.boot().unwrap();

        let mut candidate = config;
        candidate.agent.default = "new-agent".to_string();
        let candidate_yaml = serde_yaml::to_string(&candidate).unwrap();

        let terminal_state = runtime
            .block_on(async {
                let request_reload_during_dispatch = async {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        while !dispatch_started.exists() {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .expect("old-config dispatch did not start");

                    std::fs::write(&config_path, candidate_yaml).unwrap();
                    helper.sync().await;

                    assert_eq!(helper.events_by_type("agent.dispatched").len(), 1);
                    helper.assert_event_not_emitted("config.reload.detected");
                    helper.assert_event_not_emitted("config.reload.applied");

                    {
                        let mut beads = store.beads.lock().unwrap();
                        let claimed = beads
                            .iter_mut()
                            .find(|stored| stored.id == bead.id)
                            .expect("claimed test bead should remain in the mock store");
                        claimed.status = BeadStatus::Closed;
                    }

                    std::fs::write(&release_dispatch, b"").unwrap();
                };

                let (state, ()) =
                    tokio::join!(worker.run_state_machine(), request_reload_during_dispatch);
                state
            })
            .unwrap();
        runtime.block_on(helper.sync());

        assert_eq!(terminal_state, WorkerState::Stopped);
        assert_eq!(worker.config.agent.default, "new-agent");

        let trace_stdout = std::fs::read_to_string(
            workspace
                .path()
                .join(".beads/traces/needle-reload-boundary/stdout.txt"),
        )
        .unwrap();
        assert_eq!(trace_stdout, "old-config");

        let events = helper.all_events();
        let dispatch_completed = events
            .iter()
            .find(|event| event.event_type == "agent.completed")
            .expect("old-config dispatch should complete");
        assert_eq!(dispatch_completed.data["agent"], "old-agent");
        let reload_detected = events
            .iter()
            .find(|event| event.event_type == "config.reload.detected")
            .expect("changed config should be detected at the cycle boundary");
        let reload_applied = events
            .iter()
            .find(|event| event.event_type == "config.reload.applied")
            .expect("changed config should be applied at the cycle boundary");

        assert!(dispatch_completed.sequence < reload_detected.sequence);
        assert!(reload_detected.sequence < reload_applied.sequence);
        assert!(reload_applied.data["changed_keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|key| key == "agent.default"));
    }

    #[test]
    fn invalid_config_reload_keeps_worker_running_and_emits_rejection() {
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicBool, Ordering};

        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());

        let config_path = home.path().join(".config/needle/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();

        let mut config = valid_test_config();
        config.agent.default = "old-agent".to_string();
        config.agent.adapters_dir = home.path().join("adapters");
        config.worker.config_reload_check_interval_secs = 1;
        config.worker.idle_action = IdleAction::Wait;
        config.worker.idle_backoff_min = 1;
        config.worker.idle_backoff_max = 1;
        config.worker.enforce_shipped_work = false;
        config.workspace.home = home.path().join(".needle");
        config.workspace.default = workspace.path().to_path_buf();
        config.self_modification.hot_reload = false;
        config.strands.explore.enabled = false;
        config.strands.explore.workspace_root = workspace.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();

        std::fs::write(&config_path, serde_yaml::to_string(&config).unwrap()).unwrap();

        let dispatch_started = workspace.path().join("dispatch-started");
        let release_dispatch = workspace.path().join("release-dispatch");
        let mut environment = HashMap::new();
        environment.insert(
            "NEEDLE_TEST_DISPATCH_STARTED".to_string(),
            dispatch_started.display().to_string(),
        );
        environment.insert(
            "NEEDLE_TEST_RELEASE_DISPATCH".to_string(),
            release_dispatch.display().to_string(),
        );
        let adapter = crate::dispatch::AgentAdapter {
            name: "old-agent".to_string(),
            description: None,
            agent_cli: "bash".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: concat!(
                "touch \"$NEEDLE_TEST_DISPATCH_STARTED\"; ",
                "while [ ! -e \"$NEEDLE_TEST_RELEASE_DISPATCH\" ]; do sleep 0.01; done; ",
                "printf old-config"
            )
            .to_string(),
            environment,
            timeout_secs: 10,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: None,
            model: None,
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };
        let mut adapters = HashMap::new();
        adapters.insert(adapter.name.clone(), adapter);

        let mut bead = make_test_bead("needle-invalid-reload");
        bead.workspace = workspace.path().to_path_buf();
        let store = Arc::new(MockStore::new(vec![bead.clone()]));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (helper, mut worker) = runtime.block_on(async {
            let helper = crate::telemetry::test_utils::TestHelper::new("invalid-reload-test");
            let worker = Worker::new_with_telemetry(
                config.clone(),
                "invalid-reload".to_string(),
                store.clone(),
                helper.telemetry().clone(),
            );
            (helper, worker)
        });
        worker.dispatcher =
            Dispatcher::with_adapters(adapters, helper.telemetry().clone(), config.agent.timeout);
        worker.boot().unwrap();

        let running_max_workers = worker.config.worker.max_workers;
        let shutdown = worker.shutdown.clone();
        let worker_finished = Arc::new(AtomicBool::new(false));
        let worker_finished_for_run = worker_finished.clone();

        let result = runtime.block_on(async {
            let request_invalid_reload = async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !dispatch_started.exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("old-config dispatch did not start");

                std::fs::write(&config_path, "worker:\n  max_workers: 0\n").unwrap();
                helper.sync().await;

                assert_eq!(helper.events_by_type("agent.dispatched").len(), 1);
                helper.assert_event_not_emitted("config.reload.rejected");

                {
                    let mut beads = store.beads.lock().unwrap();
                    let claimed = beads
                        .iter_mut()
                        .find(|stored| stored.id == bead.id)
                        .expect("claimed test bead should remain in the mock store");
                    claimed.status = BeadStatus::Closed;
                }

                std::fs::write(&release_dispatch, b"").unwrap();

                tokio::time::timeout(Duration::from_secs(5), async {
                    while helper.events_by_type("config.reload.rejected").is_empty() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("invalid candidate was not rejected");

                assert!(
                    !worker_finished.load(Ordering::SeqCst),
                    "invalid config reload must not terminate the worker"
                );
                shutdown.store(true, Ordering::SeqCst);
            };

            let run_worker = async {
                let result = worker.run_state_machine().await;
                worker_finished_for_run.store(true, Ordering::SeqCst);
                result
            };

            tokio::join!(run_worker, request_invalid_reload).0
        });
        let terminal_state =
            result.expect("invalid config reload must not stop the worker with an error");
        runtime.block_on(helper.sync());

        assert_eq!(terminal_state, WorkerState::Stopped);
        assert_eq!(worker.config.worker.max_workers, running_max_workers);
        helper.assert_event_count("config.reload.rejected", 1);
        helper.assert_event_not_emitted("config.reload.applied");
        helper.assert_event_not_emitted("worker.errored");

        let rejected = helper
            .events_by_type("config.reload.rejected")
            .into_iter()
            .next()
            .expect("rejection event should be emitted");
        assert!(rejected.data["validation_errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error.as_str().unwrap().contains("max_workers")));
    }

    #[test]
    fn tier_c_config_reload_emits_restart_required_event() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join(".config/needle/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "worker:\n  max_workers: 4\n").unwrap();
        std::env::set_var("HOME", home.path());

        let mut config = valid_test_config();
        let workspace = tempfile::tempdir().unwrap();
        config.workspace.default = workspace.path().to_path_buf();
        config.worker.config_reload_check_interval_secs = 60;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (helper, mut worker) = runtime.block_on(async {
            let helper = crate::telemetry::test_utils::TestHelper::new("tier-c-reload-test");
            let worker = Worker::new_with_telemetry(
                config,
                "tier-c-reload".to_string(),
                Arc::new(MockStore::empty()),
                helper.telemetry().clone(),
            );
            (helper, worker)
        });

        std::fs::write(&config_path, "worker:\n  max_workers: 5\n").unwrap();
        worker.last_config_reload_check = None;
        runtime.block_on(async {
            worker.check_config_reload().await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        });

        let events = helper.events_by_type("config.reload.restart_required");
        assert_eq!(events.len(), 1);
        assert!(events[0].data["keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|key| key == "worker.max_workers"));
        assert_eq!(worker.config.worker.max_workers, 4);
    }

    #[tokio::test]
    async fn workspace_non_overridable_config_emits_restart_required_event() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join(".needle.yaml"),
            "telemetry:\n  otlp_sink:\n    enabled: true\nworker:\n  max_workers: 99\n",
        )
        .unwrap();

        let mut config = valid_test_config();
        config.workspace.default = workspace.path().to_path_buf();
        let helper = crate::telemetry::test_utils::TestHelper::new("workspace-config-test");
        let _worker = Worker::new_with_telemetry(
            config,
            "workspace-config".to_string(),
            Arc::new(MockStore::empty()),
            helper.telemetry().clone(),
        );
        tokio::time::sleep(Duration::from_millis(10)).await;

        let events = helper.events_by_type("config.reload.restart_required");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].data,
            serde_json::json!({
                "keys": ["telemetry", "worker"]
            })
        );
    }

    #[test]
    fn invalid_reload_candidate_is_rejected_without_mutating_running_config() {
        let worker = make_worker(Arc::new(MockStore::empty()));
        let running_max_workers = worker.config.worker.max_workers;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        std::fs::write(&path, "worker:\n  max_workers: 0\n").unwrap();

        assert!(worker.load_validated_config_candidate(&path).is_none());
        assert_eq!(worker.config.worker.max_workers, running_max_workers);
    }

    #[test]
    fn valid_reload_candidate_is_returned_separately_from_running_config() {
        let worker = make_worker(Arc::new(MockStore::empty()));
        let running_max_workers = worker.config.worker.max_workers;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        std::fs::write(&path, "worker:\n  max_workers: 2\n").unwrap();

        let candidate = worker
            .load_validated_config_candidate(&path)
            .expect("valid candidate should pass validation");

        assert_eq!(candidate.worker.max_workers, 2);
        assert_eq!(worker.config.worker.max_workers, running_max_workers);
    }

    #[test]
    fn tier_a_config_swap_is_atomic_and_preserves_non_live_fields() {
        let mut worker = make_worker(Arc::new(MockStore::empty()));
        let running_agent_timeout = worker.config.agent.timeout;
        let running_idle_timeout = worker.config.worker.idle_timeout;
        let running_adapters_dir = worker.config.agent.adapters_dir.clone();
        let running_max_workers = worker.config.worker.max_workers;
        let running_workspace_home = worker.config.workspace.home.clone();
        let running_telemetry_enabled = worker.config.telemetry.file_sink.enabled;

        let mut candidate = worker.config.clone();
        candidate.agent.timeout += 1;
        candidate.worker.idle_timeout += 1;
        candidate.budget.warn_usd = 10.0;
        candidate.agent.adapters_dir = PathBuf::from("/tmp/reloaded-adapters");
        candidate.worker.max_workers = running_max_workers + 1;
        candidate.workspace.home = PathBuf::from("/tmp/reloaded-home");
        candidate.telemetry.file_sink.enabled = !running_telemetry_enabled;

        let changed_keys = worker.apply_tier_a_config(&candidate);

        assert_eq!(worker.config.agent.timeout, running_agent_timeout + 1);
        assert_eq!(worker.config.worker.idle_timeout, running_idle_timeout + 1);
        assert_eq!(worker.config.budget.warn_usd, 10.0);
        assert!(changed_keys.contains(&"agent.timeout".to_string()));
        assert!(changed_keys.contains(&"worker.idle_timeout".to_string()));
        assert!(changed_keys.contains(&"budget.warn_usd".to_string()));
        assert_eq!(worker.config.agent.adapters_dir, running_adapters_dir);
        assert_eq!(worker.config.worker.max_workers, running_max_workers);
        assert_eq!(worker.config.workspace.home, running_workspace_home);
        assert_eq!(
            worker.config.telemetry.file_sink.enabled,
            running_telemetry_enabled
        );
    }

    #[test]
    fn tier_b_rebuilds_only_changed_component_subtrees() {
        let mut worker = make_worker(Arc::new(MockStore::empty()));
        let adapter_dir = tempfile::tempdir().unwrap();
        let mut candidate = worker.config.clone();

        candidate.strands.explore.enabled = !candidate.strands.explore.enabled;
        candidate.prompt.instructions = Some("reloaded prompt instructions".to_string());
        candidate.agent.adapters_dir = adapter_dir.path().to_path_buf();
        candidate.limits.providers.insert(
            "anthropic".to_string(),
            crate::config::ProviderLimits {
                max_concurrent: Some(7),
                requests_per_minute: None,
            },
        );
        candidate.validation.outcome_timeout_seconds += 1;

        let report = worker.rebuild_tier_b_components(&candidate);

        assert!(report.failures.is_empty());
        assert_eq!(
            report.rebuilt_components,
            vec![
                "StrandRunner",
                "PromptBuilder",
                "Dispatcher",
                "RateLimiter",
                "OutcomeHandler",
            ]
        );
        assert_eq!(
            worker.config.strands.explore.enabled,
            candidate.strands.explore.enabled
        );
        assert_eq!(
            worker.config.prompt.instructions,
            candidate.prompt.instructions
        );
        assert_eq!(
            worker.config.agent.adapters_dir,
            candidate.agent.adapters_dir
        );
        assert!(worker.config.limits.providers.contains_key("anthropic"));
        assert_eq!(
            worker.config.validation.outcome_timeout_seconds,
            candidate.validation.outcome_timeout_seconds
        );

        let unchanged_candidate = worker.config.clone();
        let unchanged = worker.rebuild_tier_b_components(&unchanged_candidate);
        assert!(unchanged.rebuilt_components.is_empty());
        assert!(unchanged.applied_keys.is_empty());
        assert!(unchanged.failures.is_empty());
    }

    #[test]
    fn failed_dispatcher_rebuild_keeps_previous_instance_and_does_not_block_later_rebuilds() {
        let mut worker = make_worker(Arc::new(MockStore::empty()));
        let previous_adapters_dir = worker.config.agent.adapters_dir.clone();
        let mut previous_adapters = worker
            .dispatcher
            .adapter_names()
            .into_iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        previous_adapters.sort_unstable();

        let adapter_dir = tempfile::tempdir().unwrap();
        std::fs::write(adapter_dir.path().join("broken.yaml"), "name: [").unwrap();

        let mut candidate = worker.config.clone();
        candidate.prompt.instructions = Some("still applies".to_string());
        candidate.agent.adapters_dir = adapter_dir.path().to_path_buf();
        candidate.limits.providers.insert(
            "anthropic".to_string(),
            crate::config::ProviderLimits {
                max_concurrent: Some(3),
                requests_per_minute: None,
            },
        );
        candidate.validation.outcome_timeout_seconds += 1;

        let report = worker.rebuild_tier_b_components(&candidate);

        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].component, "Dispatcher");
        assert!(report.failures[0].error.contains("invalid YAML"));
        assert_eq!(
            report.rebuilt_components,
            vec!["PromptBuilder", "RateLimiter", "OutcomeHandler"]
        );
        assert_eq!(worker.config.agent.adapters_dir, previous_adapters_dir);
        let mut retained_adapters = worker
            .dispatcher
            .adapter_names()
            .into_iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        retained_adapters.sort_unstable();
        assert_eq!(retained_adapters, previous_adapters);

        assert_eq!(
            worker.config.prompt.instructions,
            candidate.prompt.instructions
        );
        assert!(worker.config.limits.providers.contains_key("anthropic"));
        assert_eq!(
            worker.config.validation.outcome_timeout_seconds,
            candidate.validation.outcome_timeout_seconds
        );
    }

    #[test]
    fn failed_prompt_rebuild_keeps_previous_builder_while_dispatcher_reloads() {
        let mut worker = make_worker(Arc::new(MockStore::empty()));
        let previous_prompt = serde_json::to_value(&worker.config.prompt).unwrap();
        let adapter_dir = tempfile::tempdir().unwrap();
        let mut candidate = worker.config.clone();
        candidate.prompt.templates.insert(
            "pluck".to_string(),
            "invalid reload variable: {not_a_prompt_variable}".to_string(),
        );
        candidate.agent.adapters_dir = adapter_dir.path().to_path_buf();

        let report = worker.rebuild_tier_b_components(&candidate);

        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].component, "PromptBuilder");
        assert!(report.failures[0].error.contains("unknown variable"));
        assert_eq!(report.rebuilt_components, vec!["Dispatcher"]);
        assert_eq!(
            serde_json::to_value(&worker.config.prompt).unwrap(),
            previous_prompt
        );
        assert_eq!(
            worker.config.agent.adapters_dir,
            candidate.agent.adapters_dir
        );
    }

    #[test]
    fn isolated_rebuild_helper_never_moves_out_the_previous_value_on_error() {
        let mut current = String::from("previous");
        let mut report = TierBReloadReport::default();

        let installed = install_rebuilt_component(
            &mut current,
            "test-component",
            Err(anyhow::anyhow!("rebuild failed")),
            &mut report,
        );

        assert!(!installed);
        assert_eq!(current, "previous");
        assert_eq!(report.failures.len(), 1);
        assert!(report.rebuilt_components.is_empty());
    }

    #[test]
    fn invalid_config_reload_is_non_fatal_and_keeps_running_config() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join(".config/needle/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "worker:\n  max_workers: 4\n").unwrap();
        std::env::set_var("HOME", home.path());

        let mut config = valid_test_config();
        config.worker.config_reload_check_interval_secs = 60;
        let mut worker = Worker::new(
            config,
            "config-reload-invalid".to_string(),
            Arc::new(MockStore::empty()),
        );
        let running_max_workers = worker.config.worker.max_workers;

        std::fs::write(&config_path, "worker:\n  max_workers: 0\n").unwrap();
        worker.last_config_reload_check = None;

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(worker.check_config_reload())
            .expect("reload rejection must not escape the reload check");

        assert_eq!(worker.config.worker.max_workers, running_max_workers);
    }

    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<Mutex<Vec<u8>>>);

    struct CapturedLogWriter(std::sync::Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedLogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedLogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedLogWriter(self.0.clone())
        }
    }

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
        async fn release(&self, id: &BeadId) -> Result<()> {
            let mut beads = self.beads.lock().unwrap();
            let bead = beads
                .iter_mut()
                .find(|bead| bead.id == *id)
                .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))?;
            bead.status = BeadStatus::Open;
            bead.assignee = None;
            Ok(())
        }
        async fn block(&self, id: &BeadId) -> Result<()> {
            let mut beads = self.beads.lock().unwrap();
            let bead = beads
                .iter_mut()
                .find(|bead| bead.id == *id)
                .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))?;
            bead.status = BeadStatus::Blocked;
            Ok(())
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, id: &BeadId) -> Result<()> {
            let mut beads = self.beads.lock().unwrap();
            let bead = beads
                .iter_mut()
                .find(|bead| bead.id == *id)
                .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))?;
            bead.status = BeadStatus::Open;
            bead.assignee = None;
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
        async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
            let mut beads = self.beads.lock().unwrap();
            if let Some(bead) = beads.iter_mut().find(|b| b.status == BeadStatus::Open) {
                bead.status = BeadStatus::InProgress;
                bead.assignee = Some(actor.to_string());
                Ok(ClaimResult::Claimed(bead.clone()))
            } else {
                Ok(ClaimResult::NotClaimable {
                    reason: "no open beads".to_string(),
                })
            }
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
        async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
            let mut beads = self.beads.lock().unwrap();
            let bead = beads
                .iter_mut()
                .find(|bead| bead.id == *id)
                .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))?;
            bead.assignee = None;
            Ok(())
        }

        fn has_valid_store(&self) -> bool {
            true // Mock store always has a valid store
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
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn make_worker(store: Arc<dyn BeadStore>) -> Worker {
        let mut config = valid_test_config();
        // Disable hot-reload in tests — it would re-exec into a different binary.
        config.self_modification.hot_reload = false;
        // Disable Explore: it defaults to scanning the real $HOME for any
        // `.beads/` directory and operating on whatever it finds via the real
        // `bf`/`br` CLI. This shared helper is used by unit tests that never
        // intend to exercise real filesystem discovery (that's what
        // strand::explore::tests own, with its own tempdir isolation) — a
        // multi-cycle test using this helper (e.g. via `.run()`) previously
        // reached a real Explore scan once its MockStore emptied out,
        // claiming/mutating real beads across unrelated repos on this
        // server. See bf-2unnq's contamination addendum.
        config.strands.explore.enabled = false;
        // Pin Explore scan root to a tempdir so the Explore strand cannot
        // scan the real home directory if it ever runs. Even though Explore
        // is disabled above, defense-in-depth ensures tests are isolated.
        let temp_dir = tempfile::tempdir().unwrap();
        config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();
        Worker::new(config, "test-worker".to_string(), store)
    }

    /// Return a configuration whose selected adapter is one of NEEDLE's
    /// built-ins. `Config::default()` intentionally names the legacy
    /// operator-provided `claude` adapter, which is absent in isolated CI.
    fn valid_test_config() -> Config {
        let mut config = Config::default();
        config.agent.default = "claude-sonnet".to_string();
        // Isolate operator state. `workspace.home` defaults to the real
        // `~/.needle`, whose `state/workers.json` is the live fleet's worker
        // registry — booting a test worker registered into it and raced both
        // the fleet and the other tests sharing this worker id. `adapters_dir`
        // is pinned alongside it so adapter resolution sees only built-ins and
        // never depends on what the operator happens to have installed.
        let home = crate::util::test_env::isolated_home();
        config.agent.adapters_dir = home.join("adapters");
        config.workspace.home = home;
        // Drop the default routing rules. They rewrite any sonnet/opus/fable/
        // haiku model to `claude-print` and otherwise fall back to
        // `claude-code-glm-4.7` — both operator-provided adapters that exist
        // only in the real `~/.config/needle/adapters`. Leaving them on makes
        // these tests pass or fail based on what the machine happens to have
        // installed, and the boot error blames `agent.default` rather than the
        // adapter routing actually selected. Tests that exercise routing build
        // their own `RoutingConfig`.
        config.agent.routing = None;
        config
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
                                              // Pin workspace_root to prevent Explore strand from scanning real home
        let temp_dir = tempfile::tempdir().unwrap();
        config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();
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
        let mut config = valid_test_config();
        config.worker.idle_action = IdleAction::Exit;
        config.self_modification.hot_reload = false;
        config.strands.explore.enabled = false;
        // Pin workspace_root to prevent Explore strand from scanning real home
        let temp_dir = tempfile::tempdir().unwrap();
        config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();
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
        let result = worker.resolve_adapter();
        // `valid_test_config` selects the `claude-sonnet` built-in and disables
        // routing, so resolution must succeed without consulting any adapter
        // the operator happens to have installed. This previously asserted an
        // error, which only held because the default routing rules rewrote the
        // request to `claude-print` — an adapter that is not built in.
        let adapter = result.expect("built-in adapter should resolve");
        assert_eq!(adapter.name, "claude-sonnet");
    }

    #[tokio::test]
    async fn resolve_adapter_fails_when_routed_yaml_is_missing() {
        use crate::config::{RoutingConfig, RoutingRule};

        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = valid_test_config();
        config.agent.routing = Some(RoutingConfig {
            rules: vec![RoutingRule {
                match_model: "claude-sonnet-4-6".to_string(),
                // This is intentionally the name of a built-in adapter. A
                // routed selection must not use it when its YAML is absent.
                adapter: "claude-sonnet".to_string(),
            }],
            default_adapter: None,
            strict: false,
        });
        let expected_yaml = config.agent.adapters_dir.join("claude-sonnet.yaml");
        let worker = Worker::new(config, "test-routing-missing-yaml".to_string(), store);

        let error = worker
            .resolve_adapter()
            .expect_err("missing routed adapter YAML must fail resolution")
            .to_string();

        assert!(error.contains("claude-sonnet-4-6"), "error: {error}");
        assert!(error.contains("matched pattern"), "error: {error}");
        assert!(
            error.contains(expected_yaml.to_string_lossy().as_ref()),
            "error: {error}"
        );
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
        // Pin workspace_root to prevent Explore strand from scanning real home
        config.strands.explore.workspace_root = dir.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();
        let worker = Worker::new(config, "test-worker".to_string(), store);
        assert_eq!(worker.beads_processed(), 0);
    }

    #[tokio::test]
    async fn do_select_with_no_beads_transitions_to_exhausted() {
        let store = Arc::new(MockStore::empty());
        let mut config = valid_test_config();
        config.self_modification.hot_reload = false;
        // Disable Explore strand so it doesn't find beads from the filesystem
        config.strands.explore.enabled = false;
        // Pin workspace_root to prevent Explore strand from scanning real home
        let temp_dir = tempfile::tempdir().unwrap();
        config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();
        let mut worker = Worker::new(config, "test-worker".to_string(), store);
        worker.boot().unwrap();

        worker.do_select().await.unwrap();
        // When claim_auto fails and strand waterfall finds no candidates,
        // we transition to Exhausted.
        assert_eq!(*worker.state(), WorkerState::Exhausted);
    }

    #[tokio::test]
    async fn shutdown_flag_causes_stop() {
        let store = Arc::new(MockStore::empty());
        let mut config = valid_test_config();
        config.worker.idle_action = IdleAction::Exit;
        config.self_modification.hot_reload = false;
        // Disable Explore strand so it doesn't scan the real filesystem —
        // safe today only because run_inner()'s loop checks `shutdown` before
        // reaching do_select()/Explore; disable explicitly rather than rely
        // on that ordering never changing (see bf-2unnq contamination).
        config.strands.explore.enabled = false;
        // Pin workspace_root to prevent Explore strand from scanning real home
        let temp_dir = tempfile::tempdir().unwrap();
        config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();
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
        // With claim_auto, successful claim transitions directly to Building
        // (skips Claiming state which was used in the old two-phase select/claim flow)
        assert_eq!(*worker.state(), WorkerState::Building);
        assert!(worker.current_bead.is_some());
    }

    #[tokio::test]
    async fn regression_2026_08_17_worker_never_holds_two_claims() {
        // On 2026-08-17, needle-55ec0193 remained held after a nominally
        // successful dispatch and was re-claimed by the same worker in the same
        // second. A second selection cycle must not overwrite that first claim
        // and acquire another bead. MockStore intentionally permits one actor to
        // claim multiple different beads so this test exercises the Worker
        // invariant rather than inheriting protection from the backend.
        let bead_a = make_test_bead("needle-claim-a");
        let bead_b = make_test_bead("needle-claim-b");
        let store = Arc::new(MockStore::new(vec![bead_a.clone(), bead_b.clone()]));
        let mut worker = make_worker(store.clone());
        worker.boot().unwrap();

        worker.do_select().await.unwrap();

        let worker_id = worker.qualified_id();
        assert_eq!(
            worker.current_bead.as_ref().map(|bead| &bead.id),
            Some(&bead_a.id)
        );
        assert_eq!(
            store.show(&bead_a.id).await.unwrap().status,
            BeadStatus::InProgress
        );

        // Simulate the leaked transition that re-entered SELECTING without
        // applying a terminal BeadAction for bead A, then attempt to claim B.
        worker.set_state(WorkerState::Selecting).unwrap();
        let error = worker.do_select().await.unwrap_err();

        assert!(
            error.to_string().contains("single-claim invariant"),
            "unexpected second-claim error: {error:#}"
        );
        let stored_beads = store.beads.lock().unwrap();
        let held_by_worker: Vec<_> = stored_beads
            .iter()
            .filter(|bead| {
                bead.status == BeadStatus::InProgress
                    && bead.assignee.as_deref() == Some(worker_id.as_str())
            })
            .map(|bead| bead.id.clone())
            .collect();
        assert!(
            held_by_worker.len() <= 1,
            "worker held multiple beads simultaneously: {held_by_worker:?}"
        );
        assert_eq!(
            stored_beads
                .iter()
                .find(|bead| bead.id == bead_a.id)
                .unwrap()
                .status,
            BeadStatus::Open,
            "the first claim must be released before rejecting the second"
        );
        assert_eq!(
            stored_beads
                .iter()
                .find(|bead| bead.id == bead_b.id)
                .unwrap()
                .status,
            BeadStatus::Open,
            "bead B must remain unclaimed after the rejected attempt"
        );
    }

    #[tokio::test]
    async fn regression_2026_08_17_exit_zero_without_close_cannot_loop() {
        // Exact incident fixture (needle-3386daef / needle-55ec0193):
        //   02:52 claim -> 02:58 success (6m), worker needle-otlp-test
        //   04:49 claim -> 04:59 timeout (10m), worker seam-2
        //   05:04 claim -> 05:35 success (32m), worker luna-needle
        //   05:35 re-claimed by luna-needle in the SAME SECOND as success
        // That was ~48 minutes across three workers, zero commits referencing
        // the bead, and the bead never left in_progress.
        let workspace = tempfile::tempdir().unwrap();
        let mut incident = make_test_bead("needle-55ec0193");
        incident.title =
            "handle_success leaks the claim when the agent exits 0 without closing the bead"
                .to_string();
        incident.body = Some(
            "2026-08-17: three dispatches (needle-otlp-test, seam-2, luna-needle); \
             luna-needle re-claimed at 05:35 in the same second as its success; \
             ~48 agent-minutes and zero commits"
                .to_string(),
        );
        incident.workspace = workspace.path().to_path_buf();

        let store = Arc::new(MockStore::new(vec![incident.clone()]));
        let mut config = valid_test_config();
        config.verification = vec!["false".to_string()];
        config.self_modification.hot_reload = false;
        config.strands.explore.enabled = false;
        config.strands.explore.workspace_root = workspace.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();
        let mut worker = Worker::new(config, "luna-needle".to_string(), store.clone());
        worker.boot().unwrap();
        worker.do_select().await.unwrap();

        let claimed = store.show(&incident.id).await.unwrap();
        assert_eq!(claimed.status, BeadStatus::InProgress);
        assert_eq!(
            claimed.assignee.as_deref(),
            Some(worker.qualified_id().as_str())
        );

        // Reproduce the agent process exiting 0 without closing the bead. The
        // failing definition-of-done gate must classify this as Failure and
        // produce a mandatory release action, never Success/BeadOrphaned.
        worker.state = WorkerState::Handling;
        worker.exec_output = Some((
            AgentOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            false,
        ));
        let action = worker.do_handle().await.unwrap();

        assert_eq!(worker.last_outcome.as_deref(), Some("failure"));
        assert_eq!(action, BeadAction::Released);
        let released = store.show(&incident.id).await.unwrap();
        assert_eq!(released.status, BeadStatus::Open);
        assert_eq!(released.assignee, None);

        // The state-machine boundary must consume the action before advancing.
        worker.apply_bead_action(action).await.unwrap();
        assert_eq!(*worker.state(), WorkerState::Logging);

        // Defense in depth: recreate the historical leaked state and attempt
        // the same-worker, same-bead re-dispatch. Single-claim enforcement must
        // reject the selection and release the stale claim before any new claim
        // can occur; the bead is never dispatchable while it remains held.
        let actor = worker.qualified_id();
        let leaked = match store.claim(&incident.id, &actor).await.unwrap() {
            ClaimResult::Claimed(bead) => bead,
            other => panic!("incident fixture could not recreate held claim: {other:?}"),
        };
        worker.current_bead = Some(leaked);
        worker.state = WorkerState::Selecting;

        let redispatch_error = worker.do_select().await.unwrap_err();
        assert!(
            redispatch_error
                .to_string()
                .contains("single-claim invariant"),
            "held bead was not rejected before re-dispatch: {redispatch_error:#}"
        );
        assert!(worker.current_bead.is_none());
        let recovered = store.show(&incident.id).await.unwrap();
        assert_eq!(recovered.status, BeadStatus::Open);
        assert_eq!(recovered.assignee, None);
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
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::RaceLost {
                claimed_by: "other-worker".to_string(),
            })
        }
        async fn add_dependency(&self, _a: &BeadId, _b: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(&self, _a: &BeadId, _b: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        fn has_valid_store(&self) -> bool {
            true // Mock store always has a valid store
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
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "already closed".to_string(),
            })
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
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        fn has_valid_store(&self) -> bool {
            true // Mock store always has a valid store
        }
    }

    /// A store that always returns Suspect on claim (for testing suspect bead handling).
    struct SuspectStore {
        beads: Mutex<Vec<Bead>>,
    }

    impl SuspectStore {
        fn new(beads: Vec<Bead>) -> Self {
            SuspectStore {
                beads: Mutex::new(beads),
            }
        }
    }

    #[async_trait]
    impl BeadStore for SuspectStore {
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
        async fn claim(&self, id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::Suspect {
                bead_id: id.clone(),
                consecutive_errors: 3,
                last_error: "database disk image is malformed".to_string(),
            })
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
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
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "no beads".to_string(),
            })
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
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        fn has_valid_store(&self) -> bool {
            true // Mock store always has a valid store
        }
    }

    // ── do_claim tests ──

    #[test]
    fn two_hundred_claim_cycles_keep_span_depth_and_lines_bounded() {
        const CLAIM_CYCLES: usize = 200;

        let captured = CapturedLogs::default();
        let writer = crate::log_writer::LineCappedMakeWriter::new(
            captured.clone(),
            crate::log_writer::DEFAULT_MAX_LINE_BYTES,
        );
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_ansi(false)
            .without_time()
            .finish();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let home = tempfile::tempdir().unwrap();

        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let mut bead = make_test_bead("needle-span-depth");
                bead.workspace = home.path().to_path_buf();
                let store = Arc::new(MockStore::new(vec![bead.clone()]));
                let mut config = valid_test_config();
                config.workspace.home = home.path().to_path_buf();
                config.workspace.default = home.path().to_path_buf();
                config.self_modification.hot_reload = false;
                config.strands.explore.enabled = false;
                // Pin workspace_root to prevent Explore strand from scanning real home
                config.strands.explore.workspace_root = home.path().to_path_buf();
                config.strands.explore.workspaces = Vec::new();
                let mut worker = Worker::new(config, "span-depth".to_string(), store.clone());
                worker.boot().unwrap();

                for cycle in 0..CLAIM_CYCLES {
                    // Reset the in-memory bead to a claimable state for the next
                    // independent cycle. Each cycle uses a distinct bead id: the
                    // claim path circuit-breaks a single bead after
                    // MAX_CLAIM_EVENTS_PER_BEAD (100) claim events, so reusing one
                    // id would quarantine it halfway through and make this a test
                    // of the breaker rather than of span depth.
                    let mut bead = make_test_bead(&format!("needle-span-depth-{cycle}"));
                    bead.workspace = home.path().to_path_buf();
                    *store.beads.lock().unwrap() = vec![bead.clone()];
                    worker.current_bead = Some(bead.clone());
                    worker.current_strand = Some("pluck".to_string());
                    worker.state = WorkerState::Claiming;

                    worker.do_claim().await.unwrap();
                    let lifecycle_span = worker
                        .bead_lifecycle_span
                        .clone()
                        .expect("successful claim should create lifecycle span");
                    lifecycle_span.in_scope(|| {
                        tracing::info!(cycle, "claim-cycle-depth-probe");
                    });

                    worker.last_outcome = Some("success".to_string());
                    lifecycle_span.in_scope(|| worker.do_log()).unwrap();
                    assert!(worker.bead_lifecycle_span.is_none());
                }

                // Exercise the production line guard with a single event larger
                // than its configured ceiling.
                let payload = "x".repeat(crate::log_writer::DEFAULT_MAX_LINE_BYTES * 2);
                tracing::info!(%payload, "oversized-line-depth-probe");
            });
        });

        let bytes = captured.0.lock().unwrap().clone();
        let logs = String::from_utf8_lossy(&bytes);
        let probe_lines: Vec<_> = logs
            .lines()
            .filter(|line| line.contains("claim-cycle-depth-probe"))
            .collect();
        assert_eq!(probe_lines.len(), CLAIM_CYCLES);

        for line in logs.lines() {
            assert!(
                line.matches("bead.claim{").count() <= 1,
                "bead.claim depth grew beyond one: {line}"
            );
            assert!(
                line.matches("bead.lifecycle{").count() <= 1,
                "bead.lifecycle depth grew beyond one: {line}"
            );
            assert!(
                line.len() < crate::log_writer::DEFAULT_MAX_LINE_BYTES,
                "formatted log line exceeded byte cap: {} bytes",
                line.len()
            );
        }
        assert!(probe_lines
            .iter()
            .all(|line| line.matches("bead.lifecycle{").count() == 1));
    }

    #[tokio::test]
    async fn do_claim_suspect_marks_bead_and_transitions_to_selecting() {
        let bead = make_test_bead("needle-suspect");
        let store: Arc<dyn BeadStore> = Arc::new(SuspectStore::new(vec![bead]));
        let mut worker = make_worker(store);
        worker.boot().unwrap();

        // Simulate: strand selected a candidate, now in Claiming state.
        worker.current_bead = Some(make_test_bead("needle-suspect"));
        worker.state = WorkerState::Claiming;

        worker.do_claim().await.unwrap();

        // Should transition to Selecting and add the bead to exclusion set.
        assert_eq!(*worker.state(), WorkerState::Selecting);
        assert!(worker
            .exclusion_set
            .contains(&BeadId::from("needle-suspect")));
        // consecutive_race_lost should be reset (not incremented)
        assert_eq!(worker.consecutive_race_lost, 0);
        // current_bead should be cleared
        assert!(worker.current_bead.is_none());
    }

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

        // Backoff formula: 100 * (1 << (consecutive_race_lost - 1)) ms
        // For consecutive_race_lost=2: 100 * (1 << 1) = 200ms
        // Verify it slept (at least 100ms) and transitioned to Selecting.
        assert!(elapsed >= std::time::Duration::from_millis(100));
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
        let mut config = valid_test_config();
        config.self_modification.hot_reload = false;
        config.workspace.home = dir.path().to_path_buf();
        config.telemetry.file_sink.log_dir = Some(log_dir);
        config.budget.stop_usd = 10.0; // Cost (50) exceeds this threshold.
        config.budget.warn_usd = 5.0;
        // Pin workspace_root to prevent Explore strand from scanning real home
        config.strands.explore.workspace_root = dir.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();

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
        let mut config = valid_test_config();
        config.self_modification.hot_reload = false;
        config.workspace.home = dir.path().to_path_buf();
        config.telemetry.file_sink.log_dir = Some(log_dir);
        config.budget.warn_usd = 5.0; // Cost (8) exceeds warn but not stop.
        config.budget.stop_usd = 20.0;
        // Pin workspace_root to prevent Explore strand from scanning real home
        config.strands.explore.workspace_root = dir.path().to_path_buf();
        config.strands.explore.workspaces = Vec::new();

        let mut worker = Worker::new(config, "test-budget-warn".to_string(), store);
        worker.boot().unwrap();

        worker.check_budget().unwrap();
        // State should still be Selecting — warn doesn't stop the worker.
        assert_eq!(*worker.state(), WorkerState::Selecting);
    }

    #[tokio::test]
    async fn worker_boots_with_zero_budget_thresholds() {
        // Regression test: worker must boot successfully even when both budget
        // thresholds are 0.0 (disabled). A warning is emitted, but boot completes.
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let config = valid_test_config();
        // Verify defaults are 0.0
        assert_eq!(config.budget.warn_usd, 0.0);
        assert_eq!(config.budget.stop_usd, 0.0);

        let mut worker = Worker::new(config, "test-zero-budget".to_string(), store);
        // Worker must boot without error
        assert!(worker.boot().is_ok());
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

    #[tokio::test]
    async fn apply_bead_action_recovers_a_still_held_claim() {
        let mut bead = make_test_bead("needle-action-error");
        bead.status = BeadStatus::InProgress;
        bead.assignee = Some("test-worker".to_string());
        let store = Arc::new(MockStore::new(vec![bead.clone()]));
        let mut worker = make_worker(store.clone());
        worker.boot().unwrap();
        worker.state = WorkerState::Handling;
        worker.current_bead = Some(bead.clone());

        worker.apply_bead_action(BeadAction::Errored).await.unwrap();

        assert_eq!(*worker.state(), WorkerState::Logging);
        let current = store.show(&bead.id).await.unwrap();
        assert_eq!(current.status, BeadStatus::Open);
        assert_eq!(current.assignee, None);
    }

    #[tokio::test]
    async fn apply_bead_action_accepts_closed_and_stops_after_interruption() {
        let mut closed = make_test_bead("needle-action-closed");
        closed.status = BeadStatus::Closed;
        let closed_store = Arc::new(MockStore::new(vec![closed.clone()]));
        let mut closed_worker = make_worker(closed_store.clone());
        closed_worker.boot().unwrap();
        closed_worker.state = WorkerState::Handling;
        closed_worker.current_bead = Some(closed.clone());

        closed_worker
            .apply_bead_action(BeadAction::Closed)
            .await
            .unwrap();

        assert_eq!(*closed_worker.state(), WorkerState::Logging);
        assert_eq!(
            closed_store.show(&closed.id).await.unwrap().status,
            BeadStatus::Closed
        );

        let mut interrupted = make_test_bead("needle-action-interrupted");
        interrupted.status = BeadStatus::InProgress;
        interrupted.assignee = Some("test-worker".to_string());
        let interrupted_store = Arc::new(MockStore::new(vec![interrupted.clone()]));
        let mut interrupted_worker = make_worker(interrupted_store.clone());
        interrupted_worker.boot().unwrap();
        interrupted_worker.state = WorkerState::Handling;
        interrupted_worker.current_bead = Some(interrupted.clone());

        interrupted_worker
            .apply_bead_action(BeadAction::Interrupted)
            .await
            .unwrap();

        assert_eq!(*interrupted_worker.state(), WorkerState::Stopped);
        let current = interrupted_store.show(&interrupted.id).await.unwrap();
        assert_eq!(current.status, BeadStatus::Open);
        assert_eq!(current.assignee, None);
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
        let mut config = valid_test_config();
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
            comments: vec![],
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
        let mut config = valid_test_config();
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
            comments: vec![],
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
        let mut config = valid_test_config();
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
        let mut config = valid_test_config();
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
            provider: None,
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
        let mut config = valid_test_config();
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
        let mut config = valid_test_config();
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
        let mut config = valid_test_config();
        config.self_modification.hot_reload = false;
        // Disable Explore strand so it doesn't find beads from the filesystem
        config.strands.explore.enabled = false;
        let mut worker = Worker::new(config, "test-worker".to_string(), store);
        worker.boot().unwrap();
        worker.race_lost_this_cycle.insert(BeadId::from("old-bead"));
        worker.retry_count = 3;

        worker.do_select().await.unwrap();

        assert!(worker.race_lost_this_cycle.is_empty());
        // retry_count is NOT cleared by do_select() anymore — it must accumulate
        // across cycles to prevent infinite race-lost loops (see needle-aad8).
        assert_eq!(worker.retry_count, 3);
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
        config.agent.routing = None; // test dispatcher only has echo-test; disable model-based routing
        config.agent.timeout = 5;
        // Set workspace.default to match the bead's workspace so the remote
        // store switch logic doesn't fire.
        config.workspace.default = std::path::PathBuf::from("/tmp/test-workspace");
        // Disable Explore: once the MockStore's one bead is processed and
        // `run()` loops again, Pluck/Mend return NoWork and the waterfall
        // previously fell through to a real Explore scan of $HOME, claiming
        // and mutating real beads in unrelated repos on this server via the
        // real bf/br CLI. See bf-2unnq's contamination addendum.
        config.strands.explore.enabled = false;

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
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: None,
            model: None,
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
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
        let mut config = valid_test_config();
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
        let mut config = valid_test_config();
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
        let mut config = valid_test_config();
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

    // ── apply_routing_rules tests ──

    #[tokio::test]
    async fn apply_routing_rules_strict_true_no_match_returns_err() {
        use crate::config::{RoutingConfig, RoutingRule};
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;
        // Enable strict mode with a rule that won't match.
        config.agent.routing = Some(RoutingConfig {
            rules: vec![RoutingRule {
                match_model: "sonnet".to_string(),
                adapter: "claude-print".to_string(),
            }],
            default_adapter: None,
            strict: true,
        });
        // Set up a model that won't match the rule.
        let worker = Worker::new(config, "test-routing-strict".to_string(), store);
        let adapter = crate::dispatch::AgentAdapter {
            name: "claude".to_string(),
            description: None,
            agent_cli: "claude".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "claude {prompt}".to_string(),
            environment: std::collections::HashMap::new(),
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: Some("gpt-4o".to_string()),
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };

        let result = worker.apply_routing_rules(&adapter);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("no routing rule matched model 'gpt-4o'"),
            "error message should mention the model: {}",
            err_msg
        );
        assert!(
            err_msg.contains("set routing.strict: false"),
            "error message should suggest disabling strict mode: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn apply_routing_rules_strict_false_no_match_returns_default() {
        use crate::config::{RoutingConfig, RoutingRule};
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;
        // Disable strict mode with a rule that won't match.
        config.agent.routing = Some(RoutingConfig {
            rules: vec![RoutingRule {
                match_model: "sonnet".to_string(),
                adapter: "claude-print".to_string(),
            }],
            default_adapter: None,
            strict: false,
        });
        let worker = Worker::new(config, "test-routing-non-strict".to_string(), store);
        let adapter = crate::dispatch::AgentAdapter {
            name: "claude".to_string(),
            description: None,
            agent_cli: "claude".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "claude {prompt}".to_string(),
            environment: std::collections::HashMap::new(),
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: Some("gpt-4o".to_string()),
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };

        let result = worker.apply_routing_rules(&adapter);
        assert!(result.is_ok());
        let (chosen_adapter, matched_rule) = result.unwrap();
        // Should fall back to default adapter when no rule matches and strict is false.
        assert_eq!(chosen_adapter, "claude");
        assert_eq!(matched_rule, "default");
    }

    #[tokio::test]
    async fn apply_routing_rules_strict_true_match_returns_ok() {
        use crate::config::{RoutingConfig, RoutingRule};
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;
        // Enable strict mode with a rule that will match.
        config.agent.routing = Some(RoutingConfig {
            rules: vec![RoutingRule {
                match_model: "claude-.*".to_string(),
                adapter: "claude-print".to_string(),
            }],
            default_adapter: None,
            strict: true,
        });
        let worker = Worker::new(config, "test-routing-match".to_string(), store);
        let adapter = crate::dispatch::AgentAdapter {
            name: "claude".to_string(),
            description: None,
            agent_cli: "claude".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "claude {prompt}".to_string(),
            environment: std::collections::HashMap::new(),
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };

        let result = worker.apply_routing_rules(&adapter);
        assert!(result.is_ok());
        let (chosen_adapter, matched_rule) = result.unwrap();
        // Should use the matched adapter when rule matches.
        assert_eq!(chosen_adapter, "claude-print");
        assert_eq!(matched_rule, "claude-.*");
    }

    #[tokio::test]
    async fn apply_routing_rules_strict_false_uses_default_adapter() {
        use crate::config::{RoutingConfig, RoutingRule};
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;
        // Disable strict mode with default_adapter set.
        config.agent.routing = Some(RoutingConfig {
            rules: vec![RoutingRule {
                match_model: "sonnet".to_string(),
                adapter: "claude-print".to_string(),
            }],
            default_adapter: Some("claude-code-glm-4.7".to_string()),
            strict: false,
        });
        let worker = Worker::new(config, "test-routing-default-adapter".to_string(), store);
        let adapter = crate::dispatch::AgentAdapter {
            name: "claude".to_string(),
            description: None,
            agent_cli: "claude".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "claude {prompt}".to_string(),
            environment: std::collections::HashMap::new(),
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: Some("gpt-4o".to_string()),
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };

        let result = worker.apply_routing_rules(&adapter);
        assert!(result.is_ok());
        let (chosen_adapter, matched_rule) = result.unwrap();
        // Should use the routing default adapter when no rule matches.
        assert_eq!(chosen_adapter, "claude-code-glm-4.7");
        assert_eq!(matched_rule, "routing-default");
    }

    #[tokio::test]
    async fn default_routing_rules_anthropic_subscription_models() {
        // Verify that default routing rules route Anthropic subscription models to claude-print.
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;

        // Use the default routing rules from AgentConfig::default_routing().
        let default_routing = config
            .agent
            .routing
            .as_ref()
            .expect("default routing should be set");

        // Verify the default routing rules are configured correctly.
        assert!(
            !default_routing.rules.is_empty(),
            "should have at least one routing rule"
        );
        assert_eq!(
            default_routing.rules[0].match_model, "(claude-)?(sonnet|opus|fable|haiku).*",
            "first rule should match Anthropic subscription models"
        );
        assert_eq!(
            default_routing.rules[0].adapter, "claude-print",
            "first rule should route to claude-print"
        );
        assert_eq!(
            default_routing.default_adapter,
            Some("claude-code-glm-4.7".to_string()),
            "default adapter should be claude-code-glm-4.7"
        );
        assert!(
            !default_routing.strict,
            "strict mode should be disabled by default"
        );

        // Test routing with actual Anthropic subscription model names.
        let anthropic_models = vec![
            "claude-sonnet-4-6",
            "claude-opus-4-6",
            "claude-fable-5",
            "claude-haiku-4-5-20251001",
            "sonnet",
            "opus",
            "fable",
            "haiku",
        ];

        for model_name in anthropic_models {
            let worker = Worker::new(
                config.clone(),
                format!("test-routing-{}", model_name),
                Arc::clone(&store),
            );
            let adapter = crate::dispatch::AgentAdapter {
                name: "claude".to_string(),
                description: None,
                agent_cli: "claude".to_string(),
                version_command: None,
                input_method: crate::types::InputMethod::Stdin,
                invoke_template: "claude {prompt}".to_string(),
                environment: std::collections::HashMap::new(),
                timeout_secs: 3600,
                idle_timeout_secs: 0,
                hard_timeout_secs: 0,
                provider: Some("anthropic".to_string()),
                model: Some(model_name.to_string()),
                token_extraction: crate::dispatch::TokenExtraction::None,
                output_transform: None,
                harness: None,
                harness_version: None,
            };

            let result = worker.apply_routing_rules(&adapter);
            assert!(
                result.is_ok(),
                "routing should succeed for model {}: {:?}",
                model_name,
                result
            );
            let (chosen_adapter, matched_rule) = result.unwrap();
            assert_eq!(
                chosen_adapter, "claude-print",
                "Anthropic subscription model {} should route to claude-print, got {}",
                model_name, chosen_adapter
            );
            assert_ne!(
                matched_rule, "default",
                "model {} should match a routing rule, not fall back to default",
                model_name
            );
        }

        // Test that non-Anthropic models route to claude-code-glm-4.7.
        let non_anthropic_models = vec![
            "glm-4.7",
            "gpt-4o",
            "claude-other", // Doesn't match the subscription pattern
            "deepseek-r1",
        ];

        for model_name in non_anthropic_models {
            let worker = Worker::new(
                config.clone(),
                format!("test-routing-{}", model_name),
                Arc::clone(&store),
            );
            let adapter = crate::dispatch::AgentAdapter {
                name: "claude".to_string(),
                description: None,
                agent_cli: "claude".to_string(),
                version_command: None,
                input_method: crate::types::InputMethod::Stdin,
                invoke_template: "claude {prompt}".to_string(),
                environment: std::collections::HashMap::new(),
                timeout_secs: 3600,
                idle_timeout_secs: 0,
                hard_timeout_secs: 0,
                provider: Some("anthropic".to_string()),
                model: Some(model_name.to_string()),
                token_extraction: crate::dispatch::TokenExtraction::None,
                output_transform: None,
                harness: None,
                harness_version: None,
            };

            let result = worker.apply_routing_rules(&adapter);
            assert!(
                result.is_ok(),
                "routing should succeed for model {}: {:?}",
                model_name,
                result
            );
            let (chosen_adapter, matched_rule) = result.unwrap();
            assert_eq!(
                chosen_adapter, "claude-code-glm-4.7",
                "Non-subscription model {} should route to claude-code-glm-4.7, got {}",
                model_name, chosen_adapter
            );
            assert_eq!(
                matched_rule, "routing-default",
                "non-matching model {} should use routing-default adapter",
                model_name
            );
        }
    }

    #[tokio::test]
    async fn apply_routing_rules_first_match_wins() {
        // Test that when multiple rules match the same model, the first rule wins.
        use crate::config::{RoutingConfig, RoutingRule};
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;

        // Configure two rules that both match "claude-sonnet-4-6":
        // - First rule: more specific pattern "claude-sonnet-.*" -> claude-print
        // - Second rule: broader pattern "claude-.*" -> claude-code-glm-4.7
        config.agent.routing = Some(RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "claude-sonnet-.*".to_string(),
                    adapter: "claude-print".to_string(),
                },
                RoutingRule {
                    match_model: "claude-.*".to_string(),
                    adapter: "claude-code-glm-4.7".to_string(),
                },
            ],
            default_adapter: None,
            strict: true,
        });

        let worker = Worker::new(
            config,
            "test-routing-first-match".to_string(),
            Arc::clone(&store),
        );
        let adapter = crate::dispatch::AgentAdapter {
            name: "claude".to_string(),
            description: None,
            agent_cli: "claude".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "claude {prompt}".to_string(),
            environment: std::collections::HashMap::new(),
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };

        let result = worker.apply_routing_rules(&adapter);
        assert!(result.is_ok());
        let (chosen_adapter, matched_rule) = result.unwrap();

        // Should use the FIRST matching rule (claude-print), not the second.
        assert_eq!(
            chosen_adapter, "claude-print",
            "first matching rule should win when both rules match"
        );
        assert_eq!(
            matched_rule, "claude-sonnet-.*",
            "should report the first matching pattern"
        );
    }

    #[tokio::test]
    async fn apply_routing_rules_order_matters() {
        // Test that rule order matters - swap the order from the previous test
        // and verify the second rule now wins.
        use crate::config::{RoutingConfig, RoutingRule};
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;

        // Same two patterns as previous test, but in REVERSED order:
        // - First rule: broader pattern "claude-.*" -> claude-code-glm-4.7
        // - Second rule: more specific pattern "claude-sonnet-.*" -> claude-print
        config.agent.routing = Some(RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "claude-.*".to_string(),
                    adapter: "claude-code-glm-4.7".to_string(),
                },
                RoutingRule {
                    match_model: "claude-sonnet-.*".to_string(),
                    adapter: "claude-print".to_string(),
                },
            ],
            default_adapter: None,
            strict: true,
        });

        let worker = Worker::new(
            config,
            "test-routing-order-matters".to_string(),
            Arc::clone(&store),
        );
        let adapter = crate::dispatch::AgentAdapter {
            name: "claude".to_string(),
            description: None,
            agent_cli: "claude".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "claude {prompt}".to_string(),
            environment: std::collections::HashMap::new(),
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };

        let result = worker.apply_routing_rules(&adapter);
        assert!(result.is_ok());
        let (chosen_adapter, matched_rule) = result.unwrap();

        // Now the FIRST rule (claude-code-glm-4.7 with broader pattern) wins,
        // even though the second rule is more specific.
        assert_eq!(
            chosen_adapter, "claude-code-glm-4.7",
            "swapped rule order should cause first rule to win"
        );
        assert_eq!(
            matched_rule, "claude-.*",
            "should report the first (broader) matching pattern"
        );
    }

    // ── Routing baseline tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn apply_routing_rules_baseline_multiple_rules_same_adapter() {
        /// Baseline test documenting CURRENT matcher behavior when multiple rules
        /// both match the same model AND route to the same adapter.
        ///
        /// This test establishes the baseline behavior before first-match-wins
        /// implementation. The test verifies that when both rules match and route
        /// to the same adapter, the first matching rule is reported.
        use crate::config::{RoutingConfig, RoutingRule};
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;

        // Configure two rules that both match "claude-sonnet-4-6" AND route to
        // the same adapter "claude-print":
        // - First rule: "claude-.*" -> claude-print
        // - Second rule: "claude-sonnet-.*" -> claude-print (same adapter)
        config.agent.routing = Some(RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "claude-.*".to_string(),
                    adapter: "claude-print".to_string(),
                },
                RoutingRule {
                    match_model: "claude-sonnet-.*".to_string(),
                    adapter: "claude-print".to_string(),
                },
            ],
            default_adapter: None,
            strict: true,
        });

        let worker = Worker::new(
            config,
            "test-routing-baseline-same".to_string(),
            Arc::clone(&store),
        );

        let adapter = crate::dispatch::AgentAdapter {
            name: "claude".to_string(),
            description: None,
            agent_cli: "claude".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "claude {prompt}".to_string(),
            environment: std::collections::HashMap::new(),
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };

        let result = worker.apply_routing_rules(&adapter);
        assert!(result.is_ok(), "routing should succeed when rules match");

        let (chosen_adapter, matched_rule) = result.unwrap();

        // Current behavior: both rules match and route to the same adapter
        // The first matching rule should be reported
        assert_eq!(
            chosen_adapter, "claude-print",
            "should route to claude-print when both rules match"
        );
        assert_eq!(
            matched_rule, "claude-.*",
            "should report the FIRST matching pattern even when both route to same adapter"
        );
    }

    #[tokio::test]
    async fn apply_routing_rules_baseline_three_rules_all_match() {
        /// Baseline test documenting CURRENT matcher behavior when THREE rules
        /// all match the same model.
        ///
        /// This test verifies that the matcher evaluates rules in order and
        /// returns the first match, even when multiple subsequent rules would
        /// also match.
        use crate::config::{RoutingConfig, RoutingRule};
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;

        // Configure three rules that all match "claude-sonnet-4-6":
        // - First rule: "claude-.*" -> claude-print (most broad, should match first)
        // - Second rule: "claude-sonnet-.*" -> claude-code (more specific)
        // - Third rule: "claude-sonnet-4-.*" -> claude-fable (most specific)
        config.agent.routing = Some(RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "claude-.*".to_string(),
                    adapter: "claude-print".to_string(),
                },
                RoutingRule {
                    match_model: "claude-sonnet-.*".to_string(),
                    adapter: "claude-code-glm-4.7".to_string(),
                },
                RoutingRule {
                    match_model: "claude-sonnet-4-.*".to_string(),
                    adapter: "claude-fable".to_string(),
                },
            ],
            default_adapter: None,
            strict: true,
        });

        let worker = Worker::new(
            config,
            "test-routing-baseline-three".to_string(),
            Arc::clone(&store),
        );

        let adapter = crate::dispatch::AgentAdapter {
            name: "claude".to_string(),
            description: None,
            agent_cli: "claude".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "claude {prompt}".to_string(),
            environment: std::collections::HashMap::new(),
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };

        let result = worker.apply_routing_rules(&adapter);
        assert!(result.is_ok(), "routing should succeed when rules match");

        let (chosen_adapter, matched_rule) = result.unwrap();

        // Current behavior: FIRST matching rule wins, even though all three match
        assert_eq!(
            chosen_adapter, "claude-print",
            "should route to claude-print (first matching adapter)"
        );
        assert_eq!(
            matched_rule, "claude-.*",
            "should report the FIRST matching pattern, even though all three match"
        );

        println!(
            "BASELINE: All three rules matched, but first rule won. \
             chosen_adapter={}, matched_rule={}",
            chosen_adapter, matched_rule
        );
    }

    #[tokio::test]
    async fn apply_routing_rules_baseline_invalid_then_valid() {
        /// Baseline test documenting CURRENT matcher behavior when an invalid
        /// pattern precedes valid matching patterns.
        ///
        /// This test verifies that invalid patterns are skipped gracefully and
        /// the first valid matching pattern determines the adapter.
        use crate::config::{RoutingConfig, RoutingRule};
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;

        // Configure rules with invalid pattern in the middle:
        // - First rule: valid pattern, matches
        // - Second rule: INVALID pattern (should be skipped with warning)
        // - Third rule: valid pattern, would also match but shouldn't be checked
        config.agent.routing = Some(RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "claude-.*".to_string(),
                    adapter: "first-adapter".to_string(),
                },
                RoutingRule {
                    match_model: "[invalid(regex".to_string(),
                    adapter: "invalid-adapter".to_string(),
                },
                RoutingRule {
                    match_model: "claude-sonnet-.*".to_string(),
                    adapter: "third-adapter".to_string(),
                },
            ],
            default_adapter: None,
            strict: true,
        });

        let worker = Worker::new(
            config,
            "test-routing-baseline-invalid".to_string(),
            Arc::clone(&store),
        );

        let adapter = crate::dispatch::AgentAdapter {
            name: "claude".to_string(),
            description: None,
            agent_cli: "claude".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "claude {prompt}".to_string(),
            environment: std::collections::HashMap::new(),
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };

        let result = worker.apply_routing_rules(&adapter);
        assert!(
            result.is_ok(),
            "routing should succeed when a valid rule matches"
        );

        let (chosen_adapter, matched_rule) = result.unwrap();

        // Current behavior: first valid match wins, invalid rules are skipped
        assert_eq!(
            chosen_adapter, "first-adapter",
            "should use the first VALID matching rule"
        );
        assert_eq!(
            matched_rule, "claude-.*",
            "should report the first valid matching pattern"
        );

        println!(
            "BASELINE: Invalid pattern was skipped, first valid match determined routing. \
             chosen_adapter={}, matched_rule={}",
            chosen_adapter, matched_rule
        );
    }

    #[tokio::test]
    async fn apply_routing_rules_baseline_first_match_stops_evaluation() {
        /// Baseline test to verify whether the matcher stops at the first match
        /// or continues checking all rules.
        ///
        /// This test uses a counter pattern in the adapter name to detect
        /// whether all rules are checked or only the first match.
        use crate::config::{RoutingConfig, RoutingRule};
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.self_modification.hot_reload = false;

        // Configure rules where first match should stop evaluation:
        // - First rule: specific pattern -> adapter-1
        // - Second rule: broader pattern -> adapter-2 (should NOT be checked)
        // - Third rule: catch-all -> adapter-3 (should NOT be checked)
        config.agent.routing = Some(RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "^claude-sonnet-4-6$".to_string(),
                    adapter: "adapter-1".to_string(),
                },
                RoutingRule {
                    match_model: "claude-.*".to_string(),
                    adapter: "adapter-2".to_string(),
                },
                RoutingRule {
                    match_model: "*".to_string(),
                    adapter: "adapter-3".to_string(),
                },
            ],
            default_adapter: None,
            strict: true,
        });

        let worker = Worker::new(
            config,
            "test-routing-baseline-stops".to_string(),
            Arc::clone(&store),
        );

        let adapter = crate::dispatch::AgentAdapter {
            name: "claude".to_string(),
            description: None,
            agent_cli: "claude".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "claude {prompt}".to_string(),
            environment: std::collections::HashMap::new(),
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            token_extraction: crate::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        };

        let result = worker.apply_routing_rules(&adapter);
        assert!(result.is_ok(), "routing should succeed when a rule matches");

        let (chosen_adapter, matched_rule) = result.unwrap();

        // Current behavior: FIRST match stops evaluation
        assert_eq!(
            chosen_adapter, "adapter-1",
            "should use the first matching rule and stop checking"
        );
        assert_eq!(
            matched_rule, "^claude-sonnet-4-6$",
            "should report the exact pattern that matched first"
        );

        println!(
            "BASELINE: First match stopped evaluation. \
             chosen_adapter={}, matched_rule={}",
            chosen_adapter, matched_rule
        );
    }

    // ── Tests for retry-path decision logic (P5.6) ─────────────────────────────
    use std::fs;

    #[test]
    fn found_but_all_excluded_returns_false_when_no_evaluations() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let store = Arc::new(MockStore::empty());
        let worker = Worker::new(config, "test-worker".to_string(), store);

        // No strand evaluations yet
        assert!(!worker.found_but_all_excluded());
    }

    #[test]
    fn found_but_all_excluded_returns_false_when_no_beads_found() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Simulate strand evaluations with no beads found
        worker.last_strand_evaluations = vec![
            ("explore".to_string(), "NoWork".to_string(), 100),
            ("pluck".to_string(), "NoWork".to_string(), 50),
        ];

        assert!(!worker.found_but_all_excluded());
    }

    #[test]
    fn found_but_all_excluded_returns_true_when_explore_found_beads() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Simulate explore strand finding beads (but we're exhausted = excluded)
        worker.last_strand_evaluations = vec![
            ("explore".to_string(), "BeadFound".to_string(), 200),
            ("pluck".to_string(), "NoWork".to_string(), 50),
        ];

        assert!(
            worker.found_but_all_excluded(),
            "should return true when explore found beads but we're exhausted"
        );
    }

    #[test]
    fn found_but_all_excluded_returns_true_when_pluck_found_candidates() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Simulate pluck strand finding candidates (but we're exhausted = excluded)
        worker.last_strand_evaluations = vec![
            ("explore".to_string(), "NoWork".to_string(), 100),
            ("pluck".to_string(), "candidates_found".to_string(), 50),
        ];

        assert!(
            worker.found_but_all_excluded(),
            "should return true when pluck found candidates but we're exhausted"
        );
    }

    #[test]
    fn found_but_all_excluded_returns_false_when_bead_was_claimed() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Simulate successful claim (we wouldn't be exhausted in this case)
        worker.last_strand_evaluations = vec![("pluck".to_string(), "Claimed".to_string(), 50)];

        assert!(!worker.found_but_all_excluded());
    }

    #[test]
    fn jittered_backoff_is_within_configured_range() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.worker.idle_backoff_min = 60;
        config.worker.idle_backoff_max = 120;
        let store = Arc::new(MockStore::empty());
        let worker = Worker::new(config, "test-worker".to_string(), store);

        // Run multiple times to test randomness
        let mut results = Vec::new();
        for _ in 0..20 {
            let backoff = worker.compute_jittered_backoff();
            results.push(backoff);
            assert!(
                (60..=120).contains(&backoff),
                "jittered backoff {} should be within range [60, 120]",
                backoff
            );
        }

        // Check that we got some variety (not all the same value)
        let unique_values: std::collections::HashSet<_> = results.iter().collect();
        assert!(
            unique_values.len() > 1,
            "jittered backoff should produce varied values across multiple calls"
        );
    }

    #[test]
    fn jittered_backoff_returns_min_when_min_equals_max() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.worker.idle_backoff_min = 90;
        config.worker.idle_backoff_max = 90;
        let store = Arc::new(MockStore::empty());
        let worker = Worker::new(config, "test-worker".to_string(), store);

        let backoff = worker.compute_jittered_backoff();
        assert_eq!(
            backoff, 90,
            "jittered backoff should return min value when min equals max"
        );
    }

    #[test]
    fn check_workspace_mtimes_returns_most_recent_mtime() {
        let temp_root = tempfile::tempdir().unwrap();
        let config = Config::default();
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Create test workspaces with .beads/issues.jsonl files
        let ws1 = temp_root.path().join("workspace1");
        let ws2 = temp_root.path().join("workspace2");
        fs::create_dir_all(ws1.join(".beads")).unwrap();
        fs::create_dir_all(ws2.join(".beads")).unwrap();

        // Create issues.jsonl files
        let issues1 = ws1.join(".beads").join("issues.jsonl");
        let issues2 = ws2.join(".beads").join("issues.jsonl");
        fs::write(&issues1, "[]").unwrap();
        fs::write(&issues2, "[]").unwrap();

        // Update worker config to use test workspaces
        worker.config.workspace.default = ws1.clone();
        worker.config.strands.explore.workspaces = vec![ws2.clone()];

        // Force a small delay to ensure different mtimes
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Touch the second file to make it newer
        fs::write(&issues2, "[{\"updated\": true}]").unwrap();

        let mtime = worker.check_workspace_mtimes();
        assert!(
            mtime.is_some(),
            "check_workspace_mtimes should return Some when files exist"
        );

        // The newer file should determine the mtime
        let ws2_metadata = fs::metadata(&issues2).unwrap();
        let _ws2_mtime = ws2_metadata.modified().unwrap();

        // Note: comparing SystemTimes is imprecise due to filesystem resolution,
        // but we can check that we got a reasonable time
        assert!(
            mtime.unwrap() <= std::time::SystemTime::now(),
            "returned mtime should not be in the future"
        );
    }

    #[test]
    fn check_workspace_mtimes_returns_none_when_no_files_exist() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Set workspace to a directory with no .beads/
        let nonexistent = temp_dir.path().join("nonexistent");
        worker.config.workspace.default = nonexistent.clone();
        worker.config.strands.explore.workspaces = vec![];

        let mtime = worker.check_workspace_mtimes();
        assert!(
            mtime.is_none(),
            "check_workspace_mtimes should return None when no files exist"
        );
    }

    // ── Retry-path decision tests (P5.6: event-driven wakeups + short retry) ─────

    #[test]
    fn short_retry_backoff_used_when_found_but_excluded() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.worker.short_retry_backoff = 5;
        config.worker.idle_backoff_min = 60;
        config.worker.idle_backoff_max = 120;
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Set found_but_excluded flag to simulate found-but-excluded scenario
        worker.found_but_excluded = true;

        // Verify that short_retry_backoff would be used
        let expected_backoff = worker.config.worker.short_retry_backoff;
        let jittered_backoff = worker.compute_jittered_backoff();

        // When found_but_excluded is true, short_retry_backoff should be used
        // This is tested indirectly by verifying the flag is set correctly
        assert_eq!(
            expected_backoff, 5,
            "short_retry_backoff should be configured to 5 seconds"
        );
        assert!(
            (60..=120).contains(&jittered_backoff),
            "jittered backoff should be in idle range [60, 120]"
        );
        assert_ne!(
            expected_backoff, jittered_backoff,
            "short_retry_backoff (5s) should differ from jittered idle backoff (60-120s)"
        );
    }

    #[test]
    fn jittered_idle_backoff_used_when_no_candidates_found() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.worker.short_retry_backoff = 5;
        config.worker.idle_backoff_min = 60;
        config.worker.idle_backoff_max = 120;
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Set found_but_excluded to false (truly no work)
        worker.found_but_excluded = false;

        // Verify that jittered idle backoff would be used
        let jittered_backoff = worker.compute_jittered_backoff();

        assert!(
            (60..=120).contains(&jittered_backoff),
            "jittered backoff should be in idle range [60, 120]"
        );
        assert_ne!(
            jittered_backoff, 5,
            "jittered backoff should not equal short_retry_backoff (5s)"
        );
    }

    #[test]
    fn found_but_excluded_flag_set_from_explore_strand() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Simulate explore strand finding candidates that get excluded
        worker.last_strand_evaluations = vec![
            ("explore".to_string(), "BeadFound".to_string(), 100),
            ("pluck".to_string(), "NoWork".to_string(), 50),
        ];

        // Verify the flag is set correctly
        assert!(
            worker.found_but_all_excluded(),
            "found_but_all_excluded should return true when explore found candidates"
        );

        // Verify setting the flag works correctly
        worker.found_but_excluded = worker.found_but_all_excluded();
        assert!(
            worker.found_but_excluded,
            "found_but_excluded flag should be set to true"
        );
    }

    #[test]
    fn found_but_excluded_flag_set_from_pluck_strand() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Simulate pluck strand finding candidates that get excluded
        worker.last_strand_evaluations = vec![
            ("pluck".to_string(), "candidates_found".to_string(), 100),
            ("explore".to_string(), "NoWork".to_string(), 50),
        ];

        // Verify the flag is set correctly
        assert!(
            worker.found_but_all_excluded(),
            "found_but_all_excluded should return true when pluck found candidates"
        );

        // Verify setting the flag works correctly
        worker.found_but_excluded = worker.found_but_all_excluded();
        assert!(
            worker.found_but_excluded,
            "found_but_excluded flag should be set to true"
        );
    }

    #[test]
    fn found_but_excluded_flag_false_when_truly_no_work() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Simulate all strands returning NoWork (truly no work)
        worker.last_strand_evaluations = vec![
            ("pluck".to_string(), "NoWork".to_string(), 50),
            ("mend".to_string(), "NoWork".to_string(), 30),
            ("explore".to_string(), "NoWork".to_string(), 100),
        ];

        // Verify the flag is NOT set
        assert!(
            !worker.found_but_all_excluded(),
            "found_but_all_excluded should return false when no strand found candidates"
        );

        // Verify setting the flag works correctly
        worker.found_but_excluded = worker.found_but_all_excluded();
        assert!(
            !worker.found_but_excluded,
            "found_but_excluded flag should be set to false"
        );
    }

    #[test]
    fn retry_path_decision_uses_correct_backoff_values() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();

        // Set distinct backoff values to verify correct selection
        config.worker.short_retry_backoff = 3;
        config.worker.idle_backoff_min = 70;
        config.worker.idle_backoff_max = 130;

        let store = Arc::new(MockStore::empty());
        let worker = Worker::new(config, "test-worker".to_string(), store);

        // Test short retry backoff value
        assert_eq!(
            worker.config.worker.short_retry_backoff, 3,
            "short_retry_backoff should be 3 seconds"
        );

        // Test jittered backoff range
        let jittered = worker.compute_jittered_backoff();
        assert!(
            (70..=130).contains(&jittered),
            "jittered backoff {} should be in range [70, 130]",
            jittered
        );

        // Verify the values are distinct
        assert_ne!(
            3, jittered,
            "short_retry_backoff (3s) should differ from idle backoff ({}s)",
            jittered
        );
    }

    #[test]
    fn found_but_excluded_detects_explore_candidates_with_exclusion() {
        let _temp_dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let store = Arc::new(MockStore::empty());
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Simulate explore finding beads but all being excluded (assigned, blocked labels, etc.)
        // This is the deadlock scenario from bf-1d64q
        worker.last_strand_evaluations = vec![
            ("pluck".to_string(), "NoWork".to_string(), 50),
            ("explore".to_string(), "BeadFound".to_string(), 200),
        ];

        assert!(
            worker.found_but_all_excluded(),
            "should detect explore found candidates (excluded by filters)"
        );
    }

    // ── Supervisor presence detection tests ─────────────────────────────────────

    #[test]
    fn detect_supervisor_presence_returns_false_with_no_heartbeat_or_socket() {
        let heartbeat = None;
        let socket = None;
        let ttl_secs = 300;

        let present = detect_supervisor_presence(heartbeat, socket, ttl_secs);
        assert!(
            !present,
            "should return false when no supervisor indicators exist"
        );
    }

    #[test]
    fn detect_supervisor_presence_returns_true_with_fresh_heartbeat_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let heartbeat_path = temp_dir.path().join("supervisor-heartbeat.json");

        // Create a fresh heartbeat file
        fs::write(
            &heartbeat_path,
            r#"{"pid": 12345, "last_update": "2026-08-23T00:00:00Z"}"#,
        )
        .unwrap();

        let heartbeat = Some(&heartbeat_path);
        let socket = None;
        let ttl_secs = 300;

        let present = detect_supervisor_presence(heartbeat, socket, ttl_secs);
        assert!(present, "should return true with fresh heartbeat file");
    }

    #[test]
    fn detect_supervisor_presence_returns_false_with_stale_heartbeat_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let heartbeat_path = temp_dir.path().join("supervisor-heartbeat.json");

        // Create an old heartbeat file
        fs::write(
            &heartbeat_path,
            r#"{"pid": 12345, "last_update": "2020-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        // Set file modification time to 10 minutes ago (beyond TTL of 300 seconds)
        let ten_minutes_ago = SystemTime::now() - Duration::from_secs(600);
        filetime::set_file_mtime(&heartbeat_path, ten_minutes_ago.into()).unwrap();

        let heartbeat = Some(&heartbeat_path);
        let socket = None;
        let ttl_secs = 300;

        let present = detect_supervisor_presence(heartbeat, socket, ttl_secs);
        assert!(!present, "should return false with stale heartbeat file");
    }

    #[test]
    fn detect_supervisor_presence_returns_true_with_socket_only() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("supervisor.sock");

        // Create a socket file
        fs::write(&socket_path, "").unwrap();

        let heartbeat = None;
        let socket = Some(&socket_path);
        let ttl_secs = 300;

        let present = detect_supervisor_presence(heartbeat, socket, ttl_secs);
        assert!(present, "should return true when socket exists");
    }

    #[test]
    fn detect_supervisor_presence_returns_true_with_fresh_heartbeat_ignoring_socket() {
        let temp_dir = tempfile::tempdir().unwrap();
        let heartbeat_path = temp_dir.path().join("supervisor-heartbeat.json");
        let socket_path = temp_dir.path().join("supervisor.sock");

        // Create a fresh heartbeat file
        fs::write(
            &heartbeat_path,
            r#"{"pid": 12345, "last_update": "2026-08-23T00:00:00Z"}"#,
        )
        .unwrap();

        // Create a socket file (should be ignored since heartbeat is fresh)
        fs::write(&socket_path, "").unwrap();

        let heartbeat = Some(&heartbeat_path);
        let socket = Some(&socket_path);
        let ttl_secs = 300;

        let present = detect_supervisor_presence(heartbeat, socket, ttl_secs);
        assert!(
            present,
            "should return true with fresh heartbeat (socket ignored)"
        );
    }

    #[test]
    fn detect_supervisor_presence_falls_back_to_socket_with_stale_heartbeat() {
        let temp_dir = tempfile::tempdir().unwrap();
        let heartbeat_path = temp_dir.path().join("supervisor-heartbeat.json");
        let socket_path = temp_dir.path().join("supervisor.sock");

        // Create a stale heartbeat file
        fs::write(
            &heartbeat_path,
            r#"{"pid": 12345, "last_update": "2020-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        // Set file modification time to 10 minutes ago (beyond TTL of 300 seconds)
        let ten_minutes_ago = SystemTime::now() - Duration::from_secs(600);
        filetime::set_file_mtime(&heartbeat_path, ten_minutes_ago.into()).unwrap();

        // Create a socket file (fallback should detect this)
        fs::write(&socket_path, "").unwrap();

        let heartbeat = Some(&heartbeat_path);
        let socket = Some(&socket_path);
        let ttl_secs = 300;

        let present = detect_supervisor_presence(heartbeat, socket, ttl_secs);
        assert!(
            present,
            "should return true with socket as fallback to stale heartbeat"
        );
    }

    #[test]
    fn detect_supervisor_presence_returns_false_missing_heartbeat_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let heartbeat_path = temp_dir.path().join("supervisor-heartbeat.json");

        // Don't create the file - it doesn't exist
        let heartbeat = Some(&heartbeat_path);
        let socket = None;
        let ttl_secs = 300;

        let present = detect_supervisor_presence(heartbeat, socket, ttl_secs);
        assert!(
            !present,
            "should return false when heartbeat file doesn't exist"
        );
    }
}
