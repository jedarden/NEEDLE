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
// HeartbeatData — on-disk JSON structure
// ──────────────────────────────────────────────────────────────────────────────

/// Data written to the heartbeat JSON file on disk.
///
/// Path: `~/.needle/state/heartbeats/<qualified-id>.json`
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
        let heartbeat_dir = config.workspace.home.join("state").join("heartbeats");
        let heartbeat_interval = Duration::from_secs(config.health.heartbeat_interval_secs);
        let heartbeat_ttl = Duration::from_secs(config.health.heartbeat_ttl_secs);
        let qualified_id = format!("{}-{}", config.agent.default, worker_name);

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
            })),
            shutdown: shutdown.unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
            emitter_handle: None,
        }
    }

    /// Start the background heartbeat emitter thread.
    ///
    /// The thread writes a heartbeat JSON file every `heartbeat_interval` until
    /// `stop()` is called.
    pub fn start_emitter(&mut self) -> Result<()> {
        // Ensure heartbeat directory exists.
        std::fs::create_dir_all(&self.heartbeat_dir).with_context(|| {
            format!(
                "failed to create heartbeat directory: {}",
                self.heartbeat_dir.display()
            )
        })?;

        // Write the initial heartbeat immediately.
        self.write_heartbeat()?;

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

    /// Update the beads_processed count visible to the heartbeat emitter.
    pub fn update_beads_processed(&self, count: u64) {
        if let Ok(mut guard) = self.shared_state.lock() {
            guard.beads_processed = count;
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
        let path = self.heartbeat_path();
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "failed to remove heartbeat file on shutdown"
                );
            } else {
                tracing::debug!(path = %path.display(), "heartbeat file removed");
            }
        }
    }

    /// Path to this worker's heartbeat file.
    ///
    /// Keyed by fully-qualified identity (`{adapter}-{worker_id}`) to prevent
    /// collisions when workers from different adapter pools share a NATO name.
    pub fn heartbeat_path(&self) -> PathBuf {
        self.heartbeat_dir
            .join(format!("{}.json", self.qualified_id))
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
    /// Uses `kill -0` semantics: sends signal 0 to check existence without
    /// actually sending a signal.
    pub fn check_pid_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
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

    // ── Internal ────────────────────────────────────────────────────────────

    /// Write a heartbeat file atomically (write temp, then rename).
    fn write_heartbeat(&self) -> Result<()> {
        let (state, current_bead, beads_processed, current_workspace) = {
            let guard = self
                .shared_state
                .lock()
                .map_err(|e| anyhow::anyhow!("shared state lock poisoned: {e}"))?;
            (
                guard.state.clone(),
                guard.current_bead.clone(),
                guard.beads_processed,
                guard.current_workspace.clone(),
            )
        };

        // Use the current bead's workspace if set, otherwise fall back to home workspace.
        let effective_workspace = current_workspace.unwrap_or_else(|| self.workspace.clone());

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

/// Background emitter loop. Writes heartbeat at each interval.
///
/// Circuit breaker: after `max_consecutive_failures` consecutive write failures
/// the loop sets the shutdown flag and exits so the worker terminates instead of
/// spinning indefinitely.
///
/// Backoff: each consecutive failure doubles the inter-attempt sleep, capped at
/// [`MAX_HEARTBEAT_BACKOFF`].
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

    loop {
        std::thread::sleep(current_sleep);

        if shutdown.load(Ordering::SeqCst) {
            tracing::debug!(worker = %worker_id, "heartbeat emitter shutting down");
            return;
        }

        let (state, current_bead, beads_processed, current_workspace) = match shared_state.lock() {
            Ok(guard) => (
                guard.state.clone(),
                guard.current_bead.clone(),
                guard.beads_processed,
                guard.current_workspace.clone(),
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
            heartbeat_file: None,
        };

        let path = heartbeat_dir.join(format!("{}.json", qualified_id));
        let tmp_path = path.with_extension("json.tmp");

        let write_result: anyhow::Result<()> = (|| {
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

    fn test_config(heartbeat_dir: &Path) -> Config {
        let mut config = Config::default();
        config.workspace.home = heartbeat_dir
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        config.health.heartbeat_interval_secs = 1;
        config.health.heartbeat_ttl_secs = 5;
        config
    }

    #[tokio::test]
    async fn heartbeat_file_written_on_start() {
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

        // Wait for the emitter to write a new heartbeat.
        std::thread::sleep(Duration::from_millis(1500));

        let content = std::fs::read_to_string(monitor.heartbeat_path()).unwrap();
        let data: HeartbeatData = serde_json::from_str(&content).unwrap();
        assert_eq!(data.state, WorkerState::Executing);
        assert_eq!(data.current_bead, Some(BeadId::from("needle-abc")));
        assert_eq!(data.beads_processed, 5);

        monitor.stop();
    }

    #[tokio::test]
    async fn heartbeat_file_removed_on_stop() {
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
            std::thread::sleep(Duration::from_millis(100));
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

        let shared_state = Arc::new(Mutex::new(SharedHeartbeatState {
            state: WorkerState::Selecting,
            current_bead: None,
            beads_processed: 0,
            current_workspace: None,
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

        // Wait for emitter to write.
        std::thread::sleep(Duration::from_millis(1500));

        let content1_updated = std::fs::read_to_string(&path1).unwrap();
        let data1_updated: HeartbeatData = serde_json::from_str(&content1_updated).unwrap();
        assert_eq!(data1_updated.beads_processed, 100);

        let content2_updated = std::fs::read_to_string(&path2).unwrap();
        let data2_updated: HeartbeatData = serde_json::from_str(&content2_updated).unwrap();
        assert_eq!(data2_updated.beads_processed, 200);

        monitor1.stop();
        monitor2.stop();
    }
}
