//! Outcome routing: map agent exit codes to explicit handlers.
//!
//! Every possible exit code has a named handler. The type system enforces
//! exhaustiveness — if an outcome can happen, it must have a handler.
//!
//! Depends on: `types`, `config`, `bead_store`, `telemetry`.

use anyhow::Result;

use crate::bead_store::BeadStore;
use crate::config::Config;
use crate::telemetry::Telemetry;
use crate::types::{BeadId, BeadStatus, Outcome, ProcessOutput};

/// Routes agent outcomes to their explicit handlers.
pub struct OutcomeHandler {
    config: Config,
    telemetry: Telemetry,
}

impl OutcomeHandler {
    pub fn new(config: Config, telemetry: Telemetry) -> Self {
        OutcomeHandler { config, telemetry }
    }

    /// Handle a process output for the given bead.
    ///
    /// Uses `Outcome::classify()` to route to the correct handler.
    /// Every `Outcome` variant has an explicit arm — no wildcards.
    pub async fn handle(
        &self,
        store: &dyn BeadStore,
        bead_id: &BeadId,
        output: ProcessOutput,
    ) -> Result<()> {
        let outcome = output.classify();
        tracing::info!(
            bead_id = %bead_id,
            exit_code = output.exit_code,
            outcome = %outcome,
            "handling agent outcome"
        );

        match outcome {
            Outcome::Success => {
                // Agent is responsible for closing the bead via `br close`.
                tracing::info!(bead_id = %bead_id, "agent completed successfully");
                Ok(())
            }
            Outcome::Failure => {
                // Reset to open so another worker can retry.
                store.set_status(bead_id, BeadStatus::Open).await?;
                tracing::warn!(bead_id = %bead_id, "agent failure — bead reset to open");
                Ok(())
            }
            Outcome::Timeout => {
                // Treat timeout as a transient failure; reset to open.
                store.set_status(bead_id, BeadStatus::Open).await?;
                tracing::warn!(bead_id = %bead_id, "agent timed out — bead reset to open");
                Ok(())
            }
            Outcome::AgentNotFound => {
                // Configuration error — worker cannot proceed.
                let _ = &self.config;
                tracing::error!(
                    bead_id = %bead_id,
                    agent = %self.config.agent_binary,
                    "agent binary not found — worker should stop"
                );
                // Leave bead in_progress so it doesn't get re-picked until
                // a human fixes the configuration.
                Ok(())
            }
            Outcome::Interrupted => {
                // Reset to open so work isn't lost.
                store.set_status(bead_id, BeadStatus::Open).await?;
                tracing::info!(bead_id = %bead_id, "agent interrupted — bead reset to open");
                Ok(())
            }
            Outcome::Crash { code } => {
                let _ = &self.telemetry;
                tracing::warn!(
                    bead_id = %bead_id,
                    code,
                    "agent crashed — bead reset to open for retry"
                );
                store.set_status(bead_id, BeadStatus::Open).await?;
                Ok(())
            }
        }
    }
}
