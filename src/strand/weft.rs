//! Weft strand orchestration for LOOM turns. Adapter execution is injected so
//! the protocol client stays independent of any harness implementation.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use serde_json::Value;

use crate::bead_store::BeadStore;
use crate::config::LoomConfig;
use crate::types::{BeadId, StrandError, StrandResult};

use super::weft_client::{ClaimOutcome, LoomBrief, LoomClient, LoomError};

#[derive(Debug)]
pub struct WeftExecution {
    pub summary: String,
    pub telemetry: Value,
    pub completion: Value,
}

#[async_trait]
pub trait WeftTurnExecutor: Send + Sync {
    async fn execute(&self, brief: LoomBrief) -> Result<WeftExecution, LoomError>;
}

pub struct WeftStrand {
    config: LoomConfig,
    client: Arc<LoomClient>,
    executor: Arc<dyn WeftTurnExecutor>,
}

impl WeftStrand {
    pub fn new(
        config: LoomConfig,
        worker_name: impl Into<String>,
        executor: Arc<dyn WeftTurnExecutor>,
    ) -> Self {
        let client = Arc::new(LoomClient::new(config.clone(), worker_name));
        Self {
            config,
            client,
            executor,
        }
    }
}

#[async_trait]
impl super::Strand for WeftStrand {
    fn name(&self) -> &str {
        "weft"
    }

    fn is_generator(&self) -> bool {
        false
    }

    async fn evaluate(
        &self,
        _store: &dyn BeadStore,
        _exclusions: &HashSet<BeadId>,
    ) -> StrandResult {
        if !self.config.enabled || self.config.serve_strands.is_empty() {
            return StrandResult::NoWork;
        }

        let strands = self
            .config
            .serve_strands
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let client = Arc::clone(&self.client);
        let turns = match tokio::task::spawn_blocking(move || client.list_turns(&strands)).await {
            Ok(Ok(turns)) => turns,
            Ok(Err(error)) => return strand_error(error),
            Err(error) => return StrandResult::Error(StrandError::StoreError(anyhow!(error))),
        };

        for turn in turns {
            let client = Arc::clone(&self.client);
            let turn_id = turn.id.clone();
            let claim = match tokio::task::spawn_blocking(move || client.claim(&turn_id)).await {
                Ok(Ok(claim)) => claim,
                Ok(Err(error)) => return strand_error(error),
                Err(error) => return StrandResult::Error(StrandError::StoreError(anyhow!(error))),
            };
            match claim {
                ClaimOutcome::Contended => continue,
                ClaimOutcome::Claimed(_) | ClaimOutcome::AlreadyClaimed => {}
            }

            let client = Arc::clone(&self.client);
            let turn_id = turn.id.clone();
            let brief = match tokio::task::spawn_blocking(move || client.brief(&turn_id)).await {
                Ok(Ok(brief)) => brief,
                // A lost lease means another worker owns the outcome now. Stop
                // immediately and discard all local work for this turn.
                Ok(Err(LoomError::LeaseLost)) => return StrandResult::NoWork,
                Ok(Err(error)) => return strand_error(error),
                Err(error) => return StrandResult::Error(StrandError::StoreError(anyhow!(error))),
            };

            let execution_future = self.executor.execute(brief);
            tokio::pin!(execution_future);
            let heartbeat_delay = Duration::from_secs(self.config.heartbeat_secs.max(1));
            let mut heartbeat = tokio::time::interval_at(
                tokio::time::Instant::now() + heartbeat_delay,
                heartbeat_delay,
            );
            let execution = loop {
                tokio::select! {
                    result = &mut execution_future => break match result {
                        Ok(execution) => execution,
                        Err(error) => {
                            let client = Arc::clone(&self.client);
                            let turn_id = turn.id.clone();
                            let detail = error.to_string();
                            let _ = tokio::task::spawn_blocking(move || {
                                client.fail(&turn_id, "execution_failed", &detail, true)
                            }).await;
                            return strand_error(error);
                        }
                    },
                    _ = heartbeat.tick() => {
                        let client = Arc::clone(&self.client);
                        let turn_id = turn.id.clone();
                        match tokio::task::spawn_blocking(move || client.heartbeat(&turn_id)).await {
                            Ok(Ok(_)) => {}
                            Ok(Err(LoomError::LeaseLost)) => return StrandResult::NoWork,
                            Ok(Err(error)) => return strand_error(error),
                            Err(error) => return StrandResult::Error(StrandError::StoreError(anyhow!(error))),
                        }
                    }
                }
            };

            let client = Arc::clone(&self.client);
            let turn_id = turn.id.clone();
            let completion = execution.completion.clone();
            match tokio::task::spawn_blocking(move || client.complete(&turn_id, &completion)).await
            {
                Ok(Ok(_)) => {
                    return StrandResult::WorkPerformed {
                        summary: execution.summary,
                        telemetry: execution.telemetry,
                    };
                }
                Ok(Err(LoomError::LeaseLost)) => return StrandResult::NoWork,
                Ok(Err(error)) => return strand_error(error),
                Err(error) => return StrandResult::Error(StrandError::StoreError(anyhow!(error))),
            }
        }

        StrandResult::NoWork
    }
}

fn strand_error(error: LoomError) -> StrandResult {
    StrandResult::Error(StrandError::StoreError(anyhow!(error)))
}
