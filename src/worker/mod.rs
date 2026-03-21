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
use crate::claim::{ClaimResult, Claimer};
use crate::config::Config;
use crate::dispatch::Dispatcher;
use crate::health::HealthMonitor;
use crate::outcome::OutcomeHandler;
use crate::prompt::PromptBuilder;
use crate::strand::StrandRunner;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::WorkerState;

/// The NEEDLE worker — owns and drives the full state machine.
pub struct Worker {
    config: Config,
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
    /// Construct a worker from config and a bead store implementation.
    pub fn new(config: Config, store: Arc<dyn BeadStore>) -> Self {
        let telemetry = Telemetry::new(config.worker_name.clone());
        let strands = StrandRunner::from_config(&config);
        let claimer = Claimer::new(config.clone(), Telemetry::new(config.worker_name.clone()));
        let prompt_builder = PromptBuilder::new(config.clone());
        let dispatcher =
            Dispatcher::new(config.clone(), Telemetry::new(config.worker_name.clone()));
        let outcome_handler =
            OutcomeHandler::new(config.clone(), Telemetry::new(config.worker_name.clone()));
        let health = HealthMonitor::new(config.clone(), Telemetry::new(config.worker_name.clone()));

        Worker {
            config,
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
        self.transition(WorkerState::Booting, WorkerState::Selecting)
            .await?;

        loop {
            let candidate = self.strands.select(self.store.as_ref()).await?;

            let bead_id = match candidate {
                Some(id) => id,
                None => {
                    tracing::info!("no ready beads — worker exhausted");
                    return Ok(WorkerState::Exhausted);
                }
            };

            self.transition(WorkerState::Selecting, WorkerState::Claiming)
                .await?;
            let claim = self.claimer.claim(self.store.as_ref(), &bead_id).await?;

            match claim {
                ClaimResult::Claimed => {}
                ClaimResult::RaceLost | ClaimResult::Unavailable => {
                    self.transition(WorkerState::Claiming, WorkerState::Selecting)
                        .await?;
                    continue;
                }
            }

            self.transition(WorkerState::Claiming, WorkerState::Building)
                .await?;
            let bead = self.store.get(&bead_id).await?;
            let prompt = self.prompt_builder.build(&bead)?;

            self.transition(WorkerState::Building, WorkerState::Dispatching)
                .await?;
            self.health.update_heartbeat(Some(&bead_id)).await?;

            self.transition(WorkerState::Dispatching, WorkerState::Executing)
                .await?;
            let outcome = self.dispatcher.dispatch(&bead_id, &prompt).await?;

            self.transition(WorkerState::Executing, WorkerState::Handling)
                .await?;
            self.outcome_handler
                .handle(self.store.as_ref(), &bead_id, outcome)
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
