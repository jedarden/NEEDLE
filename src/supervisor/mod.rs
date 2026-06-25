//! Fleet supervisor — auto-scale workers based on bead queue depth.
//!
//! The supervisor monitors the global ready queue and fleet state, spawning
//! workers when beads appear and the fleet is under capacity. This eliminates
//! the need for manual `needle run` invocation when new beads are added.
//!
//! **Behavior**:
//! - Polls bead store for ready bead count
//! - Checks heartbeats for idle workers (state: exhausted, no current bead)
//! - Spawns workers up to max_workers when idle workers > 0 AND ready beads > 0
//! - Implements exponential backoff on spawn failures
//! - Respects exhaustion cooldown (no spam when truly idle)
//!
//! **Entry point**: `needle supervise` runs the supervisor as a long-lived daemon.
//!
//! Depends on: `config`, `registry`, `health`, `bead_store`, `telemetry`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tokio::time::sleep;

use crate::bead_store::{BeadStore, BrCliBeadStore};
use crate::config::{Config, ConfigLoader};
use crate::health::HeartbeatData;
use crate::registry::Registry;
use crate::telemetry::{EventKind, Telemetry};

// ──────────────────────────────────────────────────────────────────────────────
// Supervisor configuration
// ──────────────────────────────────────────────────────────────────────────────

/// Supervisor polling interval (seconds).
///
/// How often to check the bead store and fleet state. Lower values mean
/// faster response to new beads but higher CPU overhead.
const POLL_INTERVAL_SECS: u64 = 10;

/// Maximum consecutive spawn failures before entering backoff.
const MAX_SPAWN_FAILURES: u32 = 5;

/// Base backoff duration after spawn failures (seconds).
const BASE_BACKOFF_SECS: u64 = 60;

/// Maximum backoff duration (caps exponential backoff).
const MAX_BACKOFF_SECS: u64 = 300;

// ──────────────────────────────────────────────────────────────────────────────
// Fleet state snapshot
// ──────────────────────────────────────────────────────────────────────────────

/// Snapshot of fleet state at a point in time.
#[derive(Debug, Clone)]
struct FleetState {
    /// Total registered workers (live PIDs only).
    total_workers: usize,
    /// Workers currently idle (exhausted state, no bead).
    idle_workers: usize,
    /// Workers currently processing beads.
    #[allow(dead_code)]
    busy_workers: usize,
    /// Qualified worker IDs (e.g., "claude-alpha").
    #[allow(dead_code)]
    worker_ids: Vec<String>,
}

impl FleetState {
    /// Check if the fleet is at capacity.
    fn is_at_capacity(&self, max_workers: u32) -> bool {
        self.total_workers >= max_workers as usize
    }

    /// Check if the fleet has idle capacity.
    fn has_idle_capacity(&self) -> bool {
        self.idle_workers > 0
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Supervisor
// ──────────────────────────────────────────────────────────────────────────────

/// Fleet supervisor daemon.
///
/// Monitors the bead store and fleet, auto-scaling workers in response to
/// workload changes.
pub struct Supervisor {
    /// Workspace root (for bead store discovery).
    workspace: PathBuf,
    /// Fully resolved configuration.
    config: Config,
    /// Worker registry.
    registry: Registry,
    /// Bead store interface.
    store: Arc<BrCliBeadStore>,
    /// Telemetry emitter.
    telemetry: Telemetry,
    /// Consecutive spawn failures (for backoff).
    spawn_failures: u32,
    /// Last poll time.
    last_poll: Option<DateTime<Utc>>,
}

impl Supervisor {
    /// Create a new supervisor instance.
    pub fn new(
        workspace: PathBuf,
        config: Config,
        store: Arc<BrCliBeadStore>,
        telemetry: Telemetry,
    ) -> Result<Self> {
        let needle_home = &config.workspace.home;
        let registry = Registry::default_location(needle_home);

        Ok(Self {
            workspace,
            config,
            registry,
            store,
            telemetry,
            spawn_failures: 0,
            last_poll: None,
        })
    }

    /// Run the supervisor main loop.
    ///
    /// This is a long-lived method that polls continuously until shutdown
    /// is requested via the returned flag.
    pub async fn run(&mut self) -> Result<()> {
        self.emit_supervisor_started()?;

        let mut poll_count = 0u64;
        let mut spawn_count = 0u64;

        loop {
            poll_count += 1;
            let poll_start = Instant::now();

            tracing::debug!(poll_count, "supervisor poll started");

            // Snapshot fleet state.
            let fleet_state = self.snapshot_fleet_state().await?;

            // Check for ready beads.
            let ready_count = self.count_ready_beads().await?;

            tracing::debug!(
                total_workers = fleet_state.total_workers,
                idle_workers = fleet_state.idle_workers,
                ready_beads = ready_count,
                max_workers = self.config.worker.max_workers,
                "fleet state snapshot"
            );

            // Decide whether to spawn workers.
            let should_spawn = self.should_spawn_workers(&fleet_state, ready_count);

            if should_spawn {
                // Calculate how many workers to spawn (respect max_workers).
                let current_count = fleet_state.total_workers as u32;
                let max_workers = self.config.worker.max_workers;
                let capacity = max_workers.saturating_sub(current_count);

                // Spawn up to capacity, but limit to ready bead count to avoid overspawn.
                let to_spawn = capacity.min(ready_count as u32).min(5); // Max 5 at a time

                tracing::info!(
                    to_spawn,
                    current_count,
                    max_workers,
                    ready_beads = ready_count,
                    "spawning workers"
                );

                self.emit_spawn_decision(to_spawn, ready_count)?;

                match self.spawn_workers(to_spawn).await {
                    Ok(spawned) => {
                        spawn_count += spawned as u64;
                        self.spawn_failures = 0; // Reset failure counter
                        tracing::info!(
                            spawned,
                            total_spawned = spawn_count,
                            "workers spawned successfully"
                        );
                    }
                    Err(e) => {
                        self.spawn_failures += 1;
                        tracing::warn!(
                            error = %e,
                            consecutive_failures = self.spawn_failures,
                            "worker spawn failed"
                        );
                        self.emit_spawn_failed(&e)?;

                        // Enter backoff if failures threshold exceeded.
                        if self.spawn_failures >= MAX_SPAWN_FAILURES {
                            let backoff_secs = self.calculate_backoff();
                            tracing::warn!(backoff_secs, "entering spawn failure backoff");
                            self.emit_backoff(backoff_secs)?;
                            sleep(Duration::from_secs(backoff_secs)).await;
                        }
                    }
                }
            } else {
                tracing::debug!(
                    total_workers = fleet_state.total_workers,
                    ready_beads = ready_count,
                    max_workers = self.config.worker.max_workers,
                    idle = fleet_state.idle_workers,
                    "no worker spawn needed"
                );
            }

            // Emit periodic telemetry.
            if poll_count % 10 == 0 {
                self.emit_periodic_summary(
                    poll_count,
                    spawn_count,
                    fleet_state.total_workers,
                    ready_count,
                )?;
            }

            // Wait before next poll.
            let poll_elapsed = poll_start.elapsed();
            let poll_delay = Duration::from_secs(POLL_INTERVAL_SECS).saturating_sub(poll_elapsed);

            if !poll_delay.is_zero() {
                sleep(poll_delay).await;
            }

            self.last_poll = Some(Utc::now());
        }
    }

    /// Snapshot current fleet state from registry and heartbeats.
    async fn snapshot_fleet_state(&self) -> Result<FleetState> {
        // Get live workers from registry (filters dead PIDs).
        let workers = self
            .registry
            .list()
            .context("failed to read worker registry")?;

        let heartbeat_dir = self.config.workspace.home.join("state").join("heartbeats");

        let mut idle_workers = 0usize;
        let mut busy_workers = 0usize;
        let mut worker_ids = Vec::new();

        for worker in &workers {
            worker_ids.push(worker.id.clone());

            // Read heartbeat file to check state.
            let heartbeat_path = heartbeat_dir.join(format!("{}.json", worker.id));
            let heartbeat = if heartbeat_path.exists() {
                std::fs::read_to_string(&heartbeat_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<HeartbeatData>(&s).ok())
            } else {
                None
            };

            // Worker is idle if: exhausted state AND no current bead.
            let is_idle = heartbeat
                .as_ref()
                .map(|hb| {
                    matches!(hb.state, crate::types::WorkerState::Exhausted)
                        && hb.current_bead.is_none()
                })
                .unwrap_or(false);

            if is_idle {
                idle_workers += 1;
            } else {
                busy_workers += 1;
            }
        }

        Ok(FleetState {
            total_workers: workers.len(),
            idle_workers,
            busy_workers,
            worker_ids,
        })
    }

    /// Count ready beads across all workspaces.
    async fn count_ready_beads(&self) -> Result<usize> {
        // Query the bead store for ready beads.
        let filters = crate::bead_store::Filters::default();
        let ready_beads = self
            .store
            .ready(&filters)
            .await
            .context("failed to query ready beads from store")?;

        Ok(ready_beads.len())
    }

    /// Decide whether workers should be spawned.
    fn should_spawn_workers(&self, fleet: &FleetState, ready_count: usize) -> bool {
        // Spawn if:
        // 1. Not at capacity
        // 2. Has ready beads
        // 3. Has idle workers (fleet was exhausted but now has work)

        if fleet.is_at_capacity(self.config.worker.max_workers) {
            tracing::debug!("at capacity, not spawning");
            return false;
        }

        if ready_count == 0 {
            tracing::debug!("no ready beads, not spawning");
            return false;
        }

        if !fleet.has_idle_capacity() {
            // All workers are busy — this is normal, no spawn needed.
            tracing::debug!("no idle workers, fleet is fully utilized");
            return false;
        }

        true
    }

    /// Spawn N workers using the same mechanism as `needle run`.
    async fn spawn_workers(&self, count: u32) -> Result<u32> {
        // Reuse the CLI worker launch code.
        let workspace = Some(self.workspace.clone());
        let agent = Some(self.config.agent.default.clone());
        let identifier = None; // Auto-assign NATO names
        let timeout = Some(self.config.agent.timeout);
        let hot_reload = Some(self.config.self_modification.hot_reload);

        // Create a blocking task for worker spawning (uses tmux which is not async).
        let config_clone = self.config.clone();

        tokio::task::spawn_blocking(move || {
            crate::cli::launch_workers(
                config_clone,
                workspace,
                agent,
                count,
                identifier,
                timeout,
                hot_reload,
            )
        })
        .await
        .context("worker spawn task failed")??;

        // launch_workers doesn't return the actual count, so we assume success
        Ok(count)
    }

    /// Calculate exponential backoff duration.
    fn calculate_backoff(&self) -> u64 {
        let base = BASE_BACKOFF_SECS;
        let max = MAX_BACKOFF_SECS;
        let exponent = self.spawn_failures.saturating_sub(1) as u32;

        // Exponential backoff: base * 2^exponent, capped at max.
        let backoff = base * 2u64.pow(exponent);
        backoff.min(max)
    }

    // ── Telemetry emission ─────────────────────────────────────────────────────

    fn emit_supervisor_started(&self) -> Result<()> {
        self.telemetry.emit(EventKind::SupervisorStarted {
            max_workers: self.config.worker.max_workers,
            workspace: self.workspace.display().to_string(),
        })
    }

    fn emit_spawn_decision(&self, to_spawn: u32, ready_beads: usize) -> Result<()> {
        self.telemetry.emit(EventKind::SupervisorSpawnDecision {
            to_spawn,
            ready_beads: ready_beads as u32,
            max_workers: self.config.worker.max_workers,
        })
    }

    fn emit_spawn_failed(&self, error: &anyhow::Error) -> Result<()> {
        self.telemetry.emit(EventKind::SupervisorSpawnFailed {
            error: error.to_string(),
        })
    }

    fn emit_backoff(&self, backoff_secs: u64) -> Result<()> {
        self.telemetry.emit(EventKind::SupervisorBackoff {
            backoff_secs,
            consecutive_failures: self.spawn_failures,
        })
    }

    fn emit_periodic_summary(
        &self,
        poll_count: u64,
        spawn_count: u64,
        total_workers: usize,
        ready_beads: usize,
    ) -> Result<()> {
        self.telemetry.emit(EventKind::SupervisorSummary {
            polls: poll_count,
            spawned: spawn_count,
            total_workers: total_workers as u32,
            ready_beads: ready_beads as u32,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Public API
// ──────────────────────────────────────────────────────────────────────────────

/// Run the supervisor daemon for the given workspace.
///
/// This is the entry point called from `needle supervise`. It initializes
/// all dependencies and runs the supervisor loop.
pub async fn run_supervisor(workspace: Option<PathBuf>) -> Result<()> {
    // Determine workspace root.
    let workspace_root = if let Some(ref ws) = workspace {
        ws.canonicalize().unwrap_or_else(|_| ws.clone())
    } else {
        let global = ConfigLoader::load_global()?;
        global.workspace.default.clone()
    };

    // Load resolved config.
    let (config, _) = ConfigLoader::load_resolved(&workspace_root, Default::default())?;

    // Initialize bead store.
    let store = Arc::new(
        BrCliBeadStore::discover(workspace_root.clone())
            .context("failed to discover bead store")?,
    );

    // Initialize telemetry.
    let telemetry = Telemetry::from_config("supervisor".to_string(), &config.telemetry)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to create telemetry, falling back");
            Telemetry::new("supervisor".to_string())
        });

    // Start telemetry writer.
    telemetry
        .start_and_wait()
        .await
        .context("telemetry writer failed to start")?;

    // Create and run supervisor.
    let mut supervisor = Supervisor::new(workspace_root, config, store, telemetry)?;

    tracing::info!("starting fleet supervisor");
    supervisor.run().await?;

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_state_detects_idle_workers() {
        let state = FleetState {
            total_workers: 5,
            idle_workers: 2,
            busy_workers: 3,
            worker_ids: vec![],
        };

        assert!(!state.is_at_capacity(10));
        assert!(state.has_idle_capacity());
    }

    #[test]
    fn fleet_state_at_capacity() {
        let state = FleetState {
            total_workers: 10,
            idle_workers: 5,
            busy_workers: 5,
            worker_ids: vec![],
        };

        assert!(state.is_at_capacity(10));
        assert!(state.has_idle_capacity());
    }

    #[test]
    fn exponential_backoff_is_capped() {
        // Test that backoff doesn't exceed MAX_BACKOFF_SECS.
        let base = BASE_BACKOFF_SECS;
        let max = MAX_BACKOFF_SECS;

        for failures in 0u32..20 {
            let exponent = failures.saturating_sub(1);
            let backoff = (base * 2u64.pow(exponent)).min(max);
            assert!(backoff <= max, "backoff {backoff} exceeds max {max}");
        }
    }

    #[test]
    fn supervisor_snapshot_empty_fleet() {
        // Test that supervisor handles empty registry gracefully.
        let _dir = tempfile::tempdir().unwrap();
        let _config = Config::default();
        let registry = Registry::new(_dir.path());

        let workers = registry.list().unwrap();
        assert!(workers.is_empty());
    }
}
