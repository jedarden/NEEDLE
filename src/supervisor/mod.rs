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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::bead_store::{BeadStore, BrCliBeadStore, Filters};
use crate::config::{Config, ConfigLoader, CliOverrides};
use crate::registry::{Registry, WorkerEntry};
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
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        SupervisorConfig {
            workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            max_workers: 4,
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            agent: None,
            agent_timeout: None,
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
}

impl Supervisor {
    /// Create a new supervisor with the given configuration.
    pub fn new(config: SupervisorConfig, needle_config: Config) -> Result<Self> {
        // Initialize bead store
        let store: Arc<dyn BeadStore> = Arc::new(
            BrCliBeadStore::discover(config.workspace.clone())
                .context("failed to initialize bead store for supervisor")?
        );

        // Initialize registry
        let registry = Registry::default_location(&needle_config.workspace.home);

        // Initialize telemetry
        let qualified_id = "supervisor".to_string();
        let telemetry = Telemetry::from_config(qualified_id, &needle_config.telemetry)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to create telemetry for supervisor, falling back");
                Telemetry::new(qualified_id)
            });

        Ok(Supervisor {
            config,
            needle_config,
            store,
            registry,
            telemetry,
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Run the supervisor loop until shutdown is signaled.
    pub async fn run(mut self) -> Result<()> {
        // Start telemetry
        self.telemetry.start().await;

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
                tracing::debug!(
                    wait_secs,
                    "in spawn backoff, waiting before next poll"
                );
                tokio::time::sleep(Duration::from_secs(wait_secs)).await;
                continue;
            }

            // Poll the ready queue and check fleet state
            match self.tick().await {
                Ok(spawned) => {
                    consecutive_errors = 0;
                    if spawned {
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

                    // Emit error event
                    let _ = self.telemetry.emit(EventKind::SupervisorError {
                        error_message: e.to_string(),
                        consecutive_errors,
                    });

                    // Enter long backoff if error threshold exceeded
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        tracing::warn!(
                            consecutive_errors,
                            backoff_secs = ERROR_BACKOFF_SECS,
                            "error threshold exceeded, entering long backoff"
                        );
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
        self.telemetry.emit(EventKind::SupervisorSpawn {
            ready_count: ready_count as u32,
            active_workers: alive_count,
        })?;

        // Spawn the worker
        self.spawn_worker().await?;

        Ok(true)
    }

    /// Spawn a new worker process.
    async fn spawn_worker(&self) -> Result<()> {
        let worker_id = self.generate_worker_id()?;
        let agent_name = self.config.agent
            .as_ref()
            .unwrap_or(&self.needle_config.agent.default)
            .clone();

        tracing::info!(worker_id = %worker_id, agent = %agent_name, "spawning worker");

        // Build the needle run command
        let mut cmd = std::process::Command::new("needle");
        cmd.arg("run")
            .arg("--workspace").arg(&self.config.workspace)
            .arg("--agent").arg(&agent_name)
            .arg("--identifier").arg(&worker_id)
            .arg("--count").arg("1");

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
        }

        #[cfg(not(unix))]
        {
            cmd.spawn();
        }

        #[cfg(unix)]
        {
            // On Unix, use setsid to create a new session
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
            cmd.spawn()?;
        }

        tracing::info!(worker_id = %worker_id, "worker spawned successfully");
        Ok(())
    }

    /// Generate a unique worker identifier not currently in use.
    fn generate_worker_id(&self) -> Result<String> {
        const NATO_ALPHABET: &[&str] = &[
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
            "hotel", "india", "juliet", "kilo", "lima", "mike", "november",
            "oscar", "papa", "quebec", "romeo", "sierra", "tango", "uniform",
            "victor", "whiskey", "xray", "yankee", "zulu",
        ];

        let active = self.registry.list().unwrap_or_default();
        let occupied: std::collections::HashSet<String> = active
            .iter()
            .map(|w| w.id.split('-').last().unwrap_or(&w.id).to_string())
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

/// Check if a PID corresponds to a live process.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use std::process::Command;
        // Use kill(0) to check if process exists
        Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        // On non-Unix, assume alive (no portable check)
        true
    }
}

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
        assert!(config.workspace.exists() || config.workspace == PathBuf::from("."));
        assert!(config.max_workers > 0);
        assert!(config.poll_interval_secs > 0);
    }

    #[test]
    fn nato_alphabet_is_complete() {
        const NATO_ALPHABET: &[&str] = &[
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
            "hotel", "india", "juliet", "kilo", "lima", "mike", "november",
            "oscar", "papa", "quebec", "romeo", "sierra", "tango", "uniform",
            "victor", "whiskey", "xray", "yankee", "zulu",
        ];
        assert_eq!(NATO_ALPHABET.len(), 26);
        assert_eq!(NATO_ALPHABET[0], "alpha");
        assert_eq!(NATO_ALPHABET[25], "zulu");
    }
}
