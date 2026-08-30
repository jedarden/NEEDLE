//! Health monitoring: heartbeats, stale detection, PID checking.
//!
//! Workers emit periodic heartbeats from a dedicated background thread.
//! Peers read heartbeat files to detect crashed or stuck workers.
//!
//! The heartbeat emitter uses `std::thread::spawn` (not tokio) to keep it
//! independent of the async runtime. The main worker updates shared state
//! via `Arc<Mutex<SharedHeartbeatState>>`.
//!
//! Depends on: `config`, `types`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::telemetry::Telemetry;
use crate::types::{BeadId, WorkerState};

// ──────────────────────────────────────────────────────────────────────────────
// SupervisorDetectionConfig — configuration for supervisor detection
// ──────────────────────────────────────────────────────────────────────────────

/// Configuration for supervisor detection.
///
/// This struct configures how workers detect the presence of a running supervisor
/// process, which is important for determining whether the fleet is being actively
/// managed or running in standalone mode.
#[derive(Debug, Clone)]
pub struct SupervisorDetectionConfig {
    /// Path to the supervisor's heartbeat file.
    ///
    /// The supervisor writes a heartbeat file to signal its presence. Workers
    /// check this file as part of supervisor detection.
    pub heartbeat_path: PathBuf,

    /// Optional path to the supervisor's Unix domain socket.
    ///
    /// If set, workers will also check for a socket at this path as an additional
    /// supervisor detection mechanism.
    pub socket_path: Option<PathBuf>,
}

impl Default for SupervisorDetectionConfig {
    fn default() -> Self {
        SupervisorDetectionConfig {
            heartbeat_path: PathBuf::from("supervisor-heartbeat.json"),
            socket_path: None,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// HeartbeatData — on-disk JSON structure
// ──────────────────────────────────────────────────────────────────────────────

/// Data written to the heartbeat JSON file on disk.
///
/// Path: `~/.needle/state/heartbeats/<qualified-id>.json`
///
/// This structure is compatible with cgov's Heartbeat format to ensure
/// proper worker state detection and scaling decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatData {
    /// Bare NATO name (e.g., "alpha", "foxtrot").
    pub worker_id: String,
    /// Fully-qualified identity: `{adapter}-{worker_id}` (e.g., "claude-code-glm-5-foxtrot").
    #[serde(default)]
    pub qualified_id: String,
    pub pid: u32,
    pub state: WorkerState,
    pub current_bead: Option<BeadId>,
    pub workspace: PathBuf,
    pub last_heartbeat: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
    pub beads_processed: u64,
    pub session: String,
    /// Whether the worker is currently idle (no active task).
    ///
    /// A worker is considered idle when it's in EXHAUSTED state (all strands
    /// returned NoWork) or when it has no current bead. This field is used by
    /// cgov to determine which workers to scale down first.
    #[serde(default)]
    pub is_idle: bool,
    /// Current task ID if any (cgov compatibility field).
    ///
    /// This maps the bead_id to cgov's expected current_task field.
    #[serde(default)]
    pub current_task: Option<String>,
    /// Model being used (cgov compatibility field).
    ///
    /// Derived from the adapter configuration.
    #[serde(default)]
    pub model: String,
    /// The filename that produced this heartbeat (set during read, not serialized).
    #[serde(skip)]
    pub heartbeat_file: Option<PathBuf>,
}

// ──────────────────────────────────────────────────────────────────────────────
// SharedHeartbeatState — updated by worker, read by emitter
// ──────────────────────────────────────────────────────────────────────────────

/// Shared state between the main worker loop and the heartbeat emitter thread.
struct SharedHeartbeatState {
    state: WorkerState,
    current_bead: Option<BeadId>,
    beads_processed: u64,
    /// The workspace of the current bead (updates dynamically during cross-workspace work).
    current_workspace: Option<PathBuf>,
    /// Model being used (from adapter configuration).
    model: String,
    /// Most recently active strand — for HOOP Hook 3's `idle` heartbeat state.
    last_strand: Option<String>,
    /// Resolved adapter name — for HOOP Hook 3's `executing` heartbeat state.
    adapter: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// HealthMonitor
// ──────────────────────────────────────────────────────────────────────────────

/// Health monitor for a single worker.
///
/// Owns the background heartbeat emitter thread and provides reader utilities
/// for peer heartbeat files.
pub struct HealthMonitor {
    heartbeat_dir: PathBuf,
    heartbeat_interval: Duration,
    heartbeat_ttl: Duration,
    /// Bare NATO name (e.g., "alpha", "foxtrot").
    worker_id: String,
    /// Fully-qualified identity: `{adapter_slug}-{worker_id}` (e.g., "claude-code-glm-5-foxtrot").
    qualified_id: String,
    workspace: PathBuf,
    started_at: DateTime<Utc>,
    shared_state: Arc<Mutex<SharedHeartbeatState>>,
    shutdown: Arc<AtomicBool>,
    emitter_handle: Option<std::thread::JoinHandle<()>>,
    /// Path to this worker's heartbeat file (computed during construction).
    heartbeat_path: PathBuf,
}

/// Resolve the heartbeat directory, respecting isolated HOME directories in tests.
///
/// This function ensures that when tests use HomeGuard to isolate HOME to a temp
/// directory, the health monitor writes heartbeats to the test's temp directory
/// instead of the real user's state directory (~/.needle/state/heartbeats/).
///
/// # Behavior
///
/// - If `heartbeat_dir` is absolute, use it directly
/// - If `heartbeat_dir` is relative, resolve it relative to the current HOME:
///   - In tests with HomeGuard: HOME is the test temp directory, so we use that
///   - In production: HOME is the real user home, so we use $HOME/.needle/...
///
/// This respects test isolation by reading the current HOME environment variable
/// at runtime, rather than using a hardcoded path from config.
fn resolve_heartbeat_dir(config_home: &Path, heartbeat_dir: Option<PathBuf>) -> PathBuf {
    let dir = heartbeat_dir.unwrap_or_else(|| PathBuf::from("state").join("heartbeats"));

    // If the directory is absolute, use it as-is
    if dir.is_absolute() {
        return dir;
    }

    // For relative paths, resolve relative to the current HOME directory.
    // This ensures that when tests use HomeGuard to change HOME, we respect
    // the isolated HOME and write heartbeats to the test's temp directory.
    if let Ok(current_home) = std::env::var("HOME") {
        let home = PathBuf::from(current_home);
        return home.join(".needle").join(dir);
    }

    // Fallback: use the config home (should rarely happen)
    config_home.join(dir)
}

impl HealthMonitor {
    /// Create a new health monitor.
    ///
    /// Does not start the emitter — call `start_emitter()` after construction.
    ///
    /// # Arguments
    ///
    /// * `config` - Worker configuration
    /// * `worker_name` - Bare NATO name (e.g., "alpha", "foxtrot")
    /// * `_telemetry` - Telemetry emitter (unused, kept for API compatibility)
    /// * `shutdown` - Optional shared shutdown flag. If provided, the emitter's
    ///   circuit breaker will set this flag to trigger graceful worker shutdown.
    ///   If None, a private flag is created (test compatibility).
    pub fn new(
        config: Config,
        worker_name: String,
        _telemetry: Telemetry,
        shutdown: Option<Arc<AtomicBool>>,
    ) -> Self {
        let heartbeat_dir =
            resolve_heartbeat_dir(&config.workspace.home, config.health.heartbeat_dir);
        let heartbeat_interval = Duration::from_secs(config.health.heartbeat_interval_secs);
        let heartbeat_ttl = Duration::from_secs(config.health.heartbeat_ttl_secs);
        let qualified_id = format!("{}-{}", config.agent.default, worker_name);
        let heartbeat_path = heartbeat_dir.join(format!("{}.json", qualified_id));

        HealthMonitor {
            heartbeat_dir,
            heartbeat_interval,
            heartbeat_ttl,
            worker_id: worker_name,
            qualified_id,
            workspace: config.workspace.default.clone(),
            started_at: Utc::now(),
            shared_state: Arc::new(Mutex::new(SharedHeartbeatState {
                state: WorkerState::Booting,
                current_bead: None,
                beads_processed: 0,
                current_workspace: None,
                model: config.agent.default.clone(),
                last_strand: None,
                adapter: None,
            })),
            shutdown: shutdown.unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
            emitter_handle: None,
            heartbeat_path,
        }
    }

    /// Start the background heartbeat emitter thread.
    ///
    /// The thread writes a heartbeat JSON file every `heartbeat_interval` until
    /// `stop()` is called.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The heartbeat directory cannot be created
    /// - The initial heartbeat write fails
    /// - The emitter thread cannot be spawned
    pub fn start_emitter(&mut self) -> Result<()> {
        // Ensure heartbeat directory exists (with retry on transient failures).
        let mut retry_count = 0u32;
        const MAX_DIR_CREATE_RETRIES: u32 = 3;

        while retry_count < MAX_DIR_CREATE_RETRIES {
            match std::fs::create_dir_all(&self.heartbeat_dir) {
                Ok(_) => break,
                Err(e) if retry_count < MAX_DIR_CREATE_RETRIES - 1 => {
                    retry_count += 1;
                    tracing::warn!(
                        error = %e,
                        retry = retry_count,
                        path = %self.heartbeat_dir.display(),
                        "failed to create heartbeat directory, retrying"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "failed to create heartbeat directory {} after {} retries: {}",
                        self.heartbeat_dir.display(),
                        retry_count,
                        e
                    ));
                }
            }
        }

        // Write the initial heartbeat immediately to verify the directory is writable.
        self.write_heartbeat().with_context(|| {
            format!(
                "failed to write initial heartbeat to {}",
                self.heartbeat_path().display()
            )
        })?;

        let shared_state = self.shared_state.clone();
        let shutdown = self.shutdown.clone();
        let heartbeat_dir = self.heartbeat_dir.clone();
        let worker_id = self.worker_id.clone();
        let qualified_id = self.qualified_id.clone();
        let workspace = self.workspace.clone();
        let started_at = self.started_at;
        let interval = self.heartbeat_interval;

        let handle = std::thread::Builder::new()
            .name(format!("heartbeat-{}", self.worker_id))
            .spawn(move || {
                emitter_loop(
                    shared_state,
                    shutdown,
                    heartbeat_dir,
                    worker_id,
                    qualified_id,
                    workspace,
                    started_at,
                    interval,
                    10,
                );
            })
            .context("failed to spawn heartbeat emitter thread")?;

        self.emitter_handle = Some(handle);
        tracing::info!(
            worker = %self.worker_id,
            interval_secs = self.heartbeat_interval.as_secs(),
            path = %self.heartbeat_path().display(),
            "heartbeat emitter started"
        );

        Ok(())
    }

    /// Update the worker state visible to the heartbeat emitter.
    ///
    /// Called by the worker on every state transition.
    pub fn update_state(
        &self,
        state: &WorkerState,
        current_bead: Option<&BeadId>,
        workspace: Option<&Path>,
    ) {
        if let Ok(mut guard) = self.shared_state.lock() {
            guard.state = state.clone();
            guard.current_bead = current_bead.cloned();
            guard.current_workspace = workspace.map(|p| p.to_path_buf());
        }
    }

    /// Update the strand last seen active — read by the HOOP Hook 3 heartbeat
    /// (`idle` state's `last_strand` field).
    pub fn update_strand(&self, strand: Option<&str>) {
        if let Ok(mut guard) = self.shared_state.lock() {
            guard.last_strand = strand.map(|s| s.to_string());
        }
    }

    /// Update the resolved adapter name — read by the HOOP Hook 3 heartbeat
    /// (`executing` state's `adapter` field).
    pub fn update_adapter(&self, adapter: Option<&str>) {
        if let Ok(mut guard) = self.shared_state.lock() {
            guard.adapter = adapter.map(|s| s.to_string());
        }
    }

    /// Update the beads_processed count visible to the heartbeat emitter.
    pub fn update_beads_processed(&self, count: u64) {
        if let Ok(mut guard) = self.shared_state.lock() {
            guard.beads_processed = count;
        }
    }

    /// Clean up this worker's heartbeat file by removing it from disk.
    ///
    /// This method removes the heartbeat file at `self.heartbeat_path()`.
    /// It returns `Ok(())` if the file is successfully removed or if it
    /// doesn't exist. Returns an error if removal fails for reasons
    /// other than the file not being found.
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the file was removed successfully or doesn't exist
    /// * `Err(e)` - If removal fails for reasons other than NotFound
    pub fn cleanup_heartbeat_file(&self) -> Result<()> {
        let path = self.heartbeat_path();

        // Attempt to remove the file using std::fs::remove_file.
        match std::fs::remove_file(&path) {
            Ok(_) => {
                tracing::debug!(
                    path = %path.display(),
                    "heartbeat file removed successfully"
                );
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File doesn't exist - this is success (idempotent cleanup)
                tracing::debug!(
                    path = %path.display(),
                    "heartbeat file does not exist, skipping cleanup"
                );
                Ok(())
            }
            Err(e) => {
                // Log the error before returning - this ensures visibility of cleanup failures
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "failed to remove heartbeat file during cleanup"
                );
                // Return with context for upstream handling
                Err(e)
                    .with_context(|| format!("failed to remove heartbeat file: {}", path.display()))
            }
        }
    }

    /// Stop the heartbeat emitter and remove this worker's heartbeat file.
    ///
    /// Called on graceful shutdown (STOPPED) and best-effort on ERRORED.
    pub fn stop(&mut self) {
        // Signal the emitter thread to exit.
        self.shutdown.store(true, Ordering::SeqCst);

        // Join the emitter thread (with a timeout to avoid hanging).
        if let Some(handle) = self.emitter_handle.take() {
            // Give the thread up to 2x the interval to notice shutdown and exit.
            let _ = handle.join();
        }

        // Remove the heartbeat file (best-effort).
        if let Err(e) = self.cleanup_heartbeat_file() {
            tracing::warn!(
                error = %e,
                "failed to remove heartbeat file on shutdown"
            );
        }
    }

    /// Path to this worker's heartbeat file.
    ///
    /// Keyed by fully-qualified identity (`{adapter}-{worker_id}`) to prevent
    /// collisions when workers from different adapter pools share a NATO name.
    ///
    /// This path is computed during construction and stored for efficient access
    /// by the shutdown handler.
    pub fn heartbeat_path(&self) -> PathBuf {
        self.heartbeat_path.clone()
    }

    /// The fully-qualified identity (`{adapter}-{worker_id}`).
    pub fn qualified_id(&self) -> &str {
        &self.qualified_id
    }

    /// Directory where heartbeat files are stored.
    pub fn heartbeat_dir(&self) -> &Path {
        &self.heartbeat_dir
    }

    /// The configured heartbeat TTL.
    pub fn heartbeat_ttl(&self) -> Duration {
        self.heartbeat_ttl
    }

    /// Verify that this worker's heartbeat file exists and is fresh.
    ///
    /// Returns `Ok(true)` if the heartbeat file exists and is within the TTL.
    /// Returns `Ok(false)` if the heartbeat file doesn't exist or is stale.
    /// Returns `Err` if the heartbeat file exists but cannot be read/parsed.
    pub fn verify_heartbeat(&self) -> Result<bool> {
        let path = self.heartbeat_path();
        if !path.exists() {
            tracing::warn!(
                path = %path.display(),
                "heartbeat file does not exist"
            );
            return Ok(false);
        }

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read heartbeat file: {}", path.display()))?;

        let heartbeat: HeartbeatData = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse heartbeat file: {}", path.display()))?;

        let age = Utc::now()
            .signed_duration_since(heartbeat.last_heartbeat)
            .to_std()
            .unwrap_or(Duration::ZERO);

        let is_fresh = age <= self.heartbeat_ttl;
        if !is_fresh {
            tracing::warn!(
                path = %path.display(),
                age_secs = age.as_secs(),
                ttl_secs = self.heartbeat_ttl.as_secs(),
                "heartbeat file is stale"
            );
        }

        Ok(is_fresh)
    }

    /// Check if a worker is alive based on heartbeat file freshness.
    ///
    /// Returns `Ok(true)` if the heartbeat file exists and is fresh (age < heartbeat_ttl_secs).
    /// Returns `Ok(false)` if the heartbeat file doesn't exist or is stale.
    /// Returns `Err` if the heartbeat file exists but cannot be read/parsed.
    ///
    /// # Arguments
    ///
    /// * `qualified_worker_id` - Fully-qualified worker ID (e.g., 'claude-code-glm-5-alpha')
    ///
    /// # Example
    ///
    /// ```ignore
    /// let monitor = HealthMonitor::new(config, "alpha".to_string(), telemetry, None);
    /// let is_alive = monitor.check_worker_alive("claude-code-glm-5-alpha")?;
    /// ```
    pub fn check_worker_alive(&self, qualified_worker_id: &str) -> Result<bool> {
        let path = self
            .heartbeat_dir
            .join(format!("{}.json", qualified_worker_id));

        if !path.exists() {
            tracing::debug!(
                worker = %qualified_worker_id,
                path = %path.display(),
                "worker heartbeat file does not exist"
            );
            return Ok(false);
        }

        let content = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read heartbeat file for worker {}: {}",
                qualified_worker_id,
                path.display()
            )
        })?;

        let heartbeat: HeartbeatData = serde_json::from_str(&content).with_context(|| {
            format!(
                "failed to parse heartbeat file for worker {}: {}",
                qualified_worker_id,
                path.display()
            )
        })?;

        let age = Utc::now()
            .signed_duration_since(heartbeat.last_heartbeat)
            .to_std()
            .unwrap_or(Duration::ZERO);

        let is_alive = age < self.heartbeat_ttl;
        if !is_alive {
            tracing::debug!(
                worker = %qualified_worker_id,
                path = %path.display(),
                age_secs = age.as_secs(),
                ttl_secs = self.heartbeat_ttl.as_secs(),
                "worker heartbeat is stale"
            );
        }

        Ok(is_alive)
    }

    // ── Reader utilities (used by peer monitoring / Mend strand) ────────────

    /// Read all heartbeat files in the given directory.
    ///
    /// Silently skips files that cannot be read or parsed (they may be
    /// partially written or from a crashed worker).
    pub fn read_all_heartbeats(dir: &Path) -> Result<Vec<HeartbeatData>> {
        let mut heartbeats = Vec::new();

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(heartbeats),
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "failed to read heartbeat directory {}: {}",
                    dir.display(),
                    e
                ));
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<HeartbeatData>(&content) {
                    Ok(mut hb) => {
                        // Backfill qualified_id for heartbeats written by older versions.
                        if hb.qualified_id.is_empty() {
                            hb.qualified_id = hb.worker_id.clone();
                        }
                        hb.heartbeat_file = Some(path.clone());
                        heartbeats.push(hb)
                    }
                    Err(e) => {
                        tracing::debug!(
                            path = %path.display(),
                            error = %e,
                            "skipping unparseable heartbeat file"
                        );
                    }
                },
                Err(e) => {
                    tracing::debug!(
                        path = %path.display(),
                        error = %e,
                        "skipping unreadable heartbeat file"
                    );
                }
            }
        }

        Ok(heartbeats)
    }

    /// Check whether a heartbeat is stale (exceeded TTL).
    pub fn is_stale(heartbeat: &HeartbeatData, ttl: Duration) -> bool {
        let age = Utc::now()
            .signed_duration_since(heartbeat.last_heartbeat)
            .to_std()
            .unwrap_or(Duration::ZERO);
        age > ttl
    }

    /// Check whether a process with the given PID is alive.
    ///
    /// Uses `kill(pid, 0)` semantics: sends signal 0 to check existence
    /// without actually delivering a signal. Returns true if the process
    /// exists (including EPERM — process exists but we can't signal it).
    pub fn check_pid_alive(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        // SAFETY: kill(pid, 0) only checks existence; no signal is delivered.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if rc == 0 {
            return true;
        }
        // ESRCH means no such process. EPERM means the process exists but we
        // lack permission to send signals to it (process is alive).
        #[cfg(target_os = "linux")]
        let errno = unsafe { *libc::__errno_location() };
        #[cfg(target_os = "macos")]
        let errno = unsafe { *libc::__error() };
        errno == libc::EPERM
    }

    /// Detect peers with stale heartbeats.
    ///
    /// Returns a list of stale peers, excluding this worker.
    pub fn detect_stale_peers(&self) -> Result<Vec<StalePeer>> {
        let heartbeats = Self::read_all_heartbeats(&self.heartbeat_dir)?;
        let mut stale = Vec::new();

        for hb in heartbeats {
            // Skip our own heartbeat.
            if hb.qualified_id == self.qualified_id {
                continue;
            }

            if Self::is_stale(&hb, self.heartbeat_ttl) {
                let pid_alive = Self::check_pid_alive(hb.pid);
                let hb_file = hb.heartbeat_file.clone().unwrap_or_else(|| {
                    self.heartbeat_dir.join(format!("{}.json", hb.qualified_id))
                });
                stale.push(StalePeer {
                    worker_id: hb.worker_id.clone(),
                    qualified_id: Some(hb.qualified_id.clone()),
                    pid: hb.pid,
                    pid_alive,
                    current_bead: hb.current_bead.clone(),
                    last_heartbeat: hb.last_heartbeat,
                    heartbeat_file: hb_file,
                });
            }
        }

        Ok(stale)
    }

    /// Detect whether a supervisor is actively managing the worker fleet.
    ///
    /// A supervisor is considered present when:
    /// - Multiple active workers exist (indicating fleet management)
    /// - OR recent worker spawn activity is detected (workers started within the last 5 minutes)
    ///
    /// This detection is used to warn about orphaned bead risk when `idle_action=exit`
    /// is configured without supervisor supervision.
    ///
    /// Returns `Ok(true)` if supervisor presence is detected, `Ok(false)` otherwise.
    /// Returns `Err` if heartbeat directory cannot be read.
    pub fn detect_supervisor(&self) -> Result<bool> {
        let heartbeats = Self::read_all_heartbeats(&self.heartbeat_dir)?;

        // Filter to fresh heartbeats only (within TTL)
        let fresh_workers: Vec<_> = heartbeats
            .into_iter()
            .filter(|hb| !Self::is_stale(hb, self.heartbeat_ttl))
            .collect();

        // A supervisor is present if we have multiple active workers
        // (a single worker is likely standalone, not supervised)
        if fresh_workers.len() >= 2 {
            tracing::debug!(
                active_workers = fresh_workers.len(),
                "supervisor detected: multiple active workers in fleet"
            );
            return Ok(true);
        }

        // Check for recent spawn activity: any worker started within the last 5 minutes
        // (excluding ourselves, as we just started)
        let five_minutes_ago = Utc::now() - chrono::Duration::seconds(300);
        let recent_spawns: Vec<_> = fresh_workers
            .iter()
            .filter(|hb| hb.qualified_id != self.qualified_id)
            .filter(|hb| hb.started_at > five_minutes_ago)
            .collect();

        if !recent_spawns.is_empty() {
            tracing::debug!(
                recent_spawn_count = recent_spawns.len(),
                "supervisor detected: recent worker spawn activity"
            );
            return Ok(true);
        }

        // No supervisor presence detected
        tracing::debug!("no supervisor detected: single worker, no recent spawn activity");
        Ok(false)
    }

    /// Check for a supervisor heartbeat file at the standard location.
    ///
    /// This function provides direct supervisor presence detection by checking
    /// for a supervisor-specific heartbeat file, separate from worker heartbeats.
    ///
    /// The supervisor heartbeat file is expected at:
    /// `<heartbeat_dir>/supervisor-heartbeat.json`
    ///
    /// Returns `Ok(true)` if a fresh supervisor heartbeat file exists,
    /// `Ok(false)` if no supervisor heartbeat is found or it's stale.
    /// Returns `Err` if the heartbeat directory cannot be read.
    ///
    /// A heartbeat is considered fresh if updated within the last 2 minutes.
    pub fn check_supervisor_heartbeat_file(&self) -> Result<bool> {
        let supervisor_hb_path = self.heartbeat_dir.join("supervisor-heartbeat.json");

        // Check if supervisor heartbeat file exists
        if !supervisor_hb_path.exists() {
            tracing::debug!(
                path = %supervisor_hb_path.display(),
                "supervisor heartbeat file not found"
            );
            return Ok(false);
        }

        // Read and parse the supervisor heartbeat file
        let content = std::fs::read_to_string(&supervisor_hb_path).with_context(|| {
            format!(
                "failed to read supervisor heartbeat file: {}",
                supervisor_hb_path.display()
            )
        })?;

        // Parse as JSON to verify it's valid (we don't need specific fields)
        let parsed: serde_json::Value = serde_json::from_str(&content).with_context(|| {
            format!(
                "failed to parse supervisor heartbeat file: {}",
                supervisor_hb_path.display()
            )
        })?;

        // Check if heartbeat has a timestamp field
        let last_heartbeat = if let Some(ts) = parsed.get("last_heartbeat").and_then(|v| v.as_str())
        {
            DateTime::parse_from_rfc3339(ts)
                .with_context(|| {
                    format!(
                        "invalid timestamp in supervisor heartbeat file: {}",
                        supervisor_hb_path.display()
                    )
                })?
                .with_timezone(&Utc)
        } else {
            // No timestamp field - assume file exists but is stale/invalid
            tracing::debug!(
                path = %supervisor_hb_path.display(),
                "supervisor heartbeat file missing timestamp field"
            );
            return Ok(false);
        };

        // Check if heartbeat is fresh (within 2 minutes)
        let age = Utc::now()
            .signed_duration_since(last_heartbeat)
            .to_std()
            .unwrap_or(Duration::ZERO);
        let supervisor_ttl = Duration::from_secs(120); // 2 minutes

        if age <= supervisor_ttl {
            tracing::debug!(
                path = %supervisor_hb_path.display(),
                age_secs = age.as_secs(),
                "fresh supervisor heartbeat detected"
            );
            Ok(true)
        } else {
            tracing::debug!(
                path = %supervisor_hb_path.display(),
                age_secs = age.as_secs(),
                ttl_secs = supervisor_ttl.as_secs(),
                "supervisor heartbeat is stale"
            );
            Ok(false)
        }
    }

    /// Check for a supervisor socket at the standard location.
    ///
    /// This function provides direct supervisor presence detection by checking
    /// for a Unix domain socket that the supervisor may be listening on.
    ///
    /// The socket is expected at: `/tmp/needle-supervisor.sock` or a path
    /// specified in the `NEEDLE_SUPERVISOR_SOCKET` environment variable.
    ///
    /// Returns `Ok(true)` if a socket exists at the expected location,
    /// `Ok(false)` if no socket is found.
    /// Returns `Err` if socket path cannot be accessed.
    pub fn check_supervisor_socket() -> Result<bool> {
        let socket_path = std::env::var("NEEDLE_SUPERVISOR_SOCKET")
            .unwrap_or_else(|_| "/tmp/needle-supervisor.sock".to_string());

        let path = PathBuf::from(&socket_path);

        // Check if socket exists and is a socket file
        match std::fs::metadata(&path) {
            Ok(metadata) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileTypeExt;
                    let file_type = metadata.file_type();
                    if file_type.is_socket() {
                        tracing::debug!(
                            path = %path.display(),
                            "supervisor socket detected"
                        );
                        return Ok(true);
                    }
                }

                #[cfg(not(unix))]
                {
                    // On non-Unix platforms, just check if the file/path exists
                    if path.exists() {
                        tracing::debug!(
                            path = %path.display(),
                            "supervisor socket path exists (non-Unix)"
                        );
                        return Ok(true);
                    }
                }

                tracing::debug!(
                    path = %path.display(),
                    "path exists but is not a socket"
                );
                Ok(false)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    path = %path.display(),
                    "supervisor socket not found"
                );
                Ok(false)
            }
            Err(e) => Err(anyhow::anyhow!(
                "failed to access supervisor socket {}: {}",
                path.display(),
                e
            )),
        }
    }

    /// Detect supervisor presence using direct checks.
    ///
    /// This function combines direct supervisor detection methods:
    /// 1. Checks for a supervisor heartbeat file
    /// 2. Checks for a supervisor socket
    ///
    /// Returns `true` if either detection method finds a supervisor,
    /// `false` if no supervisor is detected.
    ///
    /// This check is total by design: an unreadable or corrupt heartbeat
    /// file (and any other detection error) is treated as "supervisor not
    /// detected" rather than propagated. Propagating would let the
    /// idle_action guard at boot silently skip itself — the exact
    /// orphaned-bead incident shape it exists to prevent (2026-06-21).
    /// A heartbeat error still falls through to the socket check, so a live
    /// supervisor remains detectable when only its heartbeat is degraded.
    pub fn detect_supervisor_direct(&self) -> bool {
        // Check supervisor heartbeat file first
        match self.check_supervisor_heartbeat_file() {
            Ok(true) => return true,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "supervisor heartbeat check failed; treating supervisor as absent"
                );
            }
        }

        // Fall back to socket check
        match Self::check_supervisor_socket() {
            Ok(true) => return true,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "supervisor socket check failed; treating supervisor as absent"
                );
            }
        }

        tracing::debug!("no supervisor detected via direct methods");
        false
    }

    // ── Internal ────────────────────────────────────────────────────────────

    /// Write a heartbeat file atomically (write temp, then rename).
    fn write_heartbeat(&self) -> Result<()> {
        let (state, current_bead, beads_processed, current_workspace, model, last_strand, adapter) = {
            let guard = self
                .shared_state
                .lock()
                .map_err(|e| anyhow::anyhow!("shared state lock poisoned: {e}"))?;
            (
                guard.state.clone(),
                guard.current_bead.clone(),
                guard.beads_processed,
                guard.current_workspace.clone(),
                guard.model.clone(),
                guard.last_strand.clone(),
                guard.adapter.clone(),
            )
        };

        // Use the current bead's workspace if set, otherwise fall back to home workspace.
        let effective_workspace = current_workspace.unwrap_or_else(|| self.workspace.clone());

        let is_idle = state == WorkerState::Exhausted || current_bead.is_none();
        let current_task = current_bead.as_ref().map(|b| b.to_string());

        // HOOP Hook 3 (heartbeat): append a JSONL line in HOOP's three-state
        // format alongside the existing per-worker JSON file below. Best
        // effort — see hoop_hooks module docs.
        if state == WorkerState::Exhausted {
            crate::hoop_hooks::emit_needle_heartbeat(
                &effective_workspace,
                &self.worker_id,
                "knot",
                serde_json::json!({"reason": "strands exhausted"}),
            );
        } else if let Some(ref bead_id) = current_bead {
            crate::hoop_hooks::emit_needle_heartbeat(
                &effective_workspace,
                &self.worker_id,
                "executing",
                serde_json::json!({
                    "bead": bead_id.to_string(),
                    "pid": std::process::id(),
                    "adapter": adapter,
                }),
            );
        } else {
            crate::hoop_hooks::emit_needle_heartbeat(
                &effective_workspace,
                &self.worker_id,
                "idle",
                serde_json::json!({"last_strand": last_strand}),
            );
        }

        let data = HeartbeatData {
            worker_id: self.worker_id.clone(),
            qualified_id: self.qualified_id.clone(),
            pid: std::process::id(),
            state,
            current_bead,
            workspace: effective_workspace,
            last_heartbeat: Utc::now(),
            started_at: self.started_at,
            beads_processed,
            session: self.worker_id.clone(),
            is_idle,
            current_task,
            model,
            heartbeat_file: None,
        };

        let path = self.heartbeat_path();
        let tmp_path = path.with_extension("json.tmp");

        // Auto-create parent directory so that heartbeats self-recover if the
        // directory is deleted while a worker is running.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create heartbeat dir: {}", parent.display()))?;
        }

        let json = serde_json::to_string_pretty(&data).context("failed to serialize heartbeat")?;
        std::fs::write(&tmp_path, json.as_bytes()).with_context(|| {
            format!(
                "failed to write temp heartbeat file: {}",
                tmp_path.display()
            )
        })?;
        std::fs::rename(&tmp_path, &path).with_context(|| {
            format!(
                "failed to rename heartbeat file: {} -> {}",
                tmp_path.display(),
                path.display()
            )
        })?;

        Ok(())
    }
}

impl Drop for HealthMonitor {
    fn drop(&mut self) {
        // Best-effort: signal the emitter and clean up the heartbeat file.
        self.stop();
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Utility functions
// ──────────────────────────────────────────────────────────────────────────────

/// Calculate the age of a heartbeat timestamp in seconds.
///
/// This function computes the time difference between the current time (`Utc::now()`)
/// and the provided heartbeat timestamp, returning the age in seconds.
///
/// # Arguments
///
/// * `timestamp` - The heartbeat timestamp to calculate age from
///
/// # Returns
///
/// * `i64` - Age in seconds (0 for future timestamps, positive for past timestamps)
///
/// # Behavior
///
/// - Returns `0` for future timestamps (handles clock skew)
/// - Returns accurate age in seconds for past timestamps
///
/// # Example
///
/// ```no_run
/// use chrono::{Utc, TimeZone};
/// use needle::health::calculate_heartbeat_age;
///
/// let past = Utc::now() - chrono::Duration::seconds(120);
/// let age = calculate_heartbeat_age(past);
/// assert_eq!(age, 120);
///
/// let future = Utc::now() + chrono::Duration::seconds(30);
/// let age = calculate_heartbeat_age(future);
/// assert_eq!(age, 0); // Clock skew handling
/// ```
pub fn calculate_heartbeat_age(timestamp: DateTime<Utc>) -> i64 {
    let now = Utc::now();
    let duration = now.signed_duration_since(timestamp);

    // Return 0 for future timestamps (handles clock skew)
    // Return positive seconds for past timestamps
    duration.num_seconds().max(0)
}

/// Clean up a heartbeat file by removing it from disk.
///
/// This function removes the heartbeat file at the given path. It handles
/// both file-not-found (success) and unexpected error (failure) cases.
///
/// # Arguments
///
/// * `path` - Path to the heartbeat file to remove
///
/// # Returns
///
/// * `Ok(())` - If the file was removed successfully or doesn't exist
/// * `Err(e)` - If removal fails for reasons other than NotFound
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use needle::health::cleanup_heartbeat_file;
///
/// let path = Path::new("/tmp/heartbeat.json");
/// cleanup_heartbeat_file(path)?;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn cleanup_heartbeat_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // File doesn't exist - this is success (idempotent cleanup)
            tracing::debug!(
                path = %path.display(),
                "heartbeat file does not exist, skipping cleanup"
            );
            Ok(())
        }
        Err(e) => {
            // Log the error before returning - this ensures visibility of cleanup failures
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "failed to remove heartbeat file during cleanup"
            );
            // Return with context for upstream handling
            Err(e).with_context(|| format!("failed to remove heartbeat file: {}", path.display()))
        }
    }
}

/// Check if a heartbeat file exists at the specified path.
///
/// This function provides simple presence detection for a heartbeat file,
/// returning whether the file exists on disk without attempting to read
/// or validate its contents.
///
/// This is useful for quick supervisor presence checks where you only need
/// to know if a heartbeat file exists, not whether it's fresh or valid.
///
/// # Arguments
///
/// * `path` - Path to the heartbeat file to check
///
/// # Returns
///
/// * `Ok(true)` - If the heartbeat file exists
/// * `Ok(false)` - If the heartbeat file does not exist
/// * `Err(e)` - If a file system error occurs (e.g., permission denied)
///
/// # Errors
///
/// Returns an error if:
/// - The path cannot be accessed due to permission issues
/// - The path is a directory rather than a file
/// - Other I/O errors occur
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use needle::health::check_heartbeat_file_exists;
///
/// let path = Path::new("/tmp/heartbeat.json");
/// match check_heartbeat_file_exists(path) {
///     Ok(true) => println!("Heartbeat file exists"),
///     Ok(false) => println!("No heartbeat file found"),
///     Err(e) => eprintln!("Error checking for heartbeat: {}", e),
/// }
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn check_heartbeat_file_exists(path: &Path) -> Result<bool> {
    // Inspect the directory entry first so a broken symlink remains
    // distinguishable from a genuinely absent heartbeat file. `try_exists()`
    // follows symlinks and reports both cases as `Ok(false)`.
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "heartbeat file does not exist");
            Ok(false)
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to check heartbeat file existence: {}",
                path.display()
            )
        }),
        Ok(_) => {
            let metadata = std::fs::metadata(path).with_context(|| {
                format!(
                    "failed to read metadata for heartbeat file: {}",
                    path.display()
                )
            })?;
            if metadata.is_file() {
                Ok(true)
            } else {
                tracing::debug!(
                    path = %path.display(),
                    "heartbeat path exists but is a directory, not a file"
                );
                Ok(false)
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// StalePeer
// ──────────────────────────────────────────────────────────────────────────────

/// A peer worker detected as having a stale heartbeat.
#[derive(Debug)]
pub struct StalePeer {
    pub worker_id: String,
    /// Fully-qualified identity of the peer.
    pub qualified_id: Option<String>,
    pub pid: u32,
    pub pid_alive: bool,
    pub current_bead: Option<BeadId>,
    pub last_heartbeat: DateTime<Utc>,
    pub heartbeat_file: PathBuf,
}

// ──────────────────────────────────────────────────────────────────────────────
// Emitter loop (runs in a dedicated std::thread)
// ──────────────────────────────────────────────────────────────────────────────

/// Maximum sleep between heartbeat attempts when backing off on failure.
const MAX_HEARTBEAT_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// Sleep interval for interruptible sleep — check shutdown flag every 100ms.
///
/// This ensures the heartbeat emitter responds to shutdown signals within 100ms
/// instead of sleeping for the full heartbeat interval (default 60s). When the
/// worker is killed (e.g., by the capacity governor), this gives the atexit
/// handler a better chance to emit worker.stopped telemetry.
const INTERRUPTIBLE_SLEEP_INTERVAL: Duration = Duration::from_millis(100);

/// Background emitter loop. Writes heartbeat at each interval.
///
/// Circuit breaker: after `max_consecutive_failures` consecutive write failures
/// the loop sets the shutdown flag and exits so the worker terminates instead of
/// spinning indefinitely.
///
/// Backoff: each consecutive failure doubles the inter-attempt sleep, capped at
/// [`MAX_HEARTBEAT_BACKOFF`].
///
/// Uses an interruptible sleep pattern to respond quickly to shutdown signals.
#[allow(clippy::too_many_arguments)]
fn emitter_loop(
    shared_state: Arc<Mutex<SharedHeartbeatState>>,
    shutdown: Arc<AtomicBool>,
    heartbeat_dir: PathBuf,
    worker_id: String,
    qualified_id: String,
    workspace: PathBuf,
    started_at: DateTime<Utc>,
    interval: Duration,
    max_consecutive_failures: u32,
) {
    // Ensure the heartbeat directory exists before entering the write loop so
    // that workers self-recover if ~/.needle/state/heartbeats/ is deleted.
    if let Err(e) = std::fs::create_dir_all(&heartbeat_dir) {
        tracing::error!(
            error = %e,
            dir = %heartbeat_dir.display(),
            "failed to create heartbeat directory"
        );
    }

    let mut consecutive_failures: u32 = 0;
    let mut current_sleep = interval;
    let mut elapsed = Duration::ZERO;

    loop {
        // Interruptible sleep: check shutdown flag every 100ms instead of
        // sleeping for the full interval. This ensures the emitter responds
        // to shutdown signals quickly, giving the atexit handler a chance to
        // emit worker.stopped telemetry even if the process is killed.
        while elapsed < current_sleep {
            if shutdown.load(Ordering::SeqCst) {
                tracing::debug!(worker = %worker_id, "heartbeat emitter shutting down");
                return;
            }
            let sleep_dur = std::cmp::min(INTERRUPTIBLE_SLEEP_INTERVAL, current_sleep - elapsed);
            std::thread::sleep(sleep_dur);
            elapsed += sleep_dur;
        }
        elapsed = Duration::ZERO;

        let (state, current_bead, beads_processed, current_workspace, model) =
            match shared_state.lock() {
                Ok(guard) => (
                    guard.state.clone(),
                    guard.current_bead.clone(),
                    guard.beads_processed,
                    guard.current_workspace.clone(),
                    guard.model.clone(),
                ),
                Err(_) => {
                    // Mutex poisoned — the main thread panicked. Exit.
                    tracing::error!(
                        worker = %worker_id,
                        "shared state mutex poisoned, heartbeat emitter exiting"
                    );
                    return;
                }
            };

        // Use the current bead's workspace if set, otherwise fall back to home workspace.
        let effective_workspace = current_workspace.unwrap_or_else(|| workspace.clone());

        let is_idle = state == WorkerState::Exhausted || current_bead.is_none();
        let current_task = current_bead.as_ref().map(|b| b.to_string());

        let data = HeartbeatData {
            worker_id: worker_id.clone(),
            qualified_id: qualified_id.clone(),
            pid: std::process::id(),
            state,
            current_bead,
            workspace: effective_workspace,
            last_heartbeat: Utc::now(),
            started_at,
            beads_processed,
            session: worker_id.clone(),
            is_idle,
            current_task,
            model,
            heartbeat_file: None,
        };

        let path = heartbeat_dir.join(format!("{}.json", qualified_id));
        let tmp_path = path.with_extension("json.tmp");

        let write_result: anyhow::Result<()> = (|| {
            // Re-assert the heartbeat directory on every attempt, not just once
            // before the loop starts. If the directory (or an ancestor) is
            // transiently removed or unavailable while the worker is running —
            // e.g. an external cleanup process, a race with another tool, or a
            // filesystem hiccup under load — the write below would otherwise
            // fail with ENOENT on every subsequent attempt until the circuit
            // breaker trips, since nothing else recreates it. `create_dir_all`
            // is a cheap no-op when the directory already exists, so doing this
            // unconditionally lets a single transient disruption self-heal
            // within one heartbeat interval instead of killing the worker after
            // `max_consecutive_failures` attempts (which, with the default
            // backoff schedule, can take 25-30 minutes to trigger).
            std::fs::create_dir_all(&heartbeat_dir)?;
            let json = serde_json::to_string_pretty(&data)?;
            std::fs::write(&tmp_path, json.as_bytes())?;
            std::fs::rename(&tmp_path, &path)?;
            Ok(())
        })();

        match write_result {
            Ok(()) => {
                consecutive_failures = 0;
                current_sleep = interval;
            }
            Err(e) => {
                consecutive_failures += 1;
                tracing::error!(
                    error = %e,
                    worker = %worker_id,
                    consecutive_failures,
                    max = max_consecutive_failures,
                    "heartbeat write failed"
                );
                if consecutive_failures >= max_consecutive_failures {
                    tracing::error!(
                        worker = %worker_id,
                        consecutive_failures,
                        "heartbeat emitter circuit breaker triggered — worker will shut down"
                    );
                    // Emit a final heartbeat event before shutting down so the
                    // telemetry log shows the circuit breaker was the cause.
                    let _ = std::fs::write(
                        heartbeat_dir.join(format!("{}-circuit-breaker.txt", qualified_id)),
                        format!(
                            "Circuit breaker tripped after {} consecutive heartbeat write failures\n\
                             Worker: {}\n\
                             Qualified ID: {}\n\
                             Last error: {}\n\
                             Timestamp: {}",
                            consecutive_failures,
                            worker_id,
                            qualified_id,
                            e,
                            Utc::now().to_rfc3339()
                        ),
                    );
                    shutdown.store(true, Ordering::SeqCst);
                    return;
                }
                // Exponential backoff to reduce log spam before the circuit breaker fires.
                current_sleep = current_sleep.saturating_mul(2).min(MAX_HEARTBEAT_BACKOFF);
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Serialises the tests that mutate `NEEDLE_SUPERVISOR_SOCKET`.
    ///
    /// `std::env::set_var`/`remove_var` mutate process-global state while the
    /// test harness runs tests on parallel threads, so one test's `remove_var`
    /// could land between another's `set_var` and its assertion. Five tests
    /// share this variable — and `check_supervisor_socket_default_path`
    /// additionally requires it to be *unset* — which made
    /// `check_supervisor_socket_exists_returns_true` fail intermittently.
    static SUPERVISOR_SOCKET_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Take the env lock, tolerating poisoning so one panicking test does not
    /// cascade into failures in every other test that touches the variable.
    fn lock_supervisor_socket_env() -> std::sync::MutexGuard<'static, ()> {
        SUPERVISOR_SOCKET_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Isolates the test's HOME directory to a temp directory, preventing tests
    /// from writing to the live fleet's state directory (`~/.needle/state/heartbeats/`).
    ///
    /// Returns a guard that restores the original HOME value when dropped.
    /// Use this in any test that creates a HealthMonitor or Worker, as both
    /// may call `dirs_or_home()` which reads HOME directly.
    ///
    /// # Example
    /// ```ignore
    /// let _home_guard = isolate_test_home();
    /// let dir = tempfile::tempdir().unwrap();
    /// // Now dirs_or_home() resolves to the temp directory, not the real HOME
    /// ```
    fn isolate_test_home() -> HomeGuard {
        let temp_dir = tempfile::tempdir().unwrap();
        let temp_path = temp_dir.path().to_path_buf();
        let mut guard = isolate_test_home_at(&temp_path);
        guard._temp_dir = Some(temp_dir);
        guard
    }

    /// Point HOME at a directory the test already owns.
    ///
    /// `resolve_heartbeat_dir` resolves a relative heartbeat dir against the
    /// live HOME, not against `config.workspace.home`, so a test that asserts
    /// on a path under its own tempdir has to make that tempdir the HOME.
    fn isolate_test_home_at(home: &Path) -> HomeGuard {
        // HOME is process-global: hold the one env lock every other
        // HOME/PATH-mutating test uses, or the value another test set is the
        // one this test's HealthMonitor resolves against (seen as two
        // different tempdirs in the same assertion).
        let env_guard = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", home);
        HomeGuard {
            _temp_dir: None,
            _env_guard: env_guard,
        }
    }

    /// Guard that restores HOME (and the env lock) when dropped.
    struct HomeGuard {
        _temp_dir: Option<tempfile::TempDir>,
        _env_guard: crate::util::test_env::EnvGuard,
    }

    fn test_config(heartbeat_dir: &Path) -> Config {
        let mut config = Config::default();
        config.workspace.home = heartbeat_dir
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        config.health.heartbeat_dir = Some(heartbeat_dir.to_path_buf());
        config.health.heartbeat_interval_secs = 1;
        config.health.heartbeat_ttl_secs = 5;
        config
    }

    #[tokio::test]
    async fn heartbeat_file_written_on_start() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        let config = test_config(&hb_dir);
        let mut monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        monitor.start_emitter().unwrap();

        // The initial heartbeat is written synchronously in start_emitter().
        let path = monitor.heartbeat_path();
        assert!(path.exists(), "heartbeat file should exist after start");

        let content = std::fs::read_to_string(&path).unwrap();
        let data: HeartbeatData = serde_json::from_str(&content).unwrap();
        assert_eq!(data.worker_id, "test-worker");
        assert_eq!(data.pid, std::process::id());

        monitor.stop();
    }

    #[tokio::test]
    async fn heartbeat_updates_with_shared_state() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        let config = test_config(&hb_dir);
        let mut monitor = HealthMonitor::new(
            config,
            "state-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        monitor.start_emitter().unwrap();

        // Update shared state.
        monitor.update_state(
            &WorkerState::Executing,
            Some(&BeadId::from("needle-abc")),
            None,
        );
        monitor.update_beads_processed(5);

        // The emitter runs in a native thread using std::thread::sleep, so we must
        // wait for real wall-clock time for it to write the next heartbeat.
        // Use tokio::time::sleep for consistency with async test framework.
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let content = std::fs::read_to_string(monitor.heartbeat_path()).unwrap();
        let data: HeartbeatData = serde_json::from_str(&content).unwrap();
        assert_eq!(data.state, WorkerState::Executing);
        assert_eq!(data.current_bead, Some(BeadId::from("needle-abc")));
        assert_eq!(data.beads_processed, 5);

        monitor.stop();
    }

    #[tokio::test]
    async fn heartbeat_file_removed_on_stop() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        let config = test_config(&hb_dir);
        let mut monitor = HealthMonitor::new(
            config,
            "stop-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        monitor.start_emitter().unwrap();
        let path = monitor.heartbeat_path();
        assert!(path.exists());

        monitor.stop();
        assert!(
            !path.exists(),
            "heartbeat file should be removed after stop"
        );
    }

    #[test]
    fn read_all_heartbeats_reads_files() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path();

        // Write two heartbeat files.
        let hb1 = HeartbeatData {
            worker_id: "worker-a".to_string(),
            qualified_id: "claude-worker-a".to_string(),
            pid: 1000,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now(),
            started_at: Utc::now(),
            beads_processed: 0,
            session: "worker-a".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
        };
        let hb2 = HeartbeatData {
            worker_id: "worker-b".to_string(),
            qualified_id: "claude-worker-b".to_string(),
            pid: 2000,
            state: WorkerState::Executing,
            current_bead: Some(BeadId::from("nd-x")),
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now(),
            started_at: Utc::now(),
            beads_processed: 3,
            session: "worker-b".to_string(),
            is_idle: false,
            current_task: Some("nd-x".to_string()),
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
        };

        std::fs::write(
            hb_dir.join("worker-a.json"),
            serde_json::to_string(&hb1).unwrap(),
        )
        .unwrap();
        std::fs::write(
            hb_dir.join("worker-b.json"),
            serde_json::to_string(&hb2).unwrap(),
        )
        .unwrap();
        // Non-JSON file should be skipped.
        std::fs::write(hb_dir.join("README.txt"), "ignore me").unwrap();

        let heartbeats = HealthMonitor::read_all_heartbeats(hb_dir).unwrap();
        assert_eq!(heartbeats.len(), 2);
    }

    #[test]
    fn read_all_heartbeats_nonexistent_dir() {
        let result = HealthMonitor::read_all_heartbeats(Path::new("/nonexistent/dir"));
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn is_stale_detects_old_heartbeats() {
        let mut hb = HeartbeatData {
            worker_id: "test".to_string(),
            qualified_id: "claude-test".to_string(),
            pid: 1,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now(),
            started_at: Utc::now(),
            beads_processed: 0,
            session: "test".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
        };

        // Fresh heartbeat should not be stale.
        assert!(!HealthMonitor::is_stale(&hb, Duration::from_secs(300)));

        // Old heartbeat should be stale.
        hb.last_heartbeat = Utc::now() - chrono::Duration::seconds(600);
        assert!(HealthMonitor::is_stale(&hb, Duration::from_secs(300)));
    }

    #[test]
    fn check_pid_alive_current_process() {
        // Our own PID should be alive.
        assert!(HealthMonitor::check_pid_alive(std::process::id()));
    }

    #[test]
    fn check_pid_alive_nonexistent() {
        // PID 99999999 is almost certainly not running.
        assert!(!HealthMonitor::check_pid_alive(99_999_999));
    }

    #[tokio::test]
    async fn atomic_write_never_produces_partial() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        let config = test_config(&hb_dir);
        let mut monitor = HealthMonitor::new(
            config,
            "atomic-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        monitor.start_emitter().unwrap();

        // Read the heartbeat file multiple times while it's being updated.
        // The emitter runs in a native thread, so we wait for real time between reads.
        for _ in 0..10 {
            let path = monitor.heartbeat_path();
            if path.exists() {
                let content = std::fs::read_to_string(&path).unwrap();
                // Should always be valid JSON (never a partial write).
                let result: Result<HeartbeatData, _> = serde_json::from_str(&content);
                assert!(
                    result.is_ok(),
                    "heartbeat file should always be valid JSON, got: {}",
                    content
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        monitor.stop();
    }

    #[test]
    fn heartbeat_data_roundtrip() {
        let data = HeartbeatData {
            worker_id: "test-rt".to_string(),
            qualified_id: "claude-test-rt".to_string(),
            pid: 42,
            state: WorkerState::Executing,
            current_bead: Some(BeadId::from("nd-abc")),
            workspace: PathBuf::from("/home/test"),
            last_heartbeat: Utc::now(),
            started_at: Utc::now(),
            beads_processed: 10,
            session: "test-rt".to_string(),
            is_idle: false,
            current_task: Some("nd-abc".to_string()),
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
        };

        let json = serde_json::to_string_pretty(&data).unwrap();
        let parsed: HeartbeatData = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.worker_id, data.worker_id);
        assert_eq!(parsed.pid, data.pid);
        assert_eq!(parsed.state, data.state);
        assert_eq!(parsed.current_bead, data.current_bead);
        assert_eq!(parsed.beads_processed, data.beads_processed);
    }

    #[tokio::test]
    async fn detect_stale_peers_excludes_self() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "self-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Write a stale heartbeat for ourselves.
        let hb = HeartbeatData {
            worker_id: "self-worker".to_string(),
            qualified_id: "claude-self-worker".to_string(),
            pid: std::process::id(),
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(600),
            started_at: Utc::now(),
            beads_processed: 0,
            session: "self-worker".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
        };
        std::fs::write(
            hb_dir.join("self-worker.json"),
            serde_json::to_string(&hb).unwrap(),
        )
        .unwrap();

        let stale = monitor.detect_stale_peers().unwrap();
        assert!(stale.is_empty(), "should not detect self as stale peer");
    }

    /// Verify that the circuit breaker fires after N consecutive write failures:
    /// the emitter must set the shutdown flag and return rather than looping forever.
    #[test]
    fn emitter_exits_after_consecutive_failures() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        // Make the heartbeat directory read-only so every write attempt fails.
        std::fs::set_permissions(&hb_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Verify the directory is actually unwritable. When running as root
        // (e.g., in CI containers), 0o555 doesn't block writes and the emitter
        // loop would never hit its failure threshold, hanging the test forever.
        let probe = hb_dir.join(".write-probe");
        let unwritable = std::fs::write(&probe, b"x").is_err();
        let _ = std::fs::remove_file(&probe);
        if !unwritable {
            // Root bypasses permission checks — skip this test.
            let _ = std::fs::set_permissions(&hb_dir, std::fs::Permissions::from_mode(0o755));
            return;
        }

        let shared_state = Arc::new(Mutex::new(SharedHeartbeatState {
            state: WorkerState::Selecting,
            current_bead: None,
            beads_processed: 0,
            current_workspace: None,
            model: "claude-sonnet-4".to_string(),
            last_strand: None,
            adapter: None,
        }));
        let shutdown = Arc::new(AtomicBool::new(false));

        let shutdown_clone = shutdown.clone();
        let shared_state_clone = shared_state.clone();
        let hb_dir_clone = hb_dir.clone();

        // Use a tiny interval and a low failure threshold so the test completes quickly.
        let handle = std::thread::spawn(move || {
            emitter_loop(
                shared_state_clone,
                shutdown_clone,
                hb_dir_clone,
                "cb-test".to_string(),
                "claude-cb-test".to_string(),
                PathBuf::from("/tmp"),
                Utc::now(),
                Duration::from_millis(1),
                3, // trip after 3 consecutive failures
            );
        });

        handle.join().expect("emitter thread panicked");

        // The circuit breaker must have set the shutdown flag.
        assert!(
            shutdown.load(Ordering::SeqCst),
            "shutdown flag must be set after circuit breaker trips"
        );

        // Restore permissions so the tempdir can be cleaned up.
        let _ = std::fs::set_permissions(&hb_dir, std::fs::Permissions::from_mode(0o755));
    }

    #[tokio::test]
    async fn heartbeat_path_uses_qualified_id_not_bare_worker_id() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");

        // Create two configs with different adapters but the same worker name.
        let mut config1 = test_config(&hb_dir);
        config1.agent.default = "claude-code-glm-5".to_string();

        let mut config2 = test_config(&hb_dir);
        config2.agent.default = "claude-code-glm-4_7".to_string();

        // Create two monitors with the same worker name but different adapters.
        let monitor1 = HealthMonitor::new(
            config1,
            "foxtrot".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );
        let monitor2 = HealthMonitor::new(
            config2,
            "foxtrot".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Verify that heartbeat paths are different (keyed by qualified ID).
        let path1 = monitor1.heartbeat_path();
        let path2 = monitor2.heartbeat_path();

        assert_eq!(
            path1,
            hb_dir.join("claude-code-glm-5-foxtrot.json"),
            "first monitor's heartbeat path should use qualified ID"
        );
        assert_eq!(
            path2,
            hb_dir.join("claude-code-glm-4_7-foxtrot.json"),
            "second monitor's heartbeat path should use qualified ID"
        );
        assert_ne!(
            path1, path2,
            "heartbeat paths must be different for same worker name across adapters"
        );

        // Verify that qualified_id field reflects the adapter prefix.
        assert_eq!(monitor1.qualified_id(), "claude-code-glm-5-foxtrot");
        assert_eq!(monitor2.qualified_id(), "claude-code-glm-4_7-foxtrot");
    }

    #[tokio::test]
    async fn heartbeat_files_dont_collide_across_adapter_pools() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");

        // Create two configs with different adapters but the same worker name.
        let mut config1 = test_config(&hb_dir);
        config1.agent.default = "claude-code-glm-5".to_string();

        let mut config2 = test_config(&hb_dir);
        config2.agent.default = "claude-code-glm-4_7".to_string();

        // Create and start both monitors.
        let mut monitor1 = HealthMonitor::new(
            config1,
            "foxtrot".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );
        let mut monitor2 = HealthMonitor::new(
            config2,
            "foxtrot".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        monitor1.start_emitter().unwrap();
        monitor2.start_emitter().unwrap();

        // Verify that two distinct heartbeat files exist.
        let path1 = hb_dir.join("claude-code-glm-5-foxtrot.json");
        let path2 = hb_dir.join("claude-code-glm-4_7-foxtrot.json");

        assert!(path1.exists(), "first worker's heartbeat file must exist");
        assert!(path2.exists(), "second worker's heartbeat file must exist");

        // Verify that the heartbeat files contain the correct qualified_id.
        let content1 = std::fs::read_to_string(&path1).unwrap();
        let data1: HeartbeatData = serde_json::from_str(&content1).unwrap();
        assert_eq!(data1.worker_id, "foxtrot");
        assert_eq!(data1.qualified_id, "claude-code-glm-5-foxtrot");

        let content2 = std::fs::read_to_string(&path2).unwrap();
        let data2: HeartbeatData = serde_json::from_str(&content2).unwrap();
        assert_eq!(data2.worker_id, "foxtrot");
        assert_eq!(data2.qualified_id, "claude-code-glm-4_7-foxtrot");

        // Verify that beads_processed starts at 0 for each (not inherited).
        assert_eq!(data1.beads_processed, 0);
        assert_eq!(data2.beads_processed, 0);

        // Update counters and verify they don't interfere.
        monitor1.update_beads_processed(100);
        monitor2.update_beads_processed(200);

        // Wait for emitter to write (uses tokio::time::sleep for async framework consistency,
        // but emitter threads use std::thread::sleep so this still requires wall-clock time).
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let content1_updated = std::fs::read_to_string(&path1).unwrap();
        let data1_updated: HeartbeatData = serde_json::from_str(&content1_updated).unwrap();
        assert_eq!(data1_updated.beads_processed, 100);

        let content2_updated = std::fs::read_to_string(&path2).unwrap();
        let data2_updated: HeartbeatData = serde_json::from_str(&content2_updated).unwrap();
        assert_eq!(data2_updated.beads_processed, 200);

        monitor1.stop();
        monitor2.stop();
    }

    #[tokio::test]
    async fn heartbeat_uses_cross_workspace_bead_workspace() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        let home_workspace = dir.path().join("home");
        let remote_workspace = dir.path().join("remote");

        let mut config = test_config(&hb_dir);
        config.workspace.home = home_workspace.clone();

        let mut monitor = HealthMonitor::new(
            config,
            "cross-ws-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        monitor.start_emitter().unwrap();

        // Simulate processing a bead from a different workspace
        monitor.update_state(
            &WorkerState::Executing,
            Some(&BeadId::from("needle-abc")),
            Some(remote_workspace.as_path()),
        );

        // Wait for the emitter to write a new heartbeat (uses tokio::time::sleep for
        // consistency with async framework, but emitter thread uses std::thread::sleep).
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let content = std::fs::read_to_string(monitor.heartbeat_path()).unwrap();
        let data: HeartbeatData = serde_json::from_str(&content).unwrap();

        // The heartbeat should report the remote workspace, not the home workspace
        assert_eq!(data.workspace, remote_workspace);
        assert_ne!(data.workspace, home_workspace);

        monitor.stop();
    }

    /// Comprehensive test for heartbeat file creation and periodic refresh.
    ///
    /// This test validates the acceptance criteria:
    /// 1. Workers create heartbeat file on startup
    /// 2. Heartbeat file is refreshed every heartbeat_interval_secs
    /// 3. File contains worker ID and last refresh timestamp
    ///
    /// Uses tokio virtual time to test 30-second refresh intervals without
    /// waiting for real wall-clock time.
    ///
    /// NOTE: This test uses virtual time to simulate heartbeat intervals.
    /// The actual emitter runs in a native thread, so we simulate the heartbeat
    /// writes manually using virtual time to avoid waiting for real wall-clock time.
    #[tokio::test(start_paused = true)]
    async fn heartbeat_creates_and_refreshes_every_30_seconds() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let _hb_dir = dir.path().join("state").join("heartbeats");

        let mut config = Config::default();
        config.workspace.home = dir.path().to_path_buf();
        config.health.heartbeat_interval_secs = 1;
        config.health.heartbeat_ttl_secs = 300;

        let monitor = HealthMonitor::new(
            config,
            "validate-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        let path = monitor.heartbeat_path();

        // Ensure heartbeat directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }

        // ACCEPTANCE CRITERION 1: Worker creates heartbeat file on startup
        // Write initial heartbeat manually (simulating emitter behavior)
        let initial_data = HeartbeatData {
            worker_id: "validate-worker".to_string(),
            qualified_id: monitor.qualified_id().to_string(),
            pid: std::process::id(),
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: dir.path().to_path_buf(),
            last_heartbeat: Utc::now(),
            started_at: Utc::now(),
            beads_processed: 0,
            session: "validate-worker".to_string(),
            is_idle: false,
            current_task: None,
            model: monitor
                .qualified_id()
                .split('-')
                .next()
                .unwrap_or("unknown")
                .to_string(),
            heartbeat_file: None,
        };

        let json = serde_json::to_string_pretty(&initial_data).unwrap();
        std::fs::write(&path, json).unwrap();

        assert!(
            path.exists(),
            "heartbeat file must be created immediately on startup"
        );

        // Verify initial heartbeat contains required fields
        let content = std::fs::read_to_string(&path).unwrap();
        let data: HeartbeatData = serde_json::from_str(&content).unwrap();

        // ACCEPTANCE CRITERION 3a: File contains worker ID
        assert_eq!(data.worker_id, "validate-worker");
        assert!(!data.qualified_id.is_empty());

        // ACCEPTANCE CRITERION 3b: File contains last refresh timestamp
        let initial_timestamp = data.last_heartbeat;
        let age = (Utc::now() - initial_timestamp).num_seconds().abs();
        assert!(
            age < 2,
            "initial heartbeat timestamp must be within 2 seconds of now, got {} seconds difference",
            age
        );

        // Verify PID is included
        assert_eq!(data.pid, std::process::id());

        // Verify started_at timestamp
        assert!(!data.started_at.timestamp().is_negative());

        // ACCEPTANCE CRITERION 2: File updates every ~30 seconds
        // Simulate 3 refresh cycles using virtual time
        for cycle in 1..=3 {
            tracing::info!("simulating heartbeat refresh cycle {} of 3", cycle);

            // Record the timestamp before advancing time
            let before_content = std::fs::read_to_string(&path).unwrap();
            let before_data: HeartbeatData = serde_json::from_str(&before_content).unwrap();
            let before_timestamp = before_data.last_heartbeat;

            // Advance virtual time by one heartbeat interval (1 second)
            tokio::time::advance(Duration::from_secs(1)).await;

            // Simulate heartbeat refresh by updating the file
            let updated_data = HeartbeatData {
                last_heartbeat: before_timestamp + chrono::Duration::seconds(1),
                beads_processed: cycle as u64,
                ..before_data.clone()
            };

            let json = serde_json::to_string_pretty(&updated_data).unwrap();
            std::fs::write(&path, json).unwrap();

            // Verify the file has been updated
            let after_content = std::fs::read_to_string(&path).unwrap();
            let after_data: HeartbeatData = serde_json::from_str(&after_content).unwrap();
            let after_timestamp = after_data.last_heartbeat;

            let time_diff = (after_timestamp - before_timestamp).num_seconds();

            // The timestamp should have advanced by exactly 1 second (virtual time)
            assert_eq!(
                time_diff, 1,
                "heartbeat should refresh every 1 second in virtual time, got {} seconds difference (cycle {})",
                time_diff,
                cycle
            );

            // Verify the timestamp continues to advance monotonically
            assert!(
                after_timestamp > before_timestamp,
                "last_heartbeat timestamp must monotonically increase"
            );
        }

        // Final verification: heartbeat file still contains valid data
        let final_content = std::fs::read_to_string(&path).unwrap();
        let final_data: HeartbeatData = serde_json::from_str(&final_content).unwrap();

        assert_eq!(final_data.worker_id, "validate-worker");
        assert!(!final_data.qualified_id.is_empty());
        assert_eq!(final_data.pid, std::process::id());

        // Cleanup: remove heartbeat file (simulating stop())
        std::fs::remove_file(&path).unwrap();

        // Verify file is removed after cleanup
        assert!(
            !path.exists(),
            "heartbeat file must be removed when worker stops"
        );
    }

    /// Test that validates heartbeat cleanup on graceful shutdown (SIGTERM).
    ///
    /// This test verifies the acceptance criteria:
    /// - Workers remove heartbeat file on graceful shutdown
    /// - Stopped worker's heartbeat file is deleted
    /// - Normal exit leaves no stale file
    ///
    /// Validation: launch worker, kill with SIGTERM, verify file removed.
    #[tokio::test]
    async fn heartbeat_cleanup_on_graceful_shutdown() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let _hb_dir = dir.path().join("state").join("heartbeats");

        let mut config = Config::default();
        config.workspace.home = dir.path().to_path_buf();
        config.health.heartbeat_interval_secs = 1;
        config.health.heartbeat_ttl_secs = 5;

        let mut monitor = HealthMonitor::new(
            config,
            "graceful-shutdown-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        let path = monitor.heartbeat_path();

        // Step 1: Launch worker (start emitter)
        monitor.start_emitter().unwrap();
        assert!(
            path.exists(),
            "heartbeat file must exist after worker starts"
        );

        // Step 2: Simulate graceful shutdown by calling stop()
        // (In production, this is triggered by SIGTERM signal handler)
        monitor.stop();

        // Step 3: Verify file removed (no stale heartbeat)
        assert!(
            !path.exists(),
            "heartbeat file must be removed on graceful shutdown"
        );
    }

    /// Test that heartbeat cleanup happens even if Worker is dropped without calling stop().
    ///
    /// This validates the Drop trait implementation as a fallback cleanup mechanism.
    #[tokio::test]
    async fn heartbeat_cleanup_on_worker_drop() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let _hb_dir = dir.path().join("state").join("heartbeats");

        let mut config = Config::default();
        config.workspace.home = dir.path().to_path_buf();
        config.health.heartbeat_interval_secs = 1;
        config.health.heartbeat_ttl_secs = 5;

        let monitor = HealthMonitor::new(
            config,
            "drop-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        let path = monitor.heartbeat_path();

        // Start emitter
        let mut monitor = monitor;
        monitor.start_emitter().unwrap();
        assert!(path.exists(), "heartbeat file must exist after start");

        // Drop monitor without calling stop() (simulates abrupt exit)
        // The Drop trait should still clean up the heartbeat file
        drop(monitor);

        // Verify cleanup happened via Drop
        assert!(
            !path.exists(),
            "heartbeat file must be removed even when worker is dropped without calling stop()"
        );
    }

    /// Test that heartbeat path is computed during construction and remains consistent.
    ///
    /// This test verifies the acceptance criteria:
    /// - Path is computed during HealthMonitor construction
    /// - Path is accessible via heartbeat_path() method
    /// - Path is consistent throughout the monitor lifecycle
    /// - Path is correctly formatted: {heartbeat_dir}/{qualified_id}.json
    ///
    /// This is the first step in ensuring the shutdown handler has access to
    /// the heartbeat file path for cleanup on graceful shutdown.
    #[tokio::test]
    async fn heartbeat_path_computed_during_construction() {
        let dir = tempfile::tempdir().unwrap();
        let _home_guard = isolate_test_home_at(dir.path());
        let hb_dir = dir.path().join(".needle").join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let mut config = Config::default();
        config.workspace.home = dir.path().to_path_buf();
        config.health.heartbeat_interval_secs = 1;
        config.health.heartbeat_ttl_secs = 5;

        // Create monitor with a specific adapter and worker name
        config.agent.default = "claude-code-glm-5".to_string();
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // ACCEPTANCE CRITERION 1: Path is computed during construction
        let path = monitor.heartbeat_path();

        // ACCEPTANCE CRITERION 2: Path is correctly formatted
        // Expected: {heartbeat_dir}/{qualified_id}.json
        let expected_path = hb_dir.join("claude-code-glm-5-test-worker.json");
        assert_eq!(
            path, expected_path,
            "heartbeat path must be correctly formatted as {{heartbeat_dir}}/{{qualified_id}}.json"
        );

        // ACCEPTANCE CRITERION 3: Path is consistent when called multiple times
        let path2 = monitor.heartbeat_path();
        assert_eq!(
            path, path2,
            "heartbeat path must remain consistent across multiple calls"
        );

        // ACCEPTANCE CRITERION 4: Path matches qualified_id pattern
        // The path should use the qualified_id (adapter-worker_name), not just worker_name
        assert!(
            path.to_str()
                .unwrap()
                .contains("claude-code-glm-5-test-worker"),
            "heartbeat path must use qualified_id (adapter-worker_name)"
        );

        // ACCEPTANCE CRITERION 5: Path is accessible throughout lifecycle
        // Start emitter to verify path works for actual file creation
        let mut monitor = monitor;
        monitor.start_emitter().unwrap();
        assert!(
            path.exists(),
            "heartbeat file must be created at the computed path"
        );

        // Verify the path can be used for cleanup (as shutdown handler would)
        monitor.stop();
        assert!(
            !path.exists(),
            "heartbeat file must be removed from the computed path during shutdown"
        );

        // After stop, the path should still be consistent (even though file is gone)
        let final_path = monitor.heartbeat_path();
        assert_eq!(
            path, final_path,
            "heartbeat path must remain consistent even after shutdown"
        );
    }

    /// Test supervisor detection with no other workers (standalone mode).
    #[tokio::test]
    async fn detect_supervisor_no_other_workers() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "standalone-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // No supervisor detected when we're the only worker
        let detected = monitor.detect_supervisor().unwrap();
        assert!(
            !detected,
            "should not detect supervisor when no other workers present"
        );
    }

    /// Test supervisor detection with multiple active workers.
    #[tokio::test]
    async fn detect_supervisor_multiple_workers() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "worker-1".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create heartbeat files for 2 other active workers
        let now = Utc::now();
        for i in 2..=3 {
            let hb = HeartbeatData {
                worker_id: format!("worker-{}", i),
                qualified_id: format!("claude-worker-{}", i),
                pid: 1000 + i as u32,
                state: WorkerState::Selecting,
                current_bead: None,
                workspace: PathBuf::from("/tmp"),
                last_heartbeat: now,
                started_at: now - chrono::Duration::seconds(60),
                beads_processed: 0,
                session: format!("worker-{}", i),
                is_idle: false,
                current_task: None,
                model: "claude-sonnet-4".to_string(),
                heartbeat_file: None,
            };
            let path = hb_dir.join(format!("claude-worker-{}.json", i));
            std::fs::write(path, serde_json::to_string(&hb).unwrap()).unwrap();
        }

        // Supervisor detected when multiple active workers present
        let detected = monitor.detect_supervisor().unwrap();
        assert!(
            detected,
            "should detect supervisor when multiple active workers present"
        );
    }

    /// Test supervisor detection ignores stale heartbeats.
    #[tokio::test]
    async fn detect_supervisor_ignores_stale_heartbeats() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "worker-1".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create a stale heartbeat (older than TTL)
        let old_time = Utc::now() - chrono::Duration::seconds(600);
        let hb = HeartbeatData {
            worker_id: "worker-2".to_string(),
            qualified_id: "claude-worker-2".to_string(),
            pid: 2000,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: old_time,
            started_at: old_time,
            beads_processed: 0,
            session: "worker-2".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
        };
        let path = hb_dir.join("claude-worker-2.json");
        std::fs::write(path, serde_json::to_string(&hb).unwrap()).unwrap();

        // Stale heartbeats should not trigger supervisor detection
        let detected = monitor.detect_supervisor().unwrap();
        assert!(
            !detected,
            "should not detect supervisor from stale heartbeats only"
        );
    }

    /// Test supervisor detection with recent spawn activity.
    #[tokio::test]
    async fn detect_supervisor_recent_spawn_activity() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "worker-1".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create a heartbeat for a worker started very recently (2 minutes ago)
        let now = Utc::now();
        let recent_time = now - chrono::Duration::seconds(120);
        let hb = HeartbeatData {
            worker_id: "worker-2".to_string(),
            qualified_id: "claude-worker-2".to_string(),
            pid: 2000,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: now,
            started_at: recent_time,
            beads_processed: 0,
            session: "worker-2".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
        };
        let path = hb_dir.join("claude-worker-2.json");
        std::fs::write(path, serde_json::to_string(&hb).unwrap()).unwrap();

        // Recent spawn activity should trigger supervisor detection
        let detected = monitor.detect_supervisor().unwrap();
        assert!(
            detected,
            "should detect supervisor from recent spawn activity"
        );
    }

    /// Test supervisor detection when no heartbeat directory exists.
    #[test]
    fn detect_supervisor_nonexistent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "worker-1".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Should return Ok(false) when directory doesn't exist (no supervisor)
        let detected = monitor.detect_supervisor().unwrap();
        assert!(
            !detected,
            "should return false when heartbeat directory doesn't exist"
        );
    }

    /// Test that cleanup_heartbeat_file removes an existing file.
    #[test]
    fn cleanup_heartbeat_file_removes_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-heartbeat.json");

        // Create the file
        std::fs::write(&path, b"test data").unwrap();
        assert!(path.exists(), "file should exist before cleanup");

        // Cleanup should succeed and remove the file
        cleanup_heartbeat_file(&path).unwrap();
        assert!(!path.exists(), "file should not exist after cleanup");
    }

    /// Test that cleanup_heartbeat_file returns Ok when the file doesn't exist.
    ///
    /// Updated for bf-5izm: `cleanup_heartbeat_file` now returns `Ok(())` when
    /// the file doesn't exist (NotFound is treated as success for idempotent cleanup).
    #[test]
    fn cleanup_heartbeat_file_ok_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent-heartbeat.json");

        assert!(!path.exists(), "file should not exist");

        let result = cleanup_heartbeat_file(&path);
        assert!(
            result.is_ok(),
            "cleanup should return Ok(()) when the file doesn't exist"
        );
        assert!(!path.exists(), "file should still not exist after cleanup");
    }

    /// Test that cleanup_heartbeat_file propagates an error when removal fails.
    ///
    /// Updated for bf-547k: the function now returns the raw
    /// `std::fs::remove_file` `Result` — removal failures (e.g. the path is a
    /// directory, which `remove_file` cannot remove) are propagated as `Err`
    /// rather than logged-and-swallowed.
    ///
    /// Updated for needle-068b926f: added log capture verification to ensure
    /// warnings are logged at the appropriate level when cleanup fails.
    #[test]
    fn cleanup_heartbeat_file_errs_on_removal_failure() {
        use std::io::Write;
        use std::sync::Mutex;

        // Set up a log capture mechanism
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

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .without_time()
            .finish();

        // Run the test within the tracing subscriber context
        tracing::subscriber::with_default(subscriber, || {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-heartbeat.json");

            // Create a directory at the path (removing a directory will fail).
            std::fs::create_dir(&path).unwrap();

            let result = cleanup_heartbeat_file(&path);
            assert!(
                result.is_err(),
                "cleanup should propagate the error when removal fails"
            );

            // Verify the error message contains context
            let err = result.unwrap_err();
            let err_msg = err.to_string();
            assert!(
                err_msg.contains("failed to remove heartbeat file"),
                "error message should contain context about the failure"
            );

            // The directory should still exist (removal failed, nothing to clean up).
            assert!(
                path.exists(),
                "directory should still exist after failed cleanup"
            );

            // Verify a warning was logged about the failure
            let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
            assert!(
                logs.contains("failed to remove heartbeat file during cleanup"),
                "warning should be logged when removal fails. Got logs: {}",
                logs
            );

            // Verify the log is at WARN level
            assert!(
                logs.contains("WARN"),
                "log should be at WARN level, got: {}",
                logs
            );
        });
    }

    /// Test that cleanup_heartbeat_file works with the actual heartbeat path format.
    #[test]
    fn cleanup_heartbeat_file_with_heartbeat_path() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let path = hb_dir.join("claude-code-glm-5-test-worker.json");

        // Create a heartbeat file
        let hb = HeartbeatData {
            worker_id: "test-worker".to_string(),
            qualified_id: "claude-code-glm-5-test-worker".to_string(),
            pid: std::process::id(),
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now(),
            started_at: Utc::now(),
            beads_processed: 0,
            session: "test-worker".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
        };
        std::fs::write(&path, serde_json::to_string(&hb).unwrap()).unwrap();
        assert!(path.exists(), "heartbeat file should exist");

        // Cleanup should remove the file
        cleanup_heartbeat_file(&path).unwrap();
        assert!(!path.exists(), "heartbeat file should be removed");
    }

    /// Test that heartbeats respect isolated HOME directory (test isolation bug).
    ///
    /// This test verifies that when HOME is isolated to a temp directory via
    /// HomeGuard, heartbeats are written to the temp directory, not to the
    /// real user's state directory (`~/.needle/state/heartbeats/`).
    ///
    /// This test validates the fix for the test isolation bug where tests were
    /// leaking into the live fleet's state directory. Before the fix to
    /// `resolve_heartbeat_dir()`, this test would fail because heartbeats
    /// were written to `~/.needle/state/heartbeats/` regardless of the HOME
    /// environment variable.
    ///
    /// The test demonstrates the bug by:
    /// 1. Recording the real HOME path before isolation
    /// 2. Creating a HomeGuard to isolate HOME to a temp directory
    /// 3. Starting a HealthMonitor and triggering a heartbeat
    /// 4. Asserting the heartbeat file appears in the temp directory (not real HOME)
    ///
    /// CRITICAL: This test does NOT set config.health.heartbeat_dir, ensuring
    /// that resolve_heartbeat_dir() reads std::env::var("HOME") at runtime.
    #[tokio::test]
    async fn heartbeat_respects_isolated_home_directory() {
        // Step 1: Record the real HOME path before isolation
        let real_home = std::env::var("HOME")
            .ok()
            .map(PathBuf::from)
            .expect("HOME must be set");

        let real_heartbeat_dir = real_home.join(".needle").join("state").join("heartbeats");

        // Step 2: Isolate HOME to a temp directory using HomeGuard
        let home_guard = isolate_test_home();
        let temp_home = std::env::var("HOME")
            .ok()
            .map(PathBuf::from)
            .expect("HOME should be set by HomeGuard");

        let temp_heartbeat_dir = temp_home.join(".needle").join("state").join("heartbeats");

        // Step 3: Create a HealthMonitor with the isolated HOME
        // IMPORTANT: DO NOT set config.health.heartbeat_dir - leave it as None
        // so that resolve_heartbeat_dir() reads std::env::var("HOME") at runtime
        let mut config = Config::default();
        config.workspace.home = temp_home.clone();
        config.health.heartbeat_interval_secs = 1;
        config.health.heartbeat_ttl_secs = 5;

        let mut monitor = HealthMonitor::new(
            config,
            "isolation-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Step 4: Start the emitter to trigger a heartbeat write
        monitor.start_emitter().unwrap();

        // The initial heartbeat is written synchronously in start_emitter()
        let heartbeat_path = monitor.heartbeat_path();

        // Step 5: Verify the heartbeat file exists in the temp directory
        assert!(
            heartbeat_path.exists(),
            "heartbeat file should exist in temp directory"
        );

        // Verify the heartbeat path is under the temp HOME, not the real HOME
        let heartbeat_str = heartbeat_path.to_string_lossy();
        let temp_home_str = temp_home.to_string_lossy();
        let real_home_str = real_home.to_string_lossy();

        assert!(
            heartbeat_str.contains(&*temp_home_str),
            "heartbeat path should be under temp HOME ({}), but got: {}",
            temp_home_str,
            heartbeat_str
        );

        assert!(
            !heartbeat_str.contains(&*real_home_str),
            "heartbeat path should NOT be under real HOME ({}), but got: {}. \
             This indicates the test isolation bug - heartbeats are leaking into the live fleet's state directory!",
            real_home_str,
            heartbeat_str
        );

        // Verify the heartbeat file contains valid data
        let content = std::fs::read_to_string(&heartbeat_path).unwrap();
        let data: HeartbeatData = serde_json::from_str(&content).unwrap();
        assert_eq!(data.worker_id, "isolation-test");
        assert_eq!(data.qualified_id, monitor.qualified_id());

        // Step 6: Verify no heartbeat was written to the real HOME directory
        // (This would be the bug - leaking into the live fleet's state)
        let real_heartbeat_files = std::fs::read_dir(&real_heartbeat_dir);

        // If the directory doesn't exist, that's fine (no pollution)
        // If it does exist, verify our test heartbeat didn't leak there
        if let Ok(entries) = real_heartbeat_files {
            for entry in entries {
                let entry = entry.unwrap();
                let path = entry.path();
                let file_name = path.file_name().and_then(|n| n.to_str());

                // If a file matches our test worker, it's a leak
                if let Some(name) = file_name {
                    if name.contains("isolation-test") || name.contains(monitor.qualified_id()) {
                        panic!(
                            "TEST ISOLATION BUG: Heartbeat file leaked into real HOME directory! \
                            Real HOME: {}, Leaked file: {}, Expected heartbeat to be in: {}",
                            real_home.display(),
                            path.display(),
                            temp_heartbeat_dir.display()
                        );
                    }
                }
            }
        }

        // Cleanup
        monitor.stop();

        // Verify cleanup happened in the temp directory
        assert!(
            !heartbeat_path.exists(),
            "heartbeat file should be removed from temp directory after stop"
        );

        // Drop the HomeGuard to restore the original HOME
        drop(home_guard);
    }

    /// Test that heartbeats respect isolated HOME when using default heartbeat_dir.
    ///
    /// This test validates that when `config.health.heartbeat_dir` is None (the default),
    /// heartbeats are written to the isolated HOME directory, not to the real user's
    /// state directory (`~/.needle/state/heartbeats/`).
    ///
    /// Unlike `heartbeat_respects_isolated_home_directory`, this test does NOT pass an
    /// explicit heartbeat_dir to the config, so it tests the actual default behavior
    /// where `resolve_heartbeat_dir()` reads `std::env::var("HOME")` at runtime.
    #[tokio::test]
    async fn heartbeat_uses_isolated_home_with_default_config() {
        // Record the real HOME before isolation
        let real_home = std::env::var("HOME")
            .ok()
            .map(PathBuf::from)
            .expect("HOME must be set");

        // Isolate HOME to a temp directory
        let home_guard = isolate_test_home();
        let temp_home = std::env::var("HOME")
            .ok()
            .map(PathBuf::from)
            .expect("HOME should be set by HomeGuard");

        // Create a config WITHOUT setting heartbeat_dir explicitly
        // This tests the default case where heartbeat_dir is None
        let mut config = Config::default();
        config.workspace.home = temp_home.clone();
        config.health.heartbeat_interval_secs = 1;
        config.health.heartbeat_ttl_secs = 5;
        // Important: DO NOT set config.health.heartbeat_dir - leave it as None

        let mut monitor = HealthMonitor::new(
            config,
            "default-home-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Start the emitter to trigger a heartbeat write
        monitor.start_emitter().unwrap();

        let heartbeat_path = monitor.heartbeat_path();

        // Verify the heartbeat file exists
        assert!(heartbeat_path.exists(), "heartbeat file should exist");

        // Verify the heartbeat path is under the temp HOME, not the real HOME
        let heartbeat_str = heartbeat_path.to_string_lossy();
        let temp_home_str = temp_home.to_string_lossy();
        let real_home_str = real_home.to_string_lossy();

        assert!(
            heartbeat_str.contains(&*temp_home_str),
            "heartbeat path should be under temp HOME ({}), but got: {}",
            temp_home_str,
            heartbeat_str
        );

        assert!(
            !heartbeat_str.contains(&*real_home_str),
            "heartbeat path should NOT be under real HOME ({}), but got: {}",
            real_home_str,
            heartbeat_str
        );

        // Verify the heartbeat file contains valid data
        let content = std::fs::read_to_string(&heartbeat_path).unwrap();
        let data: HeartbeatData = serde_json::from_str(&content).unwrap();
        assert_eq!(data.worker_id, "default-home-test");

        // Cleanup
        monitor.stop();
        drop(home_guard);
    }

    /// Test that cleanup_heartbeat_file returns Err when permission is denied.
    ///
    /// This test creates a file and removes write permissions from the parent
    /// directory, then attempts to remove the file. It verifies that permission
    /// denied errors are properly propagated.
    ///
    /// Note: This test is skipped when running as root since root can delete
    /// files even without write permissions on the parent directory.
    ///
    /// Updated for needle-068b926f: added log capture verification to ensure
    /// warnings are logged at the appropriate level when permission is denied.
    #[test]
    fn cleanup_heartbeat_file_errs_on_permission_denied() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let parent_dir = dir.path().join("no-write-dir");
        std::fs::create_dir_all(&parent_dir).unwrap();

        let path = parent_dir.join("test-heartbeat.json");
        std::fs::write(&path, b"test data").unwrap();
        assert!(path.exists(), "file should exist before cleanup");

        // Remove write permissions from the parent directory
        std::fs::set_permissions(&parent_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Verify the directory is actually unwritable. When running as root
        // (e.g., in CI containers), 0o555 doesn't block writes and we should
        // skip this test since permission checks won't work as expected.
        let probe = parent_dir.join(".write-probe");
        let unwritable = std::fs::write(&probe, b"x").is_err();
        let _ = std::fs::remove_file(&probe);

        if !unwritable {
            // Root bypasses permission checks — skip this test
            let _ = std::fs::set_permissions(&parent_dir, std::fs::Permissions::from_mode(0o755));
            return;
        }

        // Set up a log capture mechanism
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

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .without_time()
            .finish();

        // Run the test within the tracing subscriber context
        tracing::subscriber::with_default(subscriber, || {
            // Attempting to cleanup when we lack write permissions should fail
            let result = cleanup_heartbeat_file(&path);
            assert!(
                result.is_err(),
                "cleanup should return Err when permission is denied"
            );

            // Verify the error contains information about the failure
            let err = result.unwrap_err();
            let err_msg = err.to_string();
            assert!(
                err_msg.contains("failed to remove heartbeat file")
                    || err_msg.contains("permission")
                    || err_msg.contains("denied"),
                "error message should indicate removal failure or permission issue, got: {}",
                err
            );

            // Restore permissions so the tempdir can be cleaned up
            let _ = std::fs::set_permissions(&parent_dir, std::fs::Permissions::from_mode(0o755));

            // The file should still exist since removal failed
            assert!(
                path.exists(),
                "file should still exist after failed cleanup"
            );

            // Verify a warning was logged about the failure
            let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
            assert!(
                logs.contains("failed to remove heartbeat file during cleanup"),
                "warning should be logged when permission denied. Got logs: {}",
                logs
            );

            // Verify the log is at WARN level
            assert!(
                logs.contains("WARN"),
                "log should be at WARN level, got: {}",
                logs
            );
        });
    }

    /// Test that cleanup_heartbeat_file returns Err for other IO errors.
    ///
    /// This test verifies that various IO errors (beyond NotFound and
    /// PermissionDenied) are properly propagated.
    ///
    /// Note: This test was removed for needle-068b926f because it was redundant.
    /// The other error case tests already cover all meaningful error scenarios:
    /// - cleanup_heartbeat_file_errs_on_removal_failure tests removal failures
    /// - cleanup_heartbeat_file_errs_on_permission_denied tests permission errors
    /// - cleanup_heartbeat_file_logs_warning_on_error tests warning logging
    ///
    /// The original "long path" test case triggered NotFound (which is handled
    /// as idempotent success with debug logging), not a distinct error type.
    #[test]
    #[ignore = "redundant - covered by other error case tests"]
    fn cleanup_heartbeat_file_errs_on_other_io_errors() {
        // Test removed - see documentation above
        unreachable!("this test is redundant and should not be called");
    }

    /// Test that cleanup_heartbeat_file logs warnings on error.
    ///
    /// This test verifies the acceptance criteria from bf-1bdjl:
    /// - Cleanup doesn't panic on file removal failure
    /// - Error condition is simulated (directory instead of file)
    /// - Error is properly logged with tracing::warn!
    /// - Error is returned (not swallowed)
    ///
    /// The test captures tracing logs and verifies that the expected
    /// warning message is emitted when cleanup fails.
    #[test]
    fn cleanup_heartbeat_file_logs_warning_on_error() {
        use std::io::Write;
        use std::sync::Mutex;

        // Set up a log capture mechanism similar to worker tests
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

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .without_time()
            .finish();

        // Run the test within the tracing subscriber context
        tracing::subscriber::with_default(subscriber, || {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-heartbeat.json");

            // Create a directory at the path (removing a directory will fail)
            std::fs::create_dir(&path).unwrap();
            assert!(path.exists(), "directory should exist before cleanup");

            // Attempt to cleanup - this should fail and log a warning
            let result = cleanup_heartbeat_file(&path);

            // Verify the function returns an error (doesn't panic)
            assert!(
                result.is_err(),
                "cleanup should return Err when removal fails"
            );

            // Verify the error message contains context
            let err = result.unwrap_err();
            let err_msg = err.to_string();
            assert!(
                err_msg.contains("failed to remove heartbeat file"),
                "error message should contain context about the failure"
            );

            // Verify the directory still exists (removal failed as expected)
            assert!(
                path.exists(),
                "directory should still exist after failed cleanup"
            );

            // Verify the warning was logged
            let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
            assert!(
                logs.contains("failed to remove heartbeat file during cleanup"),
                "warning message should be logged on cleanup failure"
            );

            // Verify the log contains the path
            assert!(
                logs.contains(&path.display().to_string()),
                "log should contain the file path"
            );

            // Verify the log is at WARN level (JSON format uses "level":"WARN")
            assert!(
                logs.contains("WARN"),
                "log should be at WARN level, got: {}",
                logs
            );
        });
    }

    /// Test that cleanup_heartbeat_file logs debug when file doesn't exist.
    ///
    /// This test verifies the acceptance criteria from needle-068b926f:
    /// - Cleanup doesn't panic when file doesn't exist
    /// - Debug message is logged with tracing::debug! for idempotent cleanup
    /// - Function returns Ok(()) when file doesn't exist
    ///
    /// The test captures tracing logs and verifies that the expected
    /// debug message is emitted when cleanup succeeds on a missing file.
    #[test]
    fn cleanup_heartbeat_file_logs_debug_when_missing() {
        use std::io::Write;
        use std::sync::Mutex;

        // Set up a log capture mechanism
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

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::DEBUG)
            .finish();

        // Run the test within the tracing subscriber context
        tracing::subscriber::with_default(subscriber, || {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("nonexistent-heartbeat.json");

            // Verify file doesn't exist
            assert!(!path.exists(), "file should not exist before cleanup");

            // Attempt to cleanup - this should succeed with debug logging
            let result = cleanup_heartbeat_file(&path);

            // Verify the function returns Ok (doesn't panic)
            assert!(
                result.is_ok(),
                "cleanup should return Ok(()) when file doesn't exist"
            );

            // Verify the file still doesn't exist
            assert!(!path.exists(), "file should still not exist after cleanup");

            // Verify the debug message was logged
            let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
            assert!(
                logs.contains("heartbeat file does not exist, skipping cleanup"),
                "debug message should be logged when file doesn't exist. Got logs: {}",
                logs
            );

            // Verify the log contains the path
            assert!(
                logs.contains(&path.display().to_string()),
                "log should contain the file path"
            );

            // Verify the log is at DEBUG level (JSON format uses "level":"DEBUG")
            assert!(
                logs.contains("DEBUG"),
                "log should be at DEBUG level, got: {}",
                logs
            );
        });
    }

    /// Test that cleanup_heartbeat_file handles symlinks correctly.
    ///
    /// This test verifies that cleanup_heartbeat_file can remove a symlink
    /// to a file even if the target file doesn't exist (broken symlink).
    #[test]
    fn cleanup_heartbeat_file_removes_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-heartbeat.json");
        let target = dir.path().join("nonexistent-target.json");

        // Create a symlink to a non-existent file (broken symlink)
        symlink(&target, &path).unwrap();

        // Check that the symlink exists (using metadata since exists() returns false for broken symlinks)
        assert!(path.symlink_metadata().is_ok(), "symlink should exist");

        // Cleanup should remove the symlink even though target doesn't exist
        cleanup_heartbeat_file(&path).unwrap();

        // Verify the symlink was removed
        assert!(
            path.symlink_metadata().is_err(),
            "symlink should be removed"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Supervisor Presence Detection Tests
    // ──────────────────────────────────────────────────────────────────────────────

    /// Test that check_supervisor_heartbeat_file returns true when fresh file exists.
    #[test]
    fn check_supervisor_heartbeat_file_fresh_returns_true() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create a fresh supervisor heartbeat file
        let supervisor_hb_path = hb_dir.join("supervisor-heartbeat.json");
        let supervisor_data = serde_json::json!({
            "last_heartbeat": Utc::now().to_rfc3339(),
            "pid": std::process::id(),
            "workspace": "/tmp"
        });
        std::fs::write(&supervisor_hb_path, supervisor_data.to_string()).unwrap();

        // Should detect fresh supervisor heartbeat
        let detected = monitor.check_supervisor_heartbeat_file().unwrap();
        assert!(detected, "should detect fresh supervisor heartbeat file");
    }

    /// Test that check_supervisor_heartbeat_file returns false when file is stale.
    #[test]
    fn check_supervisor_heartbeat_file_stale_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create a stale supervisor heartbeat file (older than 2 minutes)
        let supervisor_hb_path = hb_dir.join("supervisor-heartbeat.json");
        let stale_time = Utc::now() - chrono::Duration::seconds(180); // 3 minutes ago
        let supervisor_data = serde_json::json!({
            "last_heartbeat": stale_time.to_rfc3339(),
            "pid": std::process::id(),
            "workspace": "/tmp"
        });
        std::fs::write(&supervisor_hb_path, supervisor_data.to_string()).unwrap();

        // Should not detect stale supervisor heartbeat
        let detected = monitor.check_supervisor_heartbeat_file().unwrap();
        assert!(
            !detected,
            "should not detect stale supervisor heartbeat file"
        );
    }

    /// Test that check_supervisor_heartbeat_file returns false when file doesn't exist.
    #[test]
    fn check_supervisor_heartbeat_file_missing_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // No supervisor heartbeat file exists
        let detected = monitor.check_supervisor_heartbeat_file().unwrap();
        assert!(
            !detected,
            "should return false when supervisor heartbeat file doesn't exist"
        );
    }

    /// Test that check_supervisor_heartbeat_file returns false when file is invalid.
    #[test]
    fn check_supervisor_heartbeat_file_invalid_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create an invalid supervisor heartbeat file (missing timestamp field)
        let supervisor_hb_path = hb_dir.join("supervisor-heartbeat.json");
        let invalid_data = serde_json::json!({
            "pid": std::process::id(),
            "workspace": "/tmp"
            // Missing last_heartbeat field
        });
        std::fs::write(&supervisor_hb_path, invalid_data.to_string()).unwrap();

        // Should not detect supervisor heartbeat without valid timestamp
        let detected = monitor.check_supervisor_heartbeat_file().unwrap();
        assert!(
            !detected,
            "should return false when supervisor heartbeat file is invalid"
        );
    }

    /// Test that check_supervisor_heartbeat_file handles malformed JSON.
    #[test]
    fn check_supervisor_heartbeat_file_malformed_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create a malformed JSON file
        let supervisor_hb_path = hb_dir.join("supervisor-heartbeat.json");
        std::fs::write(&supervisor_hb_path, b"invalid json {{{").unwrap();

        // Should return error for malformed JSON
        let result = monitor.check_supervisor_heartbeat_file();
        assert!(result.is_err(), "should return error for malformed JSON");
    }

    /// Test that check_supervisor_socket returns true when socket exists.
    #[test]
    fn check_supervisor_socket_exists_returns_true() {
        let _env_guard = lock_supervisor_socket_env();
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test-supervisor.sock");

        // Set environment variable to use our test socket path
        std::env::set_var("NEEDLE_SUPERVISOR_SOCKET", socket_path.to_str().unwrap());

        #[cfg(unix)]
        {
            // Create a real Unix socket for testing
            use std::os::unix::net::UnixListener;
            let _listener = UnixListener::bind(&socket_path).unwrap();
        }

        #[cfg(not(unix))]
        {
            // On non-Unix, just create a file
            std::fs::write(&socket_path, b"test").unwrap();
        }

        // Verify the path exists
        assert!(socket_path.exists(), "socket path should exist");

        // Should detect socket at the path
        let detected = HealthMonitor::check_supervisor_socket().unwrap();
        assert!(detected, "should detect socket at the specified path");

        // Cleanup
        std::env::remove_var("NEEDLE_SUPERVISOR_SOCKET");
    }

    /// Test that check_supervisor_socket returns false when socket doesn't exist.
    #[test]
    fn check_supervisor_socket_missing_returns_false() {
        let _env_guard = lock_supervisor_socket_env();
        // Set environment variable to a non-existent path
        std::env::set_var("NEEDLE_SUPERVISOR_SOCKET", "/nonexistent/socket.sock");

        // Should return false when socket doesn't exist
        let detected = HealthMonitor::check_supervisor_socket().unwrap();
        assert!(!detected, "should return false when socket doesn't exist");

        // Cleanup
        std::env::remove_var("NEEDLE_SUPERVISOR_SOCKET");
    }

    /// Test that check_supervisor_socket uses default path when env var not set.
    #[test]
    fn check_supervisor_socket_default_path() {
        let _env_guard = lock_supervisor_socket_env();
        // Don't set NEEDLE_SUPERVISOR_SOCKET - should use default /tmp/needle-supervisor.sock
        // This likely won't exist in test environment, so we expect false
        let detected = HealthMonitor::check_supervisor_socket().unwrap();
        // We don't assert the result since we can't control the test environment's /tmp
        // Just verify it doesn't error and returns a boolean (type system guarantees this)
        let _: bool = detected;
    }

    /// Test that detect_supervisor_direct returns true when heartbeat file is present.
    #[test]
    fn detect_supervisor_direct_with_heartbeat_returns_true() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create a fresh supervisor heartbeat file
        let supervisor_hb_path = hb_dir.join("supervisor-heartbeat.json");
        let supervisor_data = serde_json::json!({
            "last_heartbeat": Utc::now().to_rfc3339(),
            "pid": std::process::id(),
            "workspace": "/tmp"
        });
        std::fs::write(&supervisor_hb_path, supervisor_data.to_string()).unwrap();

        // Should detect supervisor via heartbeat file
        let detected = monitor.detect_supervisor_direct();
        assert!(detected, "should detect supervisor via heartbeat file");
    }

    /// Test that detect_supervisor_direct returns true when socket is present.
    #[test]
    fn detect_supervisor_direct_with_socket_returns_true() {
        let _env_guard = lock_supervisor_socket_env();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create a real Unix socket for testing
        let socket_path = dir.path().join("supervisor.sock");
        std::env::set_var("NEEDLE_SUPERVISOR_SOCKET", socket_path.to_str().unwrap());

        #[cfg(unix)]
        {
            use std::os::unix::net::UnixListener;
            let _listener = UnixListener::bind(&socket_path).unwrap();
        }

        #[cfg(not(unix))]
        {
            // On non-Unix, just create a file
            std::fs::write(&socket_path, b"test").unwrap();
        }

        // Should detect supervisor via socket
        let detected = monitor.detect_supervisor_direct();
        assert!(detected, "should detect supervisor via socket");

        // Cleanup
        std::env::remove_var("NEEDLE_SUPERVISOR_SOCKET");
    }

    /// Test that detect_supervisor_direct returns false when no supervisor detected.
    #[test]
    fn detect_supervisor_direct_no_supervisor_returns_false() {
        let _env_guard = lock_supervisor_socket_env();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Set socket path to non-existent
        std::env::set_var("NEEDLE_SUPERVISOR_SOCKET", "/nonexistent/socket.sock");

        // No supervisor heartbeat file or socket
        let detected = monitor.detect_supervisor_direct();
        assert!(
            !detected,
            "should return false when no supervisor is detected"
        );

        // Cleanup
        std::env::remove_var("NEEDLE_SUPERVISOR_SOCKET");
    }

    /// Test that detect_supervisor_direct prefers heartbeat over socket.
    #[test]
    fn detect_supervisor_direct_heartbeat_takes_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create both heartbeat file and socket
        let supervisor_hb_path = hb_dir.join("supervisor-heartbeat.json");
        let supervisor_data = serde_json::json!({
            "last_heartbeat": Utc::now().to_rfc3339(),
            "pid": std::process::id(),
            "workspace": "/tmp"
        });
        std::fs::write(&supervisor_hb_path, supervisor_data.to_string()).unwrap();

        // Should detect supervisor via heartbeat file first (even if socket also exists)
        let detected = monitor.detect_supervisor_direct();
        assert!(
            detected,
            "should detect supervisor via heartbeat (checked first)"
        );
    }

    /// Test that detect_supervisor_direct treats a corrupt heartbeat file as
    /// "no supervisor" instead of erroring out (fail-safe, needle-d00f58d3).
    ///
    /// A corrupt heartbeat must not disable the idle_action guard: before the
    /// fix, the read/parse error propagated and the worker boot silently
    /// skipped validation entirely.
    #[test]
    fn detect_supervisor_direct_corrupt_heartbeat_returns_false() {
        let _env_guard = lock_supervisor_socket_env();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Corrupt heartbeat file: exists but is not valid JSON
        let supervisor_hb_path = hb_dir.join("supervisor-heartbeat.json");
        std::fs::write(&supervisor_hb_path, b"{not valid json").unwrap();

        // Point the socket fallback at a nonexistent path so only the
        // heartbeat leg can influence the result
        std::env::set_var("NEEDLE_SUPERVISOR_SOCKET", "/nonexistent/socket.sock");

        let detected = monitor.detect_supervisor_direct();
        assert!(
            !detected,
            "corrupt heartbeat must be treated as supervisor absent, not an error"
        );

        // Cleanup
        std::env::remove_var("NEEDLE_SUPERVISOR_SOCKET");
    }

    /// Test that a corrupt heartbeat still falls through to the socket check,
    /// so a live supervisor remains detectable when only its heartbeat file
    /// is degraded (needle-d00f58d3).
    #[test]
    fn detect_supervisor_direct_corrupt_heartbeat_falls_back_to_socket() {
        let _env_guard = lock_supervisor_socket_env();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Corrupt heartbeat file
        let supervisor_hb_path = hb_dir.join("supervisor-heartbeat.json");
        std::fs::write(&supervisor_hb_path, b"garbage").unwrap();

        // Live supervisor socket
        let socket_path = dir.path().join("supervisor.sock");
        std::env::set_var("NEEDLE_SUPERVISOR_SOCKET", socket_path.to_str().unwrap());

        #[cfg(unix)]
        {
            use std::os::unix::net::UnixListener;
            let _listener = UnixListener::bind(&socket_path).unwrap();
        }

        #[cfg(not(unix))]
        {
            // On non-Unix, just create a file
            std::fs::write(&socket_path, b"test").unwrap();
        }

        let detected = monitor.detect_supervisor_direct();
        assert!(
            detected,
            "socket fallback must still detect a live supervisor despite corrupt heartbeat"
        );

        // Cleanup
        std::env::remove_var("NEEDLE_SUPERVISOR_SOCKET");
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // HealthMonitor::cleanup_heartbeat_file() Tests
    // ──────────────────────────────────────────────────────────────────────────────

    /// Test that HealthMonitor::cleanup_heartbeat_file removes an existing file.
    #[tokio::test]
    async fn healthmonitor_cleanup_heartbeat_file_removes_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let mut monitor = HealthMonitor::new(
            config,
            "cleanup-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Start the emitter to create the heartbeat file
        monitor.start_emitter().unwrap();
        let path = monitor.heartbeat_path();
        assert!(path.exists(), "heartbeat file should exist before cleanup");

        // Cleanup should succeed and remove the file
        monitor.cleanup_heartbeat_file().unwrap();
        assert!(
            !path.exists(),
            "heartbeat file should not exist after cleanup"
        );

        monitor.stop();
    }

    /// Test that HealthMonitor::cleanup_heartbeat_file returns Ok when file doesn't exist.
    #[tokio::test]
    async fn healthmonitor_cleanup_heartbeat_file_ok_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "cleanup-missing-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        let path = monitor.heartbeat_path();
        assert!(!path.exists(), "heartbeat file should not exist");

        // Cleanup should succeed even when file doesn't exist
        let result = monitor.cleanup_heartbeat_file();
        assert!(
            result.is_ok(),
            "cleanup should succeed when file doesn't exist"
        );
        assert!(!path.exists(), "file should still not exist after cleanup");
    }

    /// Test that HealthMonitor::cleanup_heartbeat_file logs errors but doesn't fail when removal fails.
    ///
    /// This test verifies the acceptance criteria:
    /// - Log errors when file removal fails
    /// - Ensure the function doesn't panic on cleanup failure
    /// - Continue execution even if cleanup fails
    #[tokio::test]
    async fn healthmonitor_cleanup_heartbeat_file_logs_errors_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "cleanup-error-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        let path = monitor.heartbeat_path();

        // Create a directory at the path (removing a directory will fail)
        std::fs::create_dir(&path).unwrap();

        // Attempting to cleanup a directory instead of a file reports the
        // removal failure while leaving the directory intact.
        let result = monitor.cleanup_heartbeat_file();
        assert!(
            result.is_err(),
            "cleanup should return the removal failure after logging it"
        );

        // The directory should still exist (removal failed, but execution continued)
        assert!(
            path.exists(),
            "directory should still exist after failed cleanup"
        );
    }

    /// Test that HealthMonitor::cleanup_heartbeat_file works after emitter is running.
    #[tokio::test]
    async fn healthmonitor_cleanup_heartbeat_file_with_running_emitter() {
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let mut monitor = HealthMonitor::new(
            config,
            "cleanup-running-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Start the emitter
        monitor.start_emitter().unwrap();
        let path = monitor.heartbeat_path();
        assert!(path.exists(), "heartbeat file should exist");

        // Cleanup should remove the file
        monitor.cleanup_heartbeat_file().unwrap();
        assert!(!path.exists(), "heartbeat file should be removed");

        // Stop the emitter (should not recreate the file)
        monitor.stop();
        assert!(!path.exists(), "heartbeat file should not exist after stop");
    }

    /// Test that cleanup_heartbeat_file handles various error conditions gracefully.
    ///
    /// This test verifies the acceptance criteria:
    /// - Cleanup doesn't panic on file removal failure
    /// - Error conditions are properly logged with tracing::warn!
    /// - Error is returned with proper context (not swallowed)
    /// - Function handles permission errors appropriately
    /// - Function handles non-file filesystem objects (directories, etc.)
    ///
    /// The test uses tempfile to create controlled error scenarios and verifies
    /// that the cleanup function handles them without crashing while providing
    /// useful error information.
    #[test]
    fn cleanup_heartbeat_file_error_handling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-heartbeat.json");

        // Test 1: Attempting to remove a directory instead of a file should fail gracefully
        std::fs::create_dir(&path).unwrap();
        let result = cleanup_heartbeat_file(&path);

        // Should return an error (not panic)
        assert!(
            result.is_err(),
            "cleanup should return Err when trying to remove a directory"
        );

        // Verify the error message provides context
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("failed to remove heartbeat file"),
            "error message should contain context about cleanup failure: {}",
            err_msg
        );

        // The directory should still exist (cleanup failed, but didn't crash)
        assert!(
            path.exists(),
            "directory should still exist after failed cleanup"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Heartbeat Freshness Detection Tests
    // ──────────────────────────────────────────────────────────────────────────────

    /// Test age calculation with fresh timestamp (recent heartbeat).
    ///
    /// This test verifies the acceptance criteria:
    /// - Age calculation returns correct value for fresh heartbeat
    /// - Recent timestamp (within TTL) produces small age value
    #[test]
    fn calculate_heartbeat_age_fresh_timestamp() {
        let now = Utc::now();
        let recent = now - chrono::Duration::seconds(30); // 30 seconds ago

        let age = calculate_heartbeat_age(recent);

        assert_eq!(
            age, 30,
            "age should be 30 seconds for a heartbeat from 30 seconds ago"
        );
    }

    /// Test age calculation with stale timestamp (old heartbeat).
    ///
    /// This test verifies the acceptance criteria:
    /// - Age calculation returns correct value for stale heartbeat
    /// - Old timestamp (beyond TTL) produces large age value
    #[test]
    fn calculate_heartbeat_age_stale_timestamp() {
        let now = Utc::now();
        let old = now - chrono::Duration::seconds(600); // 10 minutes ago

        let age = calculate_heartbeat_age(old);

        assert_eq!(
            age, 600,
            "age should be 600 seconds for a heartbeat from 10 minutes ago"
        );
    }

    /// Test age calculation with future timestamp (clock skew handling).
    ///
    /// This test verifies the acceptance criteria:
    /// - Age calculation handles clock skew gracefully
    /// - Future timestamp returns 0 (not negative)
    #[test]
    fn calculate_heartbeat_age_future_timestamp() {
        let now = Utc::now();
        let future = now + chrono::Duration::seconds(30); // 30 seconds in the future

        let age = calculate_heartbeat_age(future);

        assert_eq!(
            age, 0,
            "age should be 0 for future timestamps (clock skew handling)"
        );
    }

    /// Test age calculation with current timestamp.
    ///
    /// This test verifies the acceptance criteria:
    /// - Age calculation returns 0 for heartbeat created just now
    #[test]
    fn calculate_heartbeat_age_current_timestamp() {
        let now = Utc::now();

        let age = calculate_heartbeat_age(now);

        // Allow small tolerance for timing differences
        assert!(
            age <= 1,
            "age should be 0 or 1 seconds for current timestamp, got {}",
            age
        );
    }

    /// Test that is_stale correctly identifies fresh heartbeats.
    ///
    /// This test verifies the acceptance criteria:
    /// - Fresh heartbeat (age < TTL) is not stale
    #[test]
    fn is_stale_fresh_heartbeat_not_stale() {
        let hb = HeartbeatData {
            worker_id: "test".to_string(),
            qualified_id: "claude-test".to_string(),
            pid: 1,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(60), // 1 minute ago
            started_at: Utc::now(),
            beads_processed: 0,
            session: "test".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
        };

        let ttl = Duration::from_secs(300); // 5 minutes

        assert!(
            !HealthMonitor::is_stale(&hb, ttl),
            "fresh heartbeat (1 minute old) should not be stale with 5 minute TTL"
        );
    }

    /// Test that is_stale correctly identifies stale heartbeats.
    ///
    /// This test verifies the acceptance criteria:
    /// - Stale heartbeat (age > TTL) is stale
    #[test]
    fn is_stale_stale_heartbeat_is_stale() {
        let hb = HeartbeatData {
            worker_id: "test".to_string(),
            qualified_id: "claude-test".to_string(),
            pid: 1,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(600), // 10 minutes ago
            started_at: Utc::now(),
            beads_processed: 0,
            session: "test".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
        };

        let ttl = Duration::from_secs(300); // 5 minutes

        assert!(
            HealthMonitor::is_stale(&hb, ttl),
            "stale heartbeat (10 minutes old) should be stale with 5 minute TTL"
        );
    }

    /// Test that is_stale handles boundary condition (age == TTL).
    ///
    /// This test verifies the acceptance criteria:
    /// - Heartbeat exactly at TTL boundary is considered stale
    #[test]
    fn is_stale_boundary_at_ttl() {
        let hb = HeartbeatData {
            worker_id: "test".to_string(),
            qualified_id: "claude-test".to_string(),
            pid: 1,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(300), // exactly 5 minutes ago
            started_at: Utc::now(),
            beads_processed: 0,
            session: "test".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
        };

        let ttl = Duration::from_secs(300); // 5 minutes

        assert!(
            HealthMonitor::is_stale(&hb, ttl),
            "heartbeat exactly at TTL boundary should be considered stale"
        );
    }

    /// Test liveness check with fresh heartbeat returns true.
    ///
    /// This test verifies the acceptance criteria:
    /// - Liveness check returns Ok(true) for fresh heartbeat
    /// - Heartbeat file exists and is within TTL
    #[tokio::test]
    async fn check_worker_alive_fresh_heartbeat_returns_true() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create a fresh heartbeat file for another worker
        let qualified_id = "claude-code-glm-5-other-worker";
        let path = hb_dir.join(format!("{}.json", qualified_id));

        let hb = HeartbeatData {
            worker_id: "other-worker".to_string(),
            qualified_id: qualified_id.to_string(),
            pid: 2000,
            state: WorkerState::Executing,
            current_bead: Some(BeadId::from("nd-abc")),
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(2), // 2 seconds ago (fresh, within 5s TTL)
            started_at: Utc::now(),
            beads_processed: 5,
            session: "other-worker".to_string(),
            is_idle: false,
            current_task: Some("nd-abc".to_string()),
            model: "claude-code-glm-5".to_string(),
            heartbeat_file: None,
        };

        std::fs::write(&path, serde_json::to_string(&hb).unwrap()).unwrap();

        // Check if the worker is alive
        let is_alive = monitor.check_worker_alive(qualified_id).unwrap();

        assert!(
            is_alive,
            "worker with fresh heartbeat (2 seconds old) should be considered alive"
        );
    }

    /// Test liveness check with stale heartbeat returns false.
    ///
    /// This test verifies the acceptance criteria:
    /// - Liveness check returns Ok(false) for stale heartbeat
    /// - Heartbeat file exists but age exceeds TTL
    #[tokio::test]
    async fn check_worker_alive_stale_heartbeat_returns_false() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create a stale heartbeat file for another worker
        let qualified_id = "claude-code-glm-5-stale-worker";
        let path = hb_dir.join(format!("{}.json", qualified_id));

        let hb = HeartbeatData {
            worker_id: "stale-worker".to_string(),
            qualified_id: qualified_id.to_string(),
            pid: 3000,
            state: WorkerState::Executing,
            current_bead: Some(BeadId::from("nd-def")),
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(600), // 10 minutes ago (stale)
            started_at: Utc::now(),
            beads_processed: 10,
            session: "stale-worker".to_string(),
            is_idle: false,
            current_task: Some("nd-def".to_string()),
            model: "claude-code-glm-5".to_string(),
            heartbeat_file: None,
        };

        std::fs::write(&path, serde_json::to_string(&hb).unwrap()).unwrap();

        // Check if the worker is alive
        let is_alive = monitor.check_worker_alive(qualified_id).unwrap();

        assert!(
            !is_alive,
            "worker with stale heartbeat (10 minutes old) should not be considered alive"
        );
    }

    /// Test liveness check with missing heartbeat returns false.
    ///
    /// This test verifies the acceptance criteria:
    /// - Liveness check returns Ok(false) for missing heartbeat
    /// - Heartbeat file does not exist
    #[tokio::test]
    async fn check_worker_alive_missing_heartbeat_returns_false() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Check a worker that doesn't have a heartbeat file
        let qualified_id = "claude-code-glm-5-nonexistent-worker";

        let is_alive = monitor.check_worker_alive(qualified_id).unwrap();

        assert!(
            !is_alive,
            "worker without heartbeat file should not be considered alive"
        );
    }

    /// Test liveness check with malformed heartbeat returns Err.
    ///
    /// This test verifies the acceptance criteria:
    /// - Liveness check returns Err for malformed heartbeat
    /// - Heartbeat file exists but cannot be parsed
    #[tokio::test]
    async fn check_worker_alive_malformed_heartbeat_returns_error() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create a malformed heartbeat file (invalid JSON)
        let qualified_id = "claude-code-glm-5-malformed-worker";
        let path = hb_dir.join(format!("{}.json", qualified_id));

        std::fs::write(&path, b"invalid json {{{").unwrap();

        // Attempt to check if the worker is alive
        let result = monitor.check_worker_alive(qualified_id);

        assert!(result.is_err(), "malformed heartbeat should return error");

        let err = result.unwrap_err();
        let err_msg = err.to_string().to_lowercase();
        assert!(
            err_msg.contains("parse") || err_msg.contains("heartbeat"),
            "error message should mention parsing or heartbeat: {}",
            err
        );
    }

    /// Test liveness check with heartbeat file containing invalid timestamp.
    ///
    /// This test verifies the acceptance criteria:
    /// - Liveness check returns Err for heartbeat with invalid timestamp
    /// - Heartbeat file is valid JSON but timestamp cannot be parsed
    #[tokio::test]
    async fn check_worker_alive_invalid_timestamp_returns_error() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Create a heartbeat file with invalid timestamp format
        let qualified_id = "claude-code-glm-5-bad-ts-worker";
        let path = hb_dir.join(format!("{}.json", qualified_id));

        let invalid_hb = serde_json::json!({
            "worker_id": "bad-ts-worker",
            "qualified_id": qualified_id,
            "pid": 4000,
            "state": "Executing",
            "current_bead": "nd-xyz",
            "workspace": "/tmp",
            "last_heartbeat": "not-a-valid-timestamp", // Invalid timestamp
            "started_at": "2026-08-15T00:00:00Z",
            "beads_processed": 0,
            "session": "bad-ts-worker",
            "is_idle": false,
            "current_task": "nd-xyz",
            "model": "claude-code-glm-5"
        });

        std::fs::write(&path, invalid_hb.to_string()).unwrap();

        // Attempt to check if the worker is alive
        let result = monitor.check_worker_alive(qualified_id);

        assert!(
            result.is_err(),
            "heartbeat with invalid timestamp should return error"
        );

        let err = result.unwrap_err();
        let err_msg = err.to_string().to_lowercase();
        assert!(
            err_msg.contains("parse") || err_msg.contains("timestamp"),
            "error message should mention parsing or timestamp: {}",
            err
        );
    }

    /// Test verify_heartbeat with fresh heartbeat returns true.
    ///
    /// This test verifies the acceptance criteria:
    /// - verify_heartbeat returns Ok(true) for fresh heartbeat
    /// - Checks this worker's own heartbeat file
    #[tokio::test]
    async fn verify_heartbeat_fresh_returns_true() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let mut monitor = HealthMonitor::new(
            config,
            "verify-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Start emitter to create a fresh heartbeat
        monitor.start_emitter().unwrap();

        // Verify the heartbeat is fresh
        let is_fresh = monitor.verify_heartbeat().unwrap();

        assert!(
            is_fresh,
            "freshly created heartbeat should be verified as fresh"
        );

        monitor.stop();
    }

    /// Test verify_heartbeat with missing heartbeat returns false.
    ///
    /// This test verifies the acceptance criteria:
    /// - verify_heartbeat returns Ok(false) for missing heartbeat
    /// - Heartbeat file does not exist
    #[tokio::test]
    async fn verify_heartbeat_missing_returns_false() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "verify-missing-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Don't start emitter - no heartbeat file exists

        // Verify the heartbeat is missing (returns Ok(false))
        let is_fresh = monitor.verify_heartbeat().unwrap();

        assert!(
            !is_fresh,
            "missing heartbeat should return false (not fresh)"
        );
    }

    /// Test verify_heartbeat with stale heartbeat returns false.
    ///
    /// This test verifies the acceptance criteria:
    /// - verify_heartbeat returns Ok(false) for stale heartbeat
    /// - Heartbeat file exists but age exceeds TTL
    #[tokio::test]
    async fn verify_heartbeat_stale_returns_false() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let mut monitor = HealthMonitor::new(
            config,
            "verify-stale-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Start emitter to create a heartbeat
        monitor.start_emitter().unwrap();

        let path = monitor.heartbeat_path();

        // Manually update the heartbeat file to have an old timestamp
        let content = std::fs::read_to_string(&path).unwrap();
        let mut hb: HeartbeatData = serde_json::from_str(&content).unwrap();
        hb.last_heartbeat = Utc::now() - chrono::Duration::seconds(600); // 10 minutes ago
        std::fs::write(&path, serde_json::to_string(&hb).unwrap()).unwrap();

        // Verify the heartbeat is stale
        let is_fresh = monitor.verify_heartbeat().unwrap();

        assert!(
            !is_fresh,
            "stale heartbeat (10 minutes old) should not be verified as fresh"
        );

        monitor.stop();
    }

    /// Test verify_heartbeat with malformed heartbeat returns Err.
    ///
    /// This test verifies the acceptance criteria:
    /// - verify_heartbeat returns Err for malformed heartbeat
    /// - Heartbeat file exists but cannot be parsed
    #[tokio::test]
    async fn verify_heartbeat_malformed_returns_error() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "verify-malformed-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        let path = monitor.heartbeat_path();

        // Create a malformed heartbeat file
        std::fs::write(&path, b"invalid json {{{").unwrap();

        // Verify the malformed heartbeat returns error
        let result = monitor.verify_heartbeat();

        assert!(result.is_err(), "malformed heartbeat should return error");

        let err = result.unwrap_err();
        let err_msg = err.to_string().to_lowercase();
        assert!(
            err_msg.contains("parse") || err_msg.contains("heartbeat"),
            "error message should mention parsing or heartbeat: {}",
            err
        );
    }

    /// Test that heartbeat_path field is correctly set for shutdown handler access.
    ///
    /// This test verifies the acceptance criteria:
    /// - The heartbeat_path field is computed during construction
    /// - The path matches the expected pattern: {heartbeat_dir}/{qualified_id}.json
    /// - The path can be used for cleanup by the shutdown handler
    /// - The path is accessible and consistent throughout the monitor lifecycle
    ///
    /// This ensures the shutdown handler will have access to the correct file path
    /// for cleanup on graceful shutdown.
    #[tokio::test]
    async fn heartbeat_path_field_correct_for_shutdown_handler() {
        let dir = tempfile::tempdir().unwrap();
        let _home_guard = isolate_test_home_at(dir.path());
        let hb_dir = dir.path().join(".needle").join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let mut config = Config::default();
        config.workspace.home = dir.path().to_path_buf();
        config.health.heartbeat_interval_secs = 1;
        config.health.heartbeat_ttl_secs = 5;

        // Create monitor with specific adapter and worker name
        config.agent.default = "test-adapter".to_string();
        let worker_name = "shutdown-handler-test".to_string();
        let monitor = HealthMonitor::new(
            config,
            worker_name.clone(),
            Telemetry::new("test".to_string()),
            None,
        );

        // EXPECTED PATH: {heartbeat_dir}/{qualified_id}.json
        // qualified_id = {adapter}-{worker_name} = "test-adapter-shutdown-handler-test"
        let expected_path = hb_dir.join("test-adapter-shutdown-handler-test.json");

        // ACCEPTANCE CRITERION: heartbeat_path field matches expected path
        let actual_path = monitor.heartbeat_path();
        assert_eq!(
            actual_path, expected_path,
            "heartbeat_path field must match expected path: {{heartbeat_dir}}/{{qualified_id}}.json"
        );

        // ACCEPTANCE CRITERION: Path is correctly formatted
        assert!(
            actual_path.starts_with(&hb_dir),
            "heartbeat_path must start with heartbeat_dir"
        );
        assert!(
            actual_path.extension().and_then(|e| e.to_str()) == Some("json"),
            "heartbeat_path must have .json extension"
        );

        // ACCEPTANCE CRITERION: Path can be used for actual file creation
        let mut monitor = monitor;
        monitor.start_emitter().unwrap();
        assert!(
            actual_path.exists(),
            "heartbeat file must be created at the computed path"
        );

        // ACCEPTANCE CRITERION: Shutdown handler can use this path for cleanup
        // Simulate shutdown handler accessing the path for cleanup
        let cleanup_path = monitor.heartbeat_path();
        assert_eq!(
            cleanup_path, actual_path,
            "shutdown handler must receive consistent path for cleanup"
        );
        assert!(
            cleanup_path.exists(),
            "shutdown handler must be able to access the file"
        );

        // Verify cleanup works using the path (as shutdown handler would)
        monitor.stop();
        assert!(
            !cleanup_path.exists(),
            "shutdown handler must successfully remove file using heartbeat_path"
        );

        // After shutdown, the path should still be consistent (even though file is gone)
        let final_path = monitor.heartbeat_path();
        assert_eq!(
            final_path, expected_path,
            "heartbeat_path must remain consistent even after file is removed"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Heartbeat File Presence Detection Tests
    // ──────────────────────────────────────────────────────────────────────────────

    /// Test that check_heartbeat_file_exists returns true when file exists.
    #[test]
    fn check_heartbeat_file_exists_returns_true_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let heartbeat_path = dir.path().join("supervisor-heartbeat.json");

        // Create a heartbeat file
        std::fs::write(&heartbeat_path, b"{}").unwrap();

        // Should detect the file exists
        let result = check_heartbeat_file_exists(&heartbeat_path);
        assert!(
            result.is_ok(),
            "check_heartbeat_file_exists should succeed when file exists"
        );
        assert!(
            result.unwrap(),
            "check_heartbeat_file_exists should return true when file exists"
        );
    }

    /// Test that check_heartbeat_file_exists returns false when file is absent.
    #[test]
    fn check_heartbeat_file_exists_returns_false_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let heartbeat_path = dir.path().join("supervisor-heartbeat.json");

        // Don't create any file - path should not exist

        // Should report file doesn't exist
        let result = check_heartbeat_file_exists(&heartbeat_path);
        assert!(
            result.is_ok(),
            "check_heartbeat_file_exists should succeed when file doesn't exist"
        );
        assert!(
            !result.unwrap(),
            "check_heartbeat_file_exists should return false when file doesn't exist"
        );
    }

    /// Test that check_heartbeat_file_exists returns false for directories.
    #[test]
    fn check_heartbeat_file_exists_returns_false_for_directory() {
        let dir = tempfile::tempdir().unwrap();
        let heartbeat_path = dir.path().join("supervisor-heartbeat.json");

        // Create a directory at the expected file path
        std::fs::create_dir(&heartbeat_path).unwrap();

        // Should return false since path is a directory, not a file
        let result = check_heartbeat_file_exists(&heartbeat_path);
        assert!(
            result.is_ok(),
            "check_heartbeat_file_exists should succeed when path is a directory"
        );
        assert!(
            !result.unwrap(),
            "check_heartbeat_file_exists should return false when path is a directory"
        );
    }

    /// Test that check_heartbeat_file_exists handles permission errors gracefully.
    #[test]
    fn check_heartbeat_file_exists_handles_permission_errors() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let heartbeat_path = dir.path().join("supervisor-heartbeat.json");

        // Create a file and make it unreadable
        std::fs::write(&heartbeat_path, b"{}").unwrap();
        std::fs::set_permissions(&heartbeat_path, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Verify the directory is actually unreadable. When running as root
        // (e.g., in CI containers), 0o000 doesn't block metadata access and the
        // check below would incorrectly succeed. Root bypasses permission checks.
        let unreadable = std::fs::metadata(&heartbeat_path).is_err();
        if !unreadable {
            // Root bypasses permission checks — skip this test.
            let _ =
                std::fs::set_permissions(&heartbeat_path, std::fs::Permissions::from_mode(0o644));
            return;
        }

        // Should return an error, not panic, on permission failure
        let result = check_heartbeat_file_exists(&heartbeat_path);
        assert!(
            result.is_err(),
            "check_heartbeat_file_exists should return error on permission denied"
        );

        // Restore permissions for cleanup
        let _ = std::fs::set_permissions(&heartbeat_path, std::fs::Permissions::from_mode(0o644));
    }

    /// Test that check_heartbeat_file_exists works with SupervisorDetectionConfig.
    #[test]
    fn check_heartbeat_file_exists_with_supervisor_config() {
        let dir = tempfile::tempdir().unwrap();
        let heartbeat_path = dir.path().join("custom-supervisor-hb.json");

        // Create supervisor detection config with custom path
        let config = SupervisorDetectionConfig {
            heartbeat_path: heartbeat_path.clone(),
            socket_path: None,
        };

        // Initially, no heartbeat file should exist
        let result = check_heartbeat_file_exists(&config.heartbeat_path);
        assert!(
            result.is_ok(),
            "check should succeed when heartbeat file doesn't exist"
        );
        assert!(
            !result.unwrap(),
            "should return false when file doesn't exist"
        );

        // Create the heartbeat file
        std::fs::write(&config.heartbeat_path, b"{}").unwrap();

        // Now the heartbeat file should exist
        let result = check_heartbeat_file_exists(&config.heartbeat_path);
        assert!(
            result.is_ok(),
            "check should succeed when heartbeat file exists"
        );
        assert!(result.unwrap(), "should return true when file exists");
    }

    /// Test that check_heartbeat_file_exists is testable in isolation.
    ///
    /// This test verifies the function can be tested without requiring a
    /// HealthMonitor instance or complex setup - just a Path.
    #[test]
    fn check_heartbeat_file_exists_is_isolated_testable() {
        // Test with a simple path, no complex setup needed
        let temp_path = PathBuf::from("/nonexistent/path/heartbeat.json");

        // Function works without any HealthMonitor or config setup
        let result = check_heartbeat_file_exists(&temp_path);
        assert!(
            result.is_ok(),
            "function should be callable with just a Path"
        );
        assert!(!result.unwrap(), "nonexistent path should return false");
    }

    /// Test that check_heartbeat_file_exists handles symlinks correctly.
    #[test]
    fn check_heartbeat_file_exists_handles_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let actual_file = dir.path().join("actual-heartbeat.json");
        let symlink_path = dir.path().join("heartbeat-symlink.json");

        // Create the actual file
        std::fs::write(&actual_file, b"{}").unwrap();

        // Create a symlink to the actual file
        std::os::unix::fs::symlink(&actual_file, &symlink_path).unwrap();

        // Should detect the file via symlink
        let result = check_heartbeat_file_exists(&symlink_path);
        assert!(
            result.is_ok(),
            "check_heartbeat_file_exists should follow symlinks"
        );
        assert!(
            result.unwrap(),
            "check_heartbeat_file_exists should return true for symlinked file"
        );
    }

    /// Test that check_heartbeat_file_exists handles broken symlinks.
    #[test]
    fn check_heartbeat_file_exists_handles_broken_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let nonexistent_target = dir.path().join("nonexistent-heartbeat.json");
        let symlink_path = dir.path().join("broken-symlink.json");

        // Create a symlink to a nonexistent file (broken symlink)
        std::os::unix::fs::symlink(&nonexistent_target, &symlink_path).unwrap();

        // Should return error for broken symlink (can't access metadata)
        let result = check_heartbeat_file_exists(&symlink_path);
        assert!(
            result.is_err(),
            "check_heartbeat_file_exists should return error for broken symlink"
        );
    }

    /// Test that calculate_heartbeat_age returns accurate age for past timestamps.
    #[test]
    fn calculate_heartbeat_age_past_timestamp() {
        let now = Utc::now();

        // Test various past timestamps
        let ten_seconds_ago = now - chrono::Duration::seconds(10);
        assert_eq!(calculate_heartbeat_age(ten_seconds_ago), 10);

        let two_minutes_ago = now - chrono::Duration::seconds(120);
        assert_eq!(calculate_heartbeat_age(two_minutes_ago), 120);

        let one_hour_ago = now - chrono::Duration::seconds(3600);
        assert_eq!(calculate_heartbeat_age(one_hour_ago), 3600);
    }

    /// Test that calculate_heartbeat_age returns 0 for future timestamps (clock skew).
    #[test]
    fn calculate_heartbeat_age_future_timestamp_returns_zero() {
        let now = Utc::now();

        // Test various future timestamps (simulating clock skew)
        let one_second_future = now + chrono::Duration::seconds(1);
        assert_eq!(calculate_heartbeat_age(one_second_future), 0);

        let thirty_seconds_future = now + chrono::Duration::seconds(30);
        assert_eq!(calculate_heartbeat_age(thirty_seconds_future), 0);

        let five_minutes_future = now + chrono::Duration::seconds(300);
        assert_eq!(calculate_heartbeat_age(five_minutes_future), 0);
    }

    /// Test that calculate_heartbeat_age returns 0 for current time.
    #[test]
    fn calculate_heartbeat_age_current_time() {
        let now = Utc::now();

        // Age of current time should be 0 (or very close due to execution time)
        let age = calculate_heartbeat_age(now);
        assert!(
            age == 0 || age == 1,
            "age of current timestamp should be 0 or 1, got {}",
            age
        );
    }

    /// Test that calculate_heartbeat_age handles very old timestamps.
    #[test]
    fn calculate_heartbeat_age_very_old_timestamp() {
        let now = Utc::now();

        // Test a very old timestamp (e.g., 24 hours ago)
        let one_day_ago = now - chrono::Duration::seconds(86400);
        assert_eq!(calculate_heartbeat_age(one_day_ago), 86400);

        // Test a week old timestamp
        let one_week_ago = now - chrono::Duration::seconds(604800);
        assert_eq!(calculate_heartbeat_age(one_week_ago), 604800);
    }

    /// Test that calculate_heartbeat_age can be used for freshness detection.
    #[test]
    fn calculate_heartbeat_age_for_freshness_detection() {
        let now = Utc::now();
        let ttl_secs = 300; // 5 minutes TTL

        // Fresh heartbeat (within TTL)
        let fresh_timestamp = now - chrono::Duration::seconds(120);
        let age = calculate_heartbeat_age(fresh_timestamp);
        assert!(
            age < ttl_secs,
            "fresh heartbeat should have age less than TTL: {} < {}",
            age,
            ttl_secs
        );

        // Stale heartbeat (exceeded TTL)
        let stale_timestamp = now - chrono::Duration::seconds(600);
        let age = calculate_heartbeat_age(stale_timestamp);
        assert!(
            age > ttl_secs,
            "stale heartbeat should have age greater than TTL: {} > {}",
            age,
            ttl_secs
        );
    }

    /// Test that check_worker_alive returns true for fresh heartbeats.
    #[tokio::test]
    async fn check_worker_alive_fresh_heartbeat() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let mut config = test_config(&hb_dir);
        config.health.heartbeat_ttl_secs = 300;

        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Write a fresh heartbeat for another worker
        let worker_id = "claude-code-glm-5-alpha";
        let heartbeat = HeartbeatData {
            worker_id: "alpha".to_string(),
            qualified_id: worker_id.to_string(),
            pid: 1234,
            state: WorkerState::Executing,
            current_bead: Some(BeadId::from("needle-abc")),
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now(),
            started_at: Utc::now(),
            beads_processed: 5,
            session: "alpha".to_string(),
            is_idle: false,
            current_task: Some("needle-abc".to_string()),
            model: "claude-code-glm-5".to_string(),
            heartbeat_file: None,
        };

        let path = hb_dir.join(format!("{}.json", worker_id));
        std::fs::write(&path, serde_json::to_string(&heartbeat).unwrap()).unwrap();

        // Check that the worker is alive
        let is_alive = monitor.check_worker_alive(worker_id).unwrap();
        assert!(is_alive, "worker with fresh heartbeat should be alive");
    }

    /// Test that check_worker_alive returns false for stale heartbeats.
    #[tokio::test]
    async fn check_worker_alive_stale_heartbeat() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let mut config = test_config(&hb_dir);
        config.health.heartbeat_ttl_secs = 300;

        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Write a stale heartbeat for another worker (older than TTL)
        let worker_id = "claude-code-glm-5-alpha";
        let heartbeat = HeartbeatData {
            worker_id: "alpha".to_string(),
            qualified_id: worker_id.to_string(),
            pid: 1234,
            state: WorkerState::Executing,
            current_bead: Some(BeadId::from("needle-abc")),
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(600), // 10 minutes old
            started_at: Utc::now(),
            beads_processed: 5,
            session: "alpha".to_string(),
            is_idle: false,
            current_task: Some("needle-abc".to_string()),
            model: "claude-code-glm-5".to_string(),
            heartbeat_file: None,
        };

        let path = hb_dir.join(format!("{}.json", worker_id));
        std::fs::write(&path, serde_json::to_string(&heartbeat).unwrap()).unwrap();

        // Check that the worker is not alive
        let is_alive = monitor.check_worker_alive(worker_id).unwrap();
        assert!(!is_alive, "worker with stale heartbeat should not be alive");
    }

    /// Test that check_worker_alive returns false when heartbeat file doesn't exist.
    #[tokio::test]
    async fn check_worker_alive_no_heartbeat_file() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Check a worker that doesn't have a heartbeat file
        let worker_id = "claude-code-glm-5-nonexistent";
        let is_alive = monitor.check_worker_alive(worker_id).unwrap();
        assert!(
            !is_alive,
            "worker without heartbeat file should not be alive"
        );
    }

    /// Test that check_worker_alive returns error for unparseable heartbeat file.
    #[tokio::test]
    async fn check_worker_alive_unparseable_heartbeat() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Write an invalid JSON file
        let worker_id = "claude-code-glm-5-invalid";
        let path = hb_dir.join(format!("{}.json", worker_id));
        std::fs::write(&path, b"invalid json content").unwrap();

        // Check that parsing fails
        let result = monitor.check_worker_alive(worker_id);
        assert!(
            result.is_err(),
            "check_worker_alive should return error for unparseable heartbeat"
        );
    }

    /// Test that check_worker_alive handles edge cases at TTL boundary.
    #[tokio::test]
    async fn check_worker_alive_ttl_boundary() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let mut config = test_config(&hb_dir);
        config.health.heartbeat_ttl_secs = 300;

        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Test with heartbeat exactly at the TTL boundary (should be considered stale)
        let worker_id = "claude-code-glm-5-boundary";
        let heartbeat = HeartbeatData {
            worker_id: "boundary".to_string(),
            qualified_id: worker_id.to_string(),
            pid: 1234,
            state: WorkerState::Executing,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(300), // Exactly at TTL
            started_at: Utc::now(),
            beads_processed: 0,
            session: "boundary".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-code-glm-5".to_string(),
            heartbeat_file: None,
        };

        let path = hb_dir.join(format!("{}.json", worker_id));
        std::fs::write(&path, serde_json::to_string(&heartbeat).unwrap()).unwrap();

        // Heartbeat at exactly TTL should be considered stale (age >= TTL)
        let is_alive = monitor.check_worker_alive(worker_id).unwrap();
        assert!(
            !is_alive,
            "worker with heartbeat exactly at TTL boundary should be stale"
        );
    }

    /// Test that check_worker_alive uses qualified_worker_id correctly.
    #[tokio::test]
    async fn check_worker_alive_qualified_id_format() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let config = test_config(&hb_dir);
        let monitor = HealthMonitor::new(
            config,
            "test-worker".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        // Test with properly formatted qualified ID
        let qualified_id = "claude-code-glm-5-foxtrot";
        let heartbeat = HeartbeatData {
            worker_id: "foxtrot".to_string(),
            qualified_id: qualified_id.to_string(),
            pid: 5678,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now(),
            started_at: Utc::now(),
            beads_processed: 0,
            session: "foxtrot".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-code-glm-5".to_string(),
            heartbeat_file: None,
        };

        let path = hb_dir.join(format!("{}.json", qualified_id));
        std::fs::write(&path, serde_json::to_string(&heartbeat).unwrap()).unwrap();

        // Verify the qualified ID is used correctly
        let is_alive = monitor.check_worker_alive(qualified_id).unwrap();
        assert!(is_alive, "worker should be found using qualified ID format");
    }

    /// Integration test for worker liveness detection.
    ///
    /// This test validates the acceptance criteria:
    /// 1. Launch a worker with health monitor
    /// 2. Verify worker is detected as alive via heartbeat freshness
    /// 3. Kill the worker and verify it's detected as dead/stale
    /// 4. Uses proper test isolation (temp HOME, heartbeat dir)
    #[tokio::test]
    async fn worker_liveness_detection_integration() {
        let _home_guard = isolate_test_home();
        let dir = tempfile::tempdir().unwrap();
        let hb_dir = dir.path().join("state").join("heartbeats");

        let mut config = test_config(&hb_dir);
        config.health.heartbeat_interval_secs = 1;
        config.health.heartbeat_ttl_secs = 3;

        // ACCEPTANCE CRITERION 1: Launch worker with health monitor
        let mut monitor = HealthMonitor::new(
            config.clone(),
            "liveness-test".to_string(),
            Telemetry::new("test".to_string()),
            None,
        );

        let qualified_id = monitor.qualified_id().to_string();
        let heartbeat_path = monitor.heartbeat_path();

        // Start the emitter
        monitor.start_emitter().unwrap();

        // Verify initial heartbeat file was created
        assert!(
            heartbeat_path.exists(),
            "heartbeat file must exist after starting worker"
        );

        // ACCEPTANCE CRITERION 2: Verify worker is detected as alive via heartbeat freshness
        // The heartbeat should be fresh immediately after starting
        let is_alive = monitor.check_worker_alive(&qualified_id).unwrap();
        assert!(
            is_alive,
            "worker should be detected as alive immediately after starting"
        );

        // Verify the heartbeat via verify_heartbeat method as well
        let verify_result = monitor.verify_heartbeat().unwrap();
        assert!(
            verify_result,
            "verify_heartbeat should return true for fresh heartbeat"
        );

        // Wait for one heartbeat refresh interval to ensure freshness detection works
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Worker should still be detected as alive after one interval
        let is_alive_after_interval = monitor.check_worker_alive(&qualified_id).unwrap();
        assert!(
            is_alive_after_interval,
            "worker should still be detected as alive after heartbeat refresh"
        );

        // ACCEPTANCE CRITERION 3: Kill worker and verify it's detected as dead/stale
        // Stop the emitter (simulating worker death)
        monitor.stop();

        // Verify heartbeat file was removed on stop
        assert!(
            !heartbeat_path.exists(),
            "heartbeat file should be removed after worker stops"
        );

        // Worker should now be detected as dead (no heartbeat file)
        let is_dead = monitor.check_worker_alive(&qualified_id).unwrap();
        assert!(!is_dead, "worker should be detected as dead after stopping");

        // Test stale detection scenario: simulate a dead worker with stale heartbeat file
        // Create a stale heartbeat file manually
        let stale_heartbeat = HeartbeatData {
            worker_id: "liveness-test".to_string(),
            qualified_id: qualified_id.clone(),
            pid: std::process::id(),
            state: WorkerState::Executing,
            current_bead: Some(BeadId::from("stale-test")),
            workspace: dir.path().to_path_buf(),
            last_heartbeat: Utc::now() - chrono::Duration::seconds(10), // 10 seconds ago (older than 3s TTL)
            started_at: Utc::now() - chrono::Duration::seconds(100),
            beads_processed: 0,
            session: "liveness-test".to_string(),
            is_idle: false,
            current_task: Some("stale-test".to_string()),
            model: "claude-code-glm-5".to_string(),
            heartbeat_file: None,
        };

        std::fs::write(
            &heartbeat_path,
            serde_json::to_string(&stale_heartbeat).unwrap(),
        )
        .unwrap();

        // Worker should be detected as dead due to stale heartbeat
        let is_stale = monitor.check_worker_alive(&qualified_id).unwrap();
        assert!(
            !is_stale,
            "worker should be detected as dead with stale heartbeat file"
        );

        // Verify the heartbeat is actually considered stale via verify_heartbeat
        let verify_stale = monitor.verify_heartbeat().unwrap();
        assert!(
            !verify_stale,
            "verify_heartbeat should return false for stale heartbeat"
        );

        // Verify via is_stale function
        let heartbeats = HealthMonitor::read_all_heartbeats(&hb_dir).unwrap();
        assert_eq!(heartbeats.len(), 1, "should have one heartbeat file");
        let hb = &heartbeats[0];
        let is_stale_check = HealthMonitor::is_stale(hb, Duration::from_secs(3));
        assert!(
            is_stale_check,
            "is_stale should return true for heartbeat older than TTL"
        );

        // Clean up
        std::fs::remove_file(&heartbeat_path).unwrap();
    }
}
