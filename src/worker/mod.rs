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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::bead_store::BeadStore;
use crate::canary::CanaryRunner;
use crate::claim::Claimer;
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
    retry_count: u32,
    beads_processed: u64,
    shutdown: Arc<AtomicBool>,
    last_error: Option<anyhow::Error>,
    boot_time: Option<Instant>,

    // Transient fields — pass data between state handlers within a single cycle.
    built_prompt: Option<BuiltPrompt>,
    exec_output: Option<(AgentOutcome, bool)>,
    /// Effort tracking data for the current bead cycle.
    last_effort: Option<EffortData>,
}

impl Worker {
    /// Construct a worker from config, a worker name, and a bead store implementation.
    pub fn new(config: Config, worker_name: String, store: Arc<dyn BeadStore>) -> Self {
        // Create a single telemetry instance with hooks (if configured) and share
        // clones with all sub-components so that hook sinks receive every event.
        let telemetry = Telemetry::from_config(worker_name.clone(), &config.telemetry)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to create hook-enabled telemetry, falling back");
                Telemetry::new(worker_name.clone())
            });
        let strand_registry = Registry::default_location(&config.workspace.home);
        let strands =
            StrandRunner::from_config(&config, &worker_name, strand_registry, telemetry.clone());
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
        let health = HealthMonitor::new(config.clone(), worker_name.clone(), telemetry.clone());
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
        let beads_processed = registry
            .list()
            .ok()
            .and_then(|workers| workers.into_iter().find(|w| w.id == worker_name))
            .map(|entry| entry.beads_processed)
            .unwrap_or(0);

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
            retry_count: 0,
            beads_processed,
            shutdown: Arc::new(AtomicBool::new(false)),
            last_error: None,
            boot_time: None,
            built_prompt: None,
            exec_output: None,
            last_effort: None,
        }
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

        // Install signal handlers.
        self.install_signal_handlers();

        loop {
            // Check for shutdown signal between states.
            if self.shutdown.load(Ordering::SeqCst) {
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
                        return self.stop("signal received").await;
                    }
                    WorkerState::Stopped | WorkerState::Exhausted | WorkerState::Errored => {
                        return self.stop("signal received").await;
                    }
                    WorkerState::Booting => {
                        return self.stop("signal received during boot").await;
                    }
                }
            }

            match self.state {
                WorkerState::Selecting => self.do_select().await?,
                WorkerState::Claiming => self.do_claim().await?,
                WorkerState::Retrying => self.do_retry()?,
                WorkerState::Building => self.do_build()?,
                WorkerState::Dispatching => self.do_dispatch().await?,
                WorkerState::Executing => self.do_execute().await?,
                WorkerState::Handling => self.do_handle().await?,
                WorkerState::Logging => self.do_log()?,
                WorkerState::Exhausted => {
                    return self.handle_exhausted().await;
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
                    // Best-effort stop heartbeat on error.
                    self.health.stop();
                    // Best-effort deregister on error.
                    if let Err(e) = self.registry.deregister(&self.worker_name) {
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
    fn boot(&mut self) -> Result<()> {
        self.boot_time = Some(Instant::now());

        // Validate configuration.
        let errors = ConfigLoader::validate(&self.config);
        if !errors.is_empty() {
            let msg = errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            bail!("config validation failed: {msg}");
        }

        // Emit worker started event.
        self.telemetry.emit(EventKind::WorkerStarted {
            worker_name: self.worker_name.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        })?;

        // Register in worker state registry.
        let entry = WorkerEntry {
            id: self.worker_name.clone(),
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

        // Start heartbeat emitter (background thread).
        if let Err(e) = self.health.start_emitter() {
            tracing::warn!(error = %e, "failed to start heartbeat emitter");
        }

        self.set_state(WorkerState::Selecting)?;

        tracing::info!(
            worker = %self.worker_name,
            strands = ?self.strands.strand_names(),
            "worker booted"
        );

        Ok(())
    }

    // ── Signal handling ─────────────────────────────────────────────────────

    /// Install SIGTERM and SIGINT handlers that set the shutdown flag.
    fn install_signal_handlers(&self) {
        let shutdown = self.shutdown.clone();

        // SIGINT (Ctrl-C)
        let shutdown_int = shutdown.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!("received SIGINT, initiating graceful shutdown");
                shutdown_int.store(true, Ordering::SeqCst);
            }
        });

        // SIGTERM (Unix only)
        #[cfg(unix)]
        {
            let shutdown_term = shutdown;
            tokio::spawn(async move {
                let mut signal =
                    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to install SIGTERM handler");
                            return;
                        }
                    };
                signal.recv().await;
                tracing::info!("received SIGTERM, initiating graceful shutdown");
                shutdown_term.store(true, Ordering::SeqCst);
            });
        }
    }

    // ── State handlers ──────────────────────────────────────────────────────

    /// SELECTING: run strand waterfall to find a candidate bead.
    async fn do_select(&mut self) -> Result<()> {
        // Clear per-cycle state.
        self.exclusion_set.clear();
        self.retry_count = 0;
        self.current_bead = None;

        // Restore home store if it was swapped for a remote workspace.
        self.restore_home_store();

        self.health.update_state(&WorkerState::Selecting, None);

        let candidate = self.strands.select(self.store.as_ref()).await?;

        match candidate {
            Some(bead) => {
                tracing::debug!(bead_id = %bead.id, "candidate found");

                // If the bead is from a remote workspace (found by Explore),
                // swap the active store so claim/show/release operate on the
                // correct workspace. Only switch if the workspace has a real
                // .beads/ directory — avoids false triggers from mock/stub beads.
                let bead_ws = &bead.workspace;
                if !is_workspace_unset(bead_ws)
                    && bead_ws != &self.config.workspace.default
                    && bead_ws.join(".beads").is_dir()
                {
                    tracing::info!(
                        bead_id = %bead.id,
                        remote_workspace = %bead_ws.display(),
                        "bead is from remote workspace, switching store"
                    );
                    self.switch_store_to(bead_ws)?;
                }

                self.current_bead = Some(bead);
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
        self.claimer = Claimer::new(
            remote_store,
            std::path::PathBuf::from("/tmp"),
            self.config.worker.max_claim_retries,
            100,
            self.telemetry.clone(),
        );
        Ok(())
    }

    /// Restore the home workspace store if it was swapped for a remote bead.
    fn restore_home_store(&mut self) {
        if !Arc::ptr_eq(&self.store, &self.home_store) {
            tracing::debug!("restoring home workspace store");
            self.store = self.home_store.clone();
            self.claimer = Claimer::new(
                self.home_store.clone(),
                std::path::PathBuf::from("/tmp"),
                self.config.worker.max_claim_retries,
                100,
                self.telemetry.clone(),
            );
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

        let claim = self.claimer.claim_one(&bead_id, &self.worker_name).await?;

        match claim {
            ClaimResult::Claimed(mut bead) => {
                tracing::info!(bead_id = %bead.id, title = %bead.title, "claimed bead");
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
                self.set_state(WorkerState::Building)?;
            }
            ClaimResult::RaceLost { claimed_by } => {
                tracing::debug!(bead_id = %bead_id, %claimed_by, "claim race lost");
                self.exclusion_set.insert(bead_id);
                self.retry_count += 1;
                self.set_state(WorkerState::Retrying)?;
            }
            ClaimResult::NotClaimable { reason } => {
                tracing::debug!(bead_id = %bead_id, %reason, "bead not claimable");
                self.exclusion_set.insert(bead_id);
                self.current_bead = None;
                self.set_state(WorkerState::Selecting)?;
            }
        }

        Ok(())
    }

    /// RETRYING: decide whether to retry claiming or move on.
    fn do_retry(&mut self) -> Result<()> {
        if self.retry_count < self.config.worker.max_claim_retries {
            // Try the next candidate from the same strand cycle.
            self.set_state(WorkerState::Selecting)?;
        } else {
            tracing::debug!(
                retry_count = self.retry_count,
                "max claim retries exceeded, moving to next strand cycle"
            );
            self.retry_count = 0;
            self.exclusion_set.clear();
            self.current_bead = None;
            self.set_state(WorkerState::Selecting)?;
        }
        Ok(())
    }

    /// BUILDING: construct prompt from claimed bead.
    fn do_build(&mut self) -> Result<()> {
        let bead = match self.current_bead {
            Some(ref b) => b,
            None => {
                bail!("BUILDING state without current_bead — invariant violated");
            }
        };

        let build_ws = if is_workspace_unset(&bead.workspace) {
            &self.config.workspace.default
        } else {
            &bead.workspace
        };
        let prompt = self
            .prompt_builder
            .build_pluck(bead, build_ws, &self.worker_name)?;

        // Store the prompt for the dispatch phase. We use a transient field pattern:
        // the prompt is passed via self.built_prompt.
        self.built_prompt = Some(prompt);
        self.set_state(WorkerState::Dispatching)?;
        Ok(())
    }

    /// DISPATCHING: check rate limits, resolve adapter, and prepare for execution.
    async fn do_dispatch(&mut self) -> Result<()> {
        let bead = match self.current_bead {
            Some(ref b) => b,
            None => {
                bail!("DISPATCHING state without current_bead — invariant violated");
            }
        };

        self.health
            .update_state(&WorkerState::Dispatching, Some(&bead.id));

        // Check rate limits before dispatching.
        let adapter = self.resolve_adapter()?;
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
            let result = self
                .dispatcher
                .dispatch(&bead.id, &prompt, &adapter, dispatch_ws)
                .await?;
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

        let handler_result = self
            .outcome_handler
            .handle(self.store.as_ref(), &bead, &output, was_interrupted)
            .await?;

        // Evaluate for mitosis after failure — the bead has already been
        // released and failure count incremented by the outcome handler.
        if handler_result.outcome == Outcome::Failure {
            let workspace = if is_workspace_unset(&bead.workspace) {
                self.config.workspace.default.clone()
            } else {
                bead.workspace.clone()
            };
            match self
                .mitosis_evaluator
                .evaluate(
                    self.store.as_ref(),
                    &bead,
                    &workspace,
                    &self.dispatcher,
                    &self.prompt_builder,
                    &self.config.agent.default,
                )
                .await
            {
                Ok(crate::mitosis::MitosisResult::Split { children }) => {
                    tracing::info!(
                        bead_id = %bead.id,
                        children = children.len(),
                        "mitosis created child beads — parent blocked"
                    );
                }
                Ok(crate::mitosis::MitosisResult::NotSplittable) => {
                    tracing::debug!(bead_id = %bead.id, "mitosis: bead is single task");
                }
                Ok(crate::mitosis::MitosisResult::Skipped { reason }) => {
                    tracing::debug!(
                        bead_id = %bead.id,
                        reason = %reason,
                        "mitosis skipped"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %e,
                        "mitosis evaluation failed (bead already released)"
                    );
                }
            }
        }

        if was_interrupted {
            // After handling interrupted outcome, stop the worker.
            self.set_state(WorkerState::Stopped)?;
        } else {
            self.set_state(WorkerState::Logging)?;
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

        // Update heartbeat with new bead count.
        self.health.update_beads_processed(self.beads_processed);

        // Update registry with current beads_processed count (best-effort).
        if let Err(e) = self
            .registry
            .update_beads_processed(&self.worker_name, self.beads_processed)
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
                tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
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

        self.telemetry.emit(EventKind::WorkerStopped {
            reason: reason.to_string(),
            beads_processed: self.beads_processed,
            uptime_secs: uptime,
        })?;

        // Stop heartbeat emitter and remove heartbeat file.
        self.health.stop();

        // Deregister from worker state registry (best-effort).
        if let Err(e) = self.registry.deregister(&self.worker_name) {
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

    /// Transition to a new state, emitting telemetry and updating heartbeat.
    fn set_state(&mut self, to: WorkerState) -> Result<()> {
        let from = self.state.clone();
        tracing::debug!(from = %from, to = %to, "state transition");
        self.telemetry.emit(EventKind::StateTransition {
            from,
            to: to.clone(),
        })?;
        // Update heartbeat shared state with the new worker state.
        let current_bead_id = self.current_bead.as_ref().map(|b| &b.id);
        self.health.update_state(&to, current_bead_id);
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

        worker.do_retry().unwrap();

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

        worker.do_retry().unwrap();

        assert_eq!(*worker.state(), WorkerState::Selecting);
        assert_eq!(worker.retry_count, 0);
        assert!(worker.exclusion_set.is_empty());
        assert!(worker.current_bead.is_none());
    }

    // ── do_build tests ──

    #[tokio::test]
    async fn do_build_without_bead_is_invariant_error() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.state = WorkerState::Building;
        worker.current_bead = None;

        let result = worker.do_build();
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

        worker.do_build().unwrap();

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
    async fn do_select_clears_exclusion_set_and_retry_count() {
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        worker.exclusion_set.insert(BeadId::from("old-bead"));
        worker.retry_count = 3;

        worker.do_select().await.unwrap();

        assert!(worker.exclusion_set.is_empty());
        assert_eq!(worker.retry_count, 0);
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
