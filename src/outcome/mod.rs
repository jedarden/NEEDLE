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
use crate::types::{AgentOutcome, BeadId};

/// Named outcome variants for an agent execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomeKind {
    /// Agent completed work successfully (exit 0).
    Success,
    /// Agent reported a transient error; bead should be retried (exit 1).
    TransientFailure,
    /// Agent reported a permanent error; bead should be abandoned (exit 2).
    PermanentFailure,
    /// Agent was interrupted (SIGINT/SIGTERM, exit 130/143).
    Interrupted,
    /// Unknown exit code — logged and escalated.
    Unknown(i32),
}

impl OutcomeKind {
    pub fn from_exit_code(code: i32) -> Self {
        match code {
            0 => OutcomeKind::Success,
            1 => OutcomeKind::TransientFailure,
            2 => OutcomeKind::PermanentFailure,
            130 | 143 => OutcomeKind::Interrupted,
            other => OutcomeKind::Unknown(other),
        }
    }
}

/// Routes agent outcomes to their explicit handlers.
pub struct OutcomeHandler {
    config: Config,
    telemetry: Telemetry,
}

impl OutcomeHandler {
    pub fn new(config: Config, telemetry: Telemetry) -> Self {
        OutcomeHandler { config, telemetry }
    }

    /// Handle an agent outcome for the given bead.
    pub async fn handle(
        &self,
        store: &dyn BeadStore,
        bead_id: &BeadId,
        outcome: AgentOutcome,
    ) -> Result<()> {
        let kind = OutcomeKind::from_exit_code(outcome.exit_code);
        tracing::info!(
            bead_id = %bead_id,
            exit_code = outcome.exit_code,
            outcome = ?kind,
            "handling agent outcome"
        );

        match kind {
            OutcomeKind::Success => {
                // Agent is responsible for closing the bead via `br close`.
                // We just log and move on.
                tracing::info!(bead_id = %bead_id, "agent completed successfully");
                Ok(())
            }
            OutcomeKind::TransientFailure => {
                // Reset to open so another worker can retry.
                store
                    .set_status(bead_id, crate::types::BeadStatus::Open)
                    .await?;
                tracing::warn!(bead_id = %bead_id, "transient failure — bead reset to open");
                Ok(())
            }
            OutcomeKind::PermanentFailure => {
                // TODO(needle-nva): mark bead as blocked / add failure note
                tracing::error!(bead_id = %bead_id, "permanent failure — bead needs manual intervention");
                Ok(())
            }
            OutcomeKind::Interrupted => {
                // Reset to open so work isn't lost.
                store
                    .set_status(bead_id, crate::types::BeadStatus::Open)
                    .await?;
                tracing::info!(bead_id = %bead_id, "agent interrupted — bead reset to open");
                Ok(())
            }
            OutcomeKind::Unknown(code) => {
                let _ = (store, &self.config, &self.telemetry);
                tracing::warn!(bead_id = %bead_id, code, "unknown exit code — treating as transient failure");
                store
                    .set_status(bead_id, crate::types::BeadStatus::Open)
                    .await?;
                Ok(())
            }
        }
    }
}
