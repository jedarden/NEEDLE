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

/// One result from the adapter-output parse ladder (LOOM EC-05).
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedWeftOutput {
    Completion(Value),
    Fallback { reply_markdown: String },
    Empty { first_output_timeout: bool },
}

/// A terminal failure produced by the empty-output rung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeftOutputFailure {
    pub error_code: &'static str,
    pub error_detail: &'static str,
    pub retryable: bool,
}

impl ParsedWeftOutput {
    /// Build only the fields the completion contract accepts; unknown model
    /// keys are discarded before they can reach the LOOM API.
    pub fn completion_body(&self) -> Option<Value> {
        let mut body = serde_json::Map::new();
        body.insert("contract_version".into(), Value::from(1));
        match self {
            Self::Completion(completion) => {
                body.insert("parse_fallback".into(), Value::from(false));
                for field in [
                    "reply_markdown",
                    "post_summary",
                    "proposed_bead",
                    "context_description_edits",
                ] {
                    if let Some(value) = completion.get(field).filter(|value| !value.is_null()) {
                        body.insert(field.to_owned(), value.clone());
                    }
                }
            }
            Self::Fallback { reply_markdown } => {
                body.insert("parse_fallback".into(), Value::from(true));
                body.insert(
                    "reply_markdown".into(),
                    Value::String(reply_markdown.clone()),
                );
            }
            Self::Empty { .. } => return None,
        }
        Some(Value::Object(body))
    }

    pub fn failure(&self) -> Option<WeftOutputFailure> {
        match self {
            Self::Empty {
                first_output_timeout,
            } => Some(WeftOutputFailure {
                error_code: "empty_reply",
                error_detail: if *first_output_timeout {
                    "the adapter produced no output before the timeout"
                } else {
                    "the adapter produced no output"
                },
                retryable: *first_output_timeout,
            }),
            Self::Completion(_) | Self::Fallback { .. } => None,
        }
    }
}

/// Parse an adapter transcript without losing useful unstructured output.
pub fn parse_adapter_output(text: &str, first_output_timeout: bool) -> ParsedWeftOutput {
    for body in fenced_json_bodies(text) {
        let Ok(value) = serde_json::from_str::<Value>(&body) else {
            continue;
        };
        if value
            .get("reply_markdown")
            .and_then(Value::as_str)
            .is_some()
        {
            return ParsedWeftOutput::Completion(value);
        }
    }

    if !text.trim().is_empty() {
        return ParsedWeftOutput::Fallback {
            reply_markdown: text.to_owned(),
        };
    }

    ParsedWeftOutput::Empty {
        first_output_timeout,
    }
}

fn fenced_json_bodies(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if !line.trim_start().as_bytes().starts_with(&[96, 96, 96]) {
            if let Some(body) = &mut current {
                body.push_str(line);
                body.push('\n');
            }
            continue;
        }

        match &mut current {
            None => current = Some(String::new()),
            Some(body) => {
                blocks.push(std::mem::take(body));
                current = None;
            }
        }
    }
    if let Some(body) = current {
        blocks.push(body);
    }
    blocks
}

#[async_trait]
pub trait WeftTurnExecutor: Send + Sync {
    async fn execute(
        &self,
        brief: LoomBrief,
        turn_id: String,
        client: Arc<LoomClient>,
    ) -> Result<WeftExecution, LoomError>;
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

            let execution_future =
                self.executor
                    .execute(brief, turn.id.clone(), Arc::clone(&self.client));
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
                        Err(LoomError::TurnFailure { code, detail, retryable }) => {
                            let client = Arc::clone(&self.client);
                            let turn_id = turn.id.clone();
                            return match tokio::task::spawn_blocking(move || {
                                client.fail(&turn_id, code, detail, retryable)
                            }).await {
                                Ok(Ok(_)) | Ok(Err(LoomError::LeaseLost)) => StrandResult::NoWork,
                                Ok(Err(error)) => strand_error(error),
                                Err(error) => StrandResult::Error(StrandError::StoreError(anyhow!(error))),
                            };
                        }
                        Err(LoomError::LeaseLost) => return StrandResult::NoWork,
                        Err(error) => return strand_error(error),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ladder_fixture(name: &str) -> Value {
        serde_json::from_str(match name {
            "fenced_json" => {
                include_str!("../../tests/fixtures/weft/parse_ladder/fenced_json.json")
            }
            "bare_text" => include_str!("../../tests/fixtures/weft/parse_ladder/bare_text.json"),
            "empty" => include_str!("../../tests/fixtures/weft/parse_ladder/empty.json"),
            "empty_first_output_timeout" => include_str!(
                "../../tests/fixtures/weft/parse_ladder/empty_first_output_timeout.json"
            ),
            _ => unreachable!("known fixture"),
        })
        .unwrap()
    }

    #[test]
    fn parse_ladder_fenced_json_is_normalized_to_completion_contract() {
        let fixture = parse_ladder_fixture("fenced_json");
        let input = &fixture["input"];
        let parsed = parse_adapter_output(input["adapter_text"].as_str().unwrap(), false);
        assert_eq!(parsed.completion_body().unwrap(), fixture["expected"]);
    }

    #[test]
    fn parse_ladder_bare_text_is_verbatim_and_omits_post_summary() {
        let fixture = parse_ladder_fixture("bare_text");
        let input = &fixture["input"];
        let parsed = parse_adapter_output(input["adapter_text"].as_str().unwrap(), false);
        let body = parsed.completion_body().unwrap();
        assert_eq!(body, fixture["expected"]);
        for field in fixture["expected_absent"].as_array().unwrap() {
            assert!(body.get(field.as_str().unwrap()).is_none());
        }
    }

    #[test]
    fn parse_ladder_empty_reply_retries_only_after_first_output_timeout() {
        for name in ["empty", "empty_first_output_timeout"] {
            let fixture = parse_ladder_fixture(name);
            let input = &fixture["input"];
            let parsed = parse_adapter_output(
                input["adapter_text"].as_str().unwrap(),
                input["first_output_timeout"].as_bool().unwrap(),
            );
            let failure = parsed.failure().unwrap();
            assert_eq!(failure.error_code, fixture["expected"]["error_code"]);
            assert_eq!(failure.error_detail, fixture["expected"]["error_detail"]);
            assert_eq!(failure.retryable, fixture["expected"]["retryable"]);
            assert!(parsed.completion_body().is_none());
        }
    }
}
