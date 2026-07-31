//! Fleet supervisor — auto-scale workers based on queue depth.
//!
//! The supervisor monitors the bead store and fleet state, spawning workers
//! when ready beads appear and the fleet is under capacity. This eliminates
//! the need for manual `needle run` invocations when new beads arrive.
//!
//! The supervisor runs as a long-lived daemon, polling the ready queue at
//! a configurable interval and maintaining worker count at an appropriate level
//! based on demand.
//!
//! Depends on: `bead_store`, `config`, `registry`, `telemetry`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::bead_store::{BeadStore, BrCliBeadStore, Filters};
use crate::config::{CliOverrides, Config, ConfigLoader};
use crate::registry::{is_pid_alive, Registry};
use crate::telemetry::{EventKind, Telemetry};

/// Default interval for polling the ready queue (seconds).
const DEFAULT_POLL_INTERVAL_SECS: u64 = 10;

/// Backoff duration after spawning a worker (seconds).
/// Prevents rapid spawning cycles during bead arrival bursts.
const SPAWN_BACKOFF_SECS: u64 = 5;

/// Maximum consecutive errors before entering long backoff.
const MAX_CONSECUTIVE_ERRORS: u32 = 5;

/// Long backoff duration after error threshold (seconds).
const ERROR_BACKOFF_SECS: u64 = 60;

/// Supervisor configuration.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// Workspace to monitor.
    pub workspace: PathBuf,
    /// Maximum number of concurrent workers.
    pub max_workers: u32,
    /// Polling interval for ready queue (seconds).
    pub poll_interval_secs: u64,
    /// Agent adapter to use for spawned workers.
    pub agent: Option<String>,
    /// Agent timeout in seconds.
    pub agent_timeout: Option<u64>,
    /// Explicit override for the worker binary path (`worker.worker_binary_path`).
    /// When `None`, the supervisor resolves `std::env::current_exe()`.
    pub worker_binary_path: Option<PathBuf>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        SupervisorConfig {
            workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            max_workers: 4,
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            agent: None,
            agent_timeout: None,
            worker_binary_path: None,
        }
    }
}

/// Fleet supervisor state.
pub struct Supervisor {
    /// Supervisor configuration.
    config: SupervisorConfig,
    /// Full NEEDLE configuration.
    needle_config: Config,
    /// Bead store for polling the ready queue.
    store: Arc<dyn BeadStore>,
    /// Worker registry for tracking active workers.
    registry: Registry,
    /// Telemetry instance.
    telemetry: Telemetry,
    /// Shutdown flag for graceful termination.
    shutdown: Arc<AtomicBool>,
    /// Resolved worker spawn binary path — see [`resolve_worker_binary`].
    worker_binary: PathBuf,
}

/// Resolve the binary path `needle supervise` spawns workers from.
///
/// Prefers an explicit `worker.worker_binary_path` override; otherwise
/// resolves `std::env::current_exe()`, since the supervisor and worker are
/// the same binary. Falls back to a bare `"needle"` PATH lookup only if
/// `current_exe()` itself fails (e.g. the executable was deleted after
/// launch) — this is the previous behavior, now a last resort rather than
/// the only behavior. See GitHub issue jedarden/NEEDLE#11.
fn resolve_worker_binary(override_path: Option<&PathBuf>) -> PathBuf {
    if let Some(path) = override_path {
        return path.clone();
    }
    std::env::current_exe().unwrap_or_else(|e| {
        tracing::warn!(
            error = %e,
            "failed to resolve current_exe for worker spawn, falling back to PATH lookup of 'needle'"
        );
        PathBuf::from("needle")
    })
}

impl Supervisor {
    /// Create a new supervisor with the given configuration.
    pub fn new(config: SupervisorConfig, needle_config: Config) -> Result<Self> {
        // Initialize bead store
        let store: Arc<dyn BeadStore> = Arc::new(
            BrCliBeadStore::discover(
                config.workspace.clone(),
                None,
                Some("needle".to_string()),
                Some(env!("CARGO_PKG_VERSION").to_string()),
            )
            .context("failed to initialize bead store for supervisor")?,
        );

        // Initialize registry
        let registry = Registry::default_location(&needle_config.workspace.home);

        // Initialize telemetry
        let qualified_id = "supervisor".to_string();
        let telemetry = Telemetry::from_config(qualified_id.clone(), &needle_config.telemetry)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to create telemetry for supervisor, falling back");
                Telemetry::new(qualified_id)
            });

        // Resolve the worker spawn binary once at startup and log it, so a
        // name collision on $PATH (another tool occupying "needle") is
        // visible immediately rather than only via stalled worker heartbeats.
        // See GitHub issue jedarden/NEEDLE#11.
        let worker_binary = resolve_worker_binary(config.worker_binary_path.as_ref());
        tracing::info!(
            worker_binary = %worker_binary.display(),
            "resolved worker spawn binary"
        );

        Ok(Supervisor {
            config,
            needle_config,
            store,
            registry,
            telemetry,
            shutdown: Arc::new(AtomicBool::new(false)),
            worker_binary,
        })
    }

    /// Run the supervisor loop until shutdown is signaled.
    pub async fn run(mut self) -> Result<()> {
        // Install signal handlers for graceful shutdown
        let shutdown = self.shutdown.clone();
        #[cfg(unix)]
        {
            // Handle SIGINT and SIGTERM
            std::mem::drop(tokio::spawn(async move {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sigint =
                    signal(SignalKind::interrupt()).expect("failed to setup SIGINT handler");
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("failed to setup SIGTERM handler");

                tokio::select! {
                    _ = sigint.recv() => {
                        tracing::info!("received SIGINT, shutting down supervisor");
                        shutdown.store(true, Ordering::SeqCst);
                    }
                    _ = sigterm.recv() => {
                        tracing::info!("received SIGTERM, shutting down supervisor");
                        shutdown.store(true, Ordering::SeqCst);
                    }
                }
            }));
        }

        #[cfg(not(unix))]
        {
            // Handle Ctrl+C on non-Unix platforms
            let _ = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    tracing::info!("received Ctrl+C, shutting down supervisor");
                    shutdown.store(true, Ordering::SeqCst);
                }
            });
        }

        // Start telemetry
        self.telemetry.start();

        // Emit supervisor started event
        self.telemetry.emit(EventKind::SupervisorStarted {
            workspace: self.config.workspace.display().to_string(),
            max_workers: self.config.max_workers,
        })?;

        tracing::info!(
            workspace = %self.config.workspace.display(),
            max_workers = self.config.max_workers,
            poll_interval_secs = self.config.poll_interval_secs,
            "supervisor started"
        );

        let mut consecutive_errors = 0u32;
        let mut last_spawn_time = Instant::now();
        let mut total_spawned = 0u64;
        let mut total_polls = 0u64;

        // Main supervisor loop
        loop {
            // Check for shutdown signal
            if self.shutdown.load(Ordering::SeqCst) {
                tracing::info!("shutdown signal received, stopping supervisor");
                break;
            }

            // Enforce spawn backoff
            let backoff_remaining = last_spawn_time.elapsed().as_secs();
            if backoff_remaining < SPAWN_BACKOFF_SECS {
                let wait_secs = SPAWN_BACKOFF_SECS - backoff_remaining;
                tracing::debug!(wait_secs, "in spawn backoff, waiting before next poll");
                tokio::time::sleep(Duration::from_secs(wait_secs)).await;
                continue;
            }

            // Poll the ready queue and check fleet state
            total_polls += 1;

            // Emit summary every 60 ticks (approximately 10 minutes at default interval)
            if total_polls % 60 == 0 {
                let active_workers = self.registry.list().unwrap_or_default();
                let ready_beads = self
                    .store
                    .ready(&Filters::default())
                    .await
                    .unwrap_or_default();
                let _ = self.telemetry.emit(EventKind::SupervisorSummary {
                    polls: total_polls,
                    spawned: total_spawned,
                    total_workers: active_workers.len() as u32,
                    ready_beads: ready_beads.len() as u32,
                });
            }

            match self.tick().await {
                Ok(spawned) => {
                    consecutive_errors = 0;
                    if spawned {
                        total_spawned += 1;
                        last_spawn_time = Instant::now();
                    }
                }
                Err(e) => {
                    consecutive_errors += 1;
                    tracing::warn!(
                        error = %e,
                        consecutive_errors = consecutive_errors,
                        "supervisor tick failed"
                    );

                    // Emit spawn failed event
                    let _ = self.telemetry.emit(EventKind::SupervisorSpawnFailed {
                        error: e.to_string(),
                    });

                    // Enter long backoff if error threshold exceeded
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        tracing::warn!(
                            consecutive_errors,
                            backoff_secs = ERROR_BACKOFF_SECS,
                            "error threshold exceeded, entering long backoff"
                        );

                        // Emit backoff event
                        let _ = self.telemetry.emit(EventKind::SupervisorBackoff {
                            backoff_secs: ERROR_BACKOFF_SECS,
                            consecutive_failures: consecutive_errors,
                        });

                        tokio::time::sleep(Duration::from_secs(ERROR_BACKOFF_SECS)).await;
                    }
                }
            }

            // Wait before next poll
            tokio::time::sleep(Duration::from_secs(self.config.poll_interval_secs)).await;
        }

        // Emit supervisor stopped event
        self.telemetry.emit(EventKind::SupervisorStopped {
            reason: "shutdown_requested".to_string(),
        })?;

        // Shutdown telemetry
        self.telemetry.shutdown().await;

        tracing::info!("supervisor stopped");
        Ok(())
    }

    /// Single supervisor tick: poll queue and spawn workers if needed.
    ///
    /// Returns true if a worker was spawned, false otherwise.
    async fn tick(&mut self) -> Result<bool> {
        reap_zombie_children();

        // Get active workers
        let active_workers = self.registry.list().unwrap_or_default();
        let active_count = active_workers.len() as u32;

        // Filter out dead workers (PID no longer alive)
        let alive_workers: Vec<_> = active_workers
            .iter()
            .filter(|w| is_pid_alive(w.pid))
            .collect();

        // Update registry to remove dead workers
        if alive_workers.len() < active_workers.len() {
            tracing::debug!(
                removed = active_count - alive_workers.len() as u32,
                "cleaning up dead workers from registry"
            );
            for worker in &active_workers {
                if !is_pid_alive(worker.pid) {
                    let _ = self.registry.deregister(&worker.id);
                }
            }
        }

        let alive_count = alive_workers.len() as u32;

        // Check if we're at capacity
        if alive_count >= self.config.max_workers {
            tracing::debug!(
                alive_count,
                max_workers = self.config.max_workers,
                "fleet at capacity, no spawning needed"
            );
            return Ok(false);
        }

        // Poll the ready queue
        let filters = Filters::default();
        let ready_beads = self.store.ready(&filters).await?;

        // If queue is empty, no need to spawn
        if ready_beads.is_empty() {
            tracing::debug!("ready queue empty, no spawning needed");
            return Ok(false);
        }

        // We have ready beads and capacity: spawn a worker
        let ready_count = ready_beads.len();
        tracing::info!(
            ready_beads = ready_count,
            active_workers = alive_count,
            max_workers = self.config.max_workers,
            "ready beads detected with capacity, spawning worker"
        );

        // Emit spawn decision event
        self.telemetry.emit(EventKind::SupervisorSpawnDecision {
            to_spawn: 1,
            ready_beads: ready_count as u32,
            max_workers: self.config.max_workers,
        })?;

        // Spawn the worker
        self.spawn_worker(ready_count).await?;

        Ok(true)
    }

    /// Spawn a new worker process.
    async fn spawn_worker(&self, ready_count: usize) -> Result<()> {
        // Phase 1: resource check before worker spawn.
        // Check system resources (CPU and memory) before spawning a worker.
        // If saturated, retry with bounded backoff rather than proceeding into
        // a launch that may be killed by the OS due to resource pressure.
        const MAX_RESOURCE_WAIT_SECS: u64 = 120; // Maximum total wait time
        const RESOURCE_RETRY_DELAY_SECS: u64 = 5; // Initial retry delay
        let mut resource_wait_total = 0u64;
        let mut resource_retry_delay = RESOURCE_RETRY_DELAY_SECS;

        loop {
            match crate::rate_limit::RateLimiter::check_system_resources_for_launch(
                self.needle_config.worker.cpu_load_warn,
                self.needle_config.worker.memory_free_warn_mb,
                &self.telemetry,
            ) {
                Ok(()) => {
                    // Resources are acceptable, proceed to spawn
                    break;
                }
                Err(e) => {
                    if resource_wait_total >= MAX_RESOURCE_WAIT_SECS {
                        // Still saturated after max wait, fail the spawn explicitly
                        self.telemetry.emit(EventKind::SupervisorSpawnFailed {
                            error: format!(
                                "system still saturated after {}s wait: {}",
                                MAX_RESOURCE_WAIT_SECS, e
                            ),
                        })?;
                        bail!(
                            "worker spawn deferred {} times ({}s total wait), system still saturated: {}. Spawn aborted — retry when load drops",
                            resource_wait_total / resource_retry_delay,
                            resource_wait_total,
                            e
                        );
                    }

                    // Resources are saturated, wait and retry
                    tracing::warn!(
                        error = %e,
                        wait_secs = resource_retry_delay,
                        total_waited_secs = resource_wait_total,
                        "system resources saturated, deferring worker spawn"
                    );

                    self.telemetry.emit(EventKind::SupervisorSpawnFailed {
                        error: format!("system saturated: {}", e),
                    })?;

                    tokio::time::sleep(Duration::from_secs(resource_retry_delay)).await;
                    resource_wait_total += resource_retry_delay;

                    // Exponential backoff with cap at 30 seconds
                    resource_retry_delay = std::cmp::min(resource_retry_delay * 2, 30);
                }
            }
        }

        let worker_id = self.generate_worker_id()?;
        let agent_name = self
            .config
            .agent
            .as_ref()
            .unwrap_or(&self.needle_config.agent.default)
            .clone();

        tracing::info!(
            worker_id = %worker_id,
            agent = %agent_name,
            worker_binary = %self.worker_binary.display(),
            "spawning worker"
        );

        // Build the needle run command. Uses the resolved worker binary path
        // (current_exe() by default) rather than a bare PATH lookup of
        // "needle" — see GitHub issue jedarden/NEEDLE#11.
        let mut cmd = std::process::Command::new(&self.worker_binary);
        cmd.arg("run")
            .arg("--workspace")
            .arg(&self.config.workspace)
            .arg("--agent")
            .arg(&agent_name)
            .arg("--identifier")
            .arg(&worker_id)
            .arg("--count")
            .arg("1");

        if let Some(timeout) = self.config.agent_timeout {
            cmd.arg("--timeout").arg(timeout.to_string());
        }

        // Spawn in background (detached process)
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);

            // On Unix, use setsid to create a new session
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }

        // Spawn the worker process (platform-agnostic)
        cmd.spawn()?;

        // Emit worker spawned event
        self.telemetry.emit(EventKind::SupervisorWorkerSpawned {
            ready_count,
            total_spawned: 1,
        })?;

        tracing::info!(worker_id = %worker_id, "worker spawned successfully");
        Ok(())
    }

    /// Generate a unique worker identifier not currently in use.
    fn generate_worker_id(&self) -> Result<String> {
        const NATO_ALPHABET: &[&str] = &[
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
            "juliet", "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo",
            "sierra", "tango", "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
        ];

        let active = self.registry.list().unwrap_or_default();
        let occupied: std::collections::HashSet<String> = active
            .iter()
            .map(|w| w.id.split('-').next_back().unwrap_or(&w.id).to_string())
            .collect();

        for name in NATO_ALPHABET {
            if !occupied.contains(*name) {
                return Ok(name.to_string());
            }
        }

        // Fallback: use a numbered suffix
        for i in 1..=100 {
            let name = format!("worker{}", i);
            if !occupied.contains(&name) {
                return Ok(name);
            }
        }

        anyhow::bail!("unable to generate unique worker identifier")
    }
}

/// Reap any exited direct children of this process (workers spawned by
/// `Supervisor::spawn_worker`) so they don't accumulate as `<defunct>`
/// zombies for the lifetime of the supervisor daemon.
///
/// Safe to call unconditionally every tick: `needle supervise` only ever
/// directly spawns worker processes — gate commands and dispatch
/// subprocesses are spawned by the *worker* process, a separate PID tree —
/// so a blind `waitpid(-1, ...)` here cannot race with `.wait()` calls
/// elsewhere in the codebase (`dispatch`/`telemetry`/`canary`), which all
/// run in different processes. See ADR-010 / GH #12.
#[cfg(unix)]
fn reap_zombie_children() {
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: waitpid(-1, &mut status, WNOHANG) reaps any already-exited
        // direct child without blocking; `status` is a valid stack-local out
        // parameter for the duration of the call.
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        match pid {
            0 => break,          // No exited children ready to reap right now.
            n if n < 0 => break, // ECHILD (no children) or another errno; stop for this tick.
            n => tracing::debug!(reaped_pid = n, "reaped exited worker child"),
        }
    }
}

#[cfg(not(unix))]
fn reap_zombie_children() {}

/// Run the supervisor from the CLI.
///
/// This is the entry point for `needle supervise`.
pub async fn run_supervisor(workspace_opt: Option<PathBuf>) -> Result<()> {
    // Determine workspace
    let workspace_root = if let Some(ws) = workspace_opt {
        ws.canonicalize().unwrap_or_else(|_| ws.clone())
    } else {
        let global = ConfigLoader::load_global()?;
        global.workspace.default.clone()
    };

    // Load full config
    let cli_overrides = CliOverrides {
        workspace: Some(workspace_root.clone()),
        ..Default::default()
    };
    let (config, _) = ConfigLoader::load_resolved(&workspace_root, cli_overrides)?;

    // Build supervisor config
    let supervisor_config = SupervisorConfig {
        workspace: workspace_root.clone(),
        max_workers: config.worker.max_workers,
        poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
        agent: Some(config.agent.default.clone()),
        agent_timeout: Some(config.agent.timeout),
        worker_binary_path: config.worker.worker_binary_path.clone(),
    };

    // Create and run supervisor
    let supervisor = Supervisor::new(supervisor_config, config)?;
    supervisor.run().await
}

// ─── Tests ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_config_default_is_valid() {
        let config = SupervisorConfig::default();
        assert!(config.workspace.exists() || config.workspace == std::path::Path::new("."));
        assert!(config.max_workers > 0);
        assert!(config.poll_interval_secs > 0);
    }

    #[test]
    fn nato_alphabet_is_complete() {
        const NATO_ALPHABET: &[&str] = &[
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
            "juliet", "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo",
            "sierra", "tango", "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
        ];
        assert_eq!(NATO_ALPHABET.len(), 26);
        assert_eq!(NATO_ALPHABET[0], "alpha");
        assert_eq!(NATO_ALPHABET[25], "zulu");
    }

    // ── resolve_worker_binary tests (GitHub issue jedarden/NEEDLE#11) ──

    #[test]
    fn resolve_worker_binary_uses_explicit_override_when_set() {
        let override_path = PathBuf::from("/opt/custom/needle-wrapper");
        let resolved = resolve_worker_binary(Some(&override_path));
        assert_eq!(resolved, override_path);
    }

    #[test]
    fn resolve_worker_binary_defaults_to_current_exe() {
        // No override — must resolve to the actual running test binary's
        // path, not a bare "needle" PATH lookup (the pre-#11 behavior).
        let resolved = resolve_worker_binary(None);
        let expected = std::env::current_exe().unwrap();
        assert_eq!(resolved, expected);
        assert_ne!(
            resolved,
            PathBuf::from("needle"),
            "must not fall back to a bare PATH lookup when current_exe() succeeds"
        );
    }

    #[test]
    fn supervisor_config_default_has_no_worker_binary_override() {
        assert_eq!(SupervisorConfig::default().worker_binary_path, None);
    }

    // ── reap_zombie_children tests (ADR-010 / GitHub issue jedarden/NEEDLE#12) ──

    #[cfg(unix)]
    #[test]
    fn reap_zombie_children_reaps_an_exited_child() {
        // Spawn a real short-lived child directly (no setsid/detach — this
        // test process is its real parent, matching what reap_zombie_children
        // assumes: it reaps ITS OWN direct children).
        let child = std::process::Command::new("true")
            .spawn()
            .expect("failed to spawn `true`");
        let pid = child.id();

        // Wait for it to actually exit (without reaping it — do not call
        // child.wait() here, that would reap it ourselves and defeat the test).
        let stat_path = format!("/proc/{pid}/stat");
        let mut became_zombie = false;
        for _ in 0..200 {
            if let Ok(stat) = std::fs::read_to_string(&stat_path) {
                if let Some(after_comm) = stat.rfind(')') {
                    if stat[after_comm + 1..].trim_start().starts_with('Z') {
                        became_zombie = true;
                        break;
                    }
                }
            } else {
                // Already reaped by something else, or /proc entry gone —
                // can't validate the pre-condition; skip rather than false-fail.
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            became_zombie,
            "child did not reach zombie state before timeout — test precondition not met"
        );

        reap_zombie_children();

        // After reaping, /proc/<pid> should no longer exist.
        assert!(
            !std::path::Path::new(&stat_path).exists(),
            "child was not reaped: {stat_path} still exists"
        );

        // Prevent a double-wait/drop warning: the child is already reaped by
        // our sweep, so explicitly forget rather than calling child.wait().
        std::mem::forget(child);
    }

    #[cfg(unix)]
    #[test]
    fn reap_zombie_children_is_a_noop_with_no_exited_children() {
        // No children spawned by this test — should return immediately
        // without panicking or blocking, regardless of ECHILD vs pid==0.
        reap_zombie_children();
    }
}
