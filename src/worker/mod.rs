//! Worker loop: the core NEEDLE state machine.
//!
//! The Worker executes the strand waterfall, claims beads, dispatches the
//! agent, handles outcomes, and emits telemetry for every transition.
//!
//! Depends on: `strand`, `claim`, `prompt`, `dispatch`, `outcome`,
//!             `bead_store`, `telemetry`, `health`, `config`, `types`.

use anyhow::Result;
use std::sync::Arc;

use crate::bead_store::BeadStore;
use crate::claim::Claimer;
use crate::config::Config;
use crate::dispatch::Dispatcher;
use crate::health::HealthMonitor;
use crate::outcome::OutcomeHandler;
use crate::prompt::PromptBuilder;
use crate::strand::StrandRunner;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{ClaimResult, WorkerState};

/// The NEEDLE worker — owns and drives the full state machine.
pub struct Worker {
    config: Config,
    worker_name: String,
    store: Arc<dyn BeadStore>,
    telemetry: Telemetry,
    strands: StrandRunner,
    claimer: Claimer,
    prompt_builder: PromptBuilder,
    dispatcher: Dispatcher,
    outcome_handler: OutcomeHandler,
    health: HealthMonitor,
}

impl Worker {
    /// Construct a worker from config, a worker name, and a bead store implementation.
    pub fn new(config: Config, worker_name: String, store: Arc<dyn BeadStore>) -> Self {
        let telemetry = Telemetry::new(worker_name.clone());
        let strands = StrandRunner::from_config(&config);
        let claimer = Claimer::new(
            store.clone(),
            std::path::PathBuf::from("/tmp"),
            config.worker.max_claim_retries,
            100,
            Telemetry::new(worker_name.clone()),
        );
        let prompt_builder = PromptBuilder::new(&config.prompt);
        let dispatcher = Dispatcher::new(&config, Telemetry::new(worker_name.clone()))
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to load adapters, using built-in defaults");
                let builtins = crate::dispatch::builtin_adapters()
                    .into_iter()
                    .map(|a| (a.name.clone(), a))
                    .collect();
                Dispatcher::with_adapters(
                    builtins,
                    Telemetry::new(worker_name.clone()),
                    config.agent.timeout,
                )
            });
        let outcome_handler =
            OutcomeHandler::new(config.clone(), Telemetry::new(worker_name.clone()));
        let health = HealthMonitor::new(
            config.clone(),
            worker_name.clone(),
            Telemetry::new(worker_name.clone()),
        );

        Worker {
            config,
            worker_name,
            store,
            telemetry,
            strands,
            claimer,
            prompt_builder,
            dispatcher,
            outcome_handler,
            health,
        }
    }

    /// Run the worker loop until exhausted, stopped, or errored.
    pub async fn run(&mut self) -> Result<WorkerState> {
        let _ = (&self.config, &self.worker_name);
        self.transition(WorkerState::Booting, WorkerState::Selecting)
            .await?;

        loop {
            let candidate_id = self.strands.select(self.store.as_ref()).await?;

            let bead_id = match candidate_id {
                Some(id) => id,
                None => {
                    tracing::info!("no ready beads — worker exhausted");
                    return Ok(WorkerState::Exhausted);
                }
            };

            self.transition(WorkerState::Selecting, WorkerState::Claiming)
                .await?;
            let claim = self.claimer.claim_one(&bead_id, &self.worker_name).await?;

            let bead = match claim {
                ClaimResult::Claimed(bead) => bead,
                ClaimResult::RaceLost { claimed_by } => {
                    tracing::debug!(bead_id = %bead_id, %claimed_by, "claim race lost");
                    self.transition(WorkerState::Claiming, WorkerState::Selecting)
                        .await?;
                    continue;
                }
                ClaimResult::NotClaimable { reason } => {
                    tracing::debug!(bead_id = %bead_id, %reason, "bead not claimable");
                    self.transition(WorkerState::Claiming, WorkerState::Selecting)
                        .await?;
                    continue;
                }
            };

            self.transition(WorkerState::Claiming, WorkerState::Building)
                .await?;
            let prompt = self.prompt_builder.build_pluck(
                &bead,
                &self.config.workspace.default,
                &self.worker_name,
            )?;

            self.transition(WorkerState::Building, WorkerState::Dispatching)
                .await?;
            self.health.update_heartbeat(Some(&bead.id)).await?;

            self.transition(WorkerState::Dispatching, WorkerState::Executing)
                .await?;
            let adapter_name = &self.config.agent.default;
            let adapter = self
                .dispatcher
                .adapter(adapter_name)
                .cloned()
                .unwrap_or_else(|| {
                    // Fall back to first available adapter.
                    self.dispatcher
                        .adapter("claude-sonnet")
                        .cloned()
                        .unwrap_or_else(|| {
                            crate::dispatch::builtin_adapters()
                                .into_iter()
                                .next()
                                .unwrap()
                        })
                });
            let exec_result = self
                .dispatcher
                .dispatch(&bead.id, &prompt, &adapter, &self.config.workspace.default)
                .await?;
            let output = crate::types::AgentOutcome {
                exit_code: exec_result.exit_code,
                stdout: exec_result.stdout,
                stderr: exec_result.stderr,
            };

            self.transition(WorkerState::Executing, WorkerState::Handling)
                .await?;
            self.outcome_handler
                .handle(self.store.as_ref(), &bead.id, output)
                .await?;

            self.transition(WorkerState::Handling, WorkerState::Logging)
                .await?;
            // Telemetry already emitted by outcome handler.

            self.transition(WorkerState::Logging, WorkerState::Selecting)
                .await?;
            self.health.update_heartbeat(None).await?;
        }
    }

    async fn transition(&self, from: WorkerState, to: WorkerState) -> Result<()> {
        tracing::debug!(from = %from, to = %to, "state transition");
        self.telemetry
            .emit(EventKind::StateTransition { from, to })?;
        Ok(())
    }
}
