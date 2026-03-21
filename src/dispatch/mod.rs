//! Agent dispatch: execute the configured agent CLI with the prompt.
//!
//! Dispatcher spawns the agent binary as a subprocess, captures its output,
//! and returns an AgentOutcome. Telemetry is a separate channel — never
//! interleaved with agent stdout/stderr.
//!
//! Depends on: `types`, `config`, `telemetry`.

use anyhow::{Context, Result};

use crate::config::Config;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{AgentOutcome, BeadId};

/// Dispatches the agent CLI for a given bead.
pub struct Dispatcher {
    config: Config,
    telemetry: Telemetry,
}

impl Dispatcher {
    pub fn new(config: Config, telemetry: Telemetry) -> Self {
        Dispatcher { config, telemetry }
    }

    /// Spawn the agent, wait for it to exit, and return the outcome.
    pub async fn dispatch(&self, bead_id: &BeadId, prompt: &str) -> Result<AgentOutcome> {
        self.telemetry.emit(EventKind::DispatchStarted {
            bead_id: bead_id.clone(),
            agent: self.config.agent_binary.clone(),
        })?;

        let output = tokio::process::Command::new(&self.config.agent_binary)
            .args(&self.config.agent_args)
            .arg(prompt)
            .output()
            .await
            .with_context(|| format!("failed to spawn agent: {}", self.config.agent_binary))?;

        let exit_code = output.status.code().unwrap_or(-1);
        let outcome = AgentOutcome {
            exit_code,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        };

        self.telemetry.emit(EventKind::DispatchCompleted {
            bead_id: bead_id.clone(),
            exit_code,
        })?;

        Ok(outcome)
    }
}
