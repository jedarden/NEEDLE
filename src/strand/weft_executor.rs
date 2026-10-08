//! Production adapter executor for LOOM Weft turns.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::config::{Config, LoomServeStrand};
use crate::dispatch::{AgentAdapter, Dispatcher, ExecutionResult};
use crate::prompt::BuiltPrompt;
use crate::telemetry::Telemetry;
use crate::types::BeadId;

use super::weft::{parse_adapter_output, WeftExecution, WeftTurnExecutor};
use super::weft_client::{LoomBrief, LoomClient, LoomError};
use super::weft_envelope::{
    harness_for_adapter, shape_invocation, shell_quote, translate, validate_loom_adapter,
    EnvelopeRequest, WeftHarness,
};

pub struct MappedWeftExecutor {
    config: Config,
    telemetry: Telemetry,
}

impl MappedWeftExecutor {
    pub fn new(config: Config, telemetry: Telemetry) -> Self {
        Self { config, telemetry }
    }

    async fn run(
        &self,
        brief: LoomBrief,
        turn_id: String,
        client: Arc<LoomClient>,
    ) -> Result<WeftExecution, LoomError> {
        let strand = brief
            .turn
            .get("strand")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mapping = self.config.loom.serve_strands.get(strand).ok_or_else(|| {
            failure(
                "adapter_unavailable",
                "no adapter mapping exists for this strand",
                false,
            )
        })?;
        let timeout_secs = brief
            .limits
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                failure(
                    "invalid_limits",
                    "timeout_secs must be a positive integer",
                    false,
                )
            })?;
        let max_turns = brief
            .limits
            .get("max_turns")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                failure(
                    "invalid_limits",
                    "max_turns must be a non-negative integer",
                    false,
                )
            })?;

        let workspace = brief
            .workspace_root
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.config.loom.workspace_root.clone());
        if !workspace.is_absolute() || !workspace.is_dir() {
            return Err(failure(
                "workspace_unavailable",
                "configured workspace root is unavailable",
                false,
            ));
        }

        let scratch = ScratchTurn::create(&self.config.loom.scratch_dir, &turn_id)?;
        let attachment_dir = scratch.path().to_path_buf();
        download_attachments(&brief, &client, &attachment_dir).await?;

        let dispatcher = Dispatcher::new(&self.config, self.telemetry.clone()).map_err(|_| {
            failure(
                "adapter_unavailable",
                "could not load configured adapters",
                false,
            )
        })?;
        let mut adapter = dispatcher
            .adapter(&mapping.adapter)
            .cloned()
            .ok_or_else(|| {
                failure(
                    "adapter_unavailable",
                    "mapped adapter definition was not found",
                    false,
                )
            })?;
        configure_adapter(
            &mut adapter,
            mapping,
            &brief,
            timeout_secs,
            max_turns,
            &scratch,
        )?;
        let harness = harness_for_adapter(&adapter).map_err(|_| {
            failure(
                "adapter_unavailable",
                "mapped adapter uses an unsupported harness",
                false,
            )
        })?;
        validate_loom_adapter(&adapter).map_err(|_| {
            failure(
                "billing_policy",
                "Claude turns must use claude-print for subscription billing",
                false,
            )
        })?;

        let envelope = parse_envelope(&brief.envelope)?;
        let translated = translate(harness, &envelope).map_err(|_| {
            failure(
                "capability_unsupported",
                "mapped harness cannot express the requested envelope",
                false,
            )
        })?;
        let settings_path = scratch.path().join("settings.json");
        if let Some(settings) = &translated.settings {
            write_private(
                &settings_path,
                &serde_json::to_vec(settings).map_err(|_| {
                    failure(
                        "adapter_configuration",
                        "could not serialize translated settings",
                        false,
                    )
                })?,
            )?;
        }
        adapter.invoke_template = shape_invocation(
            &adapter.invoke_template,
            &translated,
            max_turns,
            &settings_path,
        );
        adapter.invoke_template = replace_template_placeholder(
            &adapter.invoke_template,
            "{workspace}",
            &workspace.display().to_string(),
        );
        adapter.invoke_template = replace_template_placeholder(
            &adapter.invoke_template,
            "{model}",
            adapter.model.as_deref().unwrap_or_default(),
        );
        adapter.invoke_template =
            replace_template_placeholder(&adapter.invoke_template, "{bead_id}", &turn_id);
        adapter.invoke_template = adapter
            .invoke_template
            .replace("{prompt_file}", "'{prompt_file}'");
        adapter.environment.insert(
            "NEEDLE_WEFT_ATTACHMENT_DIR".into(),
            attachment_dir.display().to_string(),
        );
        adapter
            .environment
            .insert("NEEDLE_WEFT_TURN_ID".into(), turn_id.clone());

        let prompt = BuiltPrompt {
            content: brief.prompt_markdown.clone(),
            hash: format!("weft-{turn_id}"),
            token_estimate: (brief.prompt_markdown.len() / 4) as u64,
            template_name: "weft".into(),
            template_version: "1".into(),
        };
        let dispatch_id = BeadId::from(format!("weft-{turn_id}"));
        let started = Instant::now();
        let result = if harness == WeftHarness::Codex && max_turns > 0 {
            dispatcher
                .dispatch_unclaimed_analysis_with_turn_limit(
                    &dispatch_id,
                    &prompt,
                    &adapter,
                    &workspace,
                    max_turns,
                )
                .await
        } else {
            dispatcher
                .dispatch_unclaimed_analysis(&dispatch_id, &prompt, &adapter, &workspace)
                .await
        }
        .map_err(|_| failure("adapter_failed", "mapped adapter execution failed", false))?;

        let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let (output, turns_used) = extract_output(harness, &result);
        let first_output_timeout = result.timeout_reason.is_some() && result.stdout.is_empty();
        let parsed = parse_adapter_output(&output, first_output_timeout);
        let Some(mut completion) = parsed.completion_body() else {
            let timeout = first_output_timeout;
            return Err(failure(
                "empty_reply",
                if timeout {
                    "the adapter produced no output before the timeout"
                } else {
                    "the adapter produced no output"
                },
                timeout,
            ));
        };
        let adapter_result = json!({
            "name": adapter.name,
            "model": adapter.model,
            "duration_ms": duration_ms,
            "turns_used": turns_used,
        });
        let Some(completion_map) = completion.as_object_mut() else {
            return Err(failure(
                "adapter_output_invalid",
                "normalized completion was not an object",
                false,
            ));
        };
        completion_map.insert("adapter".into(), adapter_result.clone());
        let summary = completion
            .get("reply_markdown")
            .and_then(Value::as_str)
            .unwrap_or("Weft turn completed")
            .lines()
            .next()
            .unwrap_or("Weft turn completed")
            .to_string();
        Ok(WeftExecution {
            summary,
            telemetry: adapter_result,
            completion,
        })
    }
}

#[async_trait]
impl WeftTurnExecutor for MappedWeftExecutor {
    async fn execute(
        &self,
        brief: LoomBrief,
        turn_id: String,
        client: Arc<LoomClient>,
    ) -> Result<WeftExecution, LoomError> {
        self.run(brief, turn_id, client).await
    }
}

fn configure_adapter(
    adapter: &mut AgentAdapter,
    mapping: &LoomServeStrand,
    brief: &LoomBrief,
    timeout_secs: u64,
    max_turns: u64,
    scratch: &ScratchTurn,
) -> Result<(), LoomError> {
    adapter.model = mapping.model.clone().or_else(|| brief.model_hint.clone());
    adapter.timeout_secs = timeout_secs;
    adapter.idle_timeout_secs = 0;
    adapter.hard_timeout_secs = timeout_secs;
    // Ensure model prompts and adapter settings stay inside the private turn directory.
    adapter.environment.insert(
        "NEEDLE_WEFT_SCRATCH_DIR".into(),
        scratch.path().display().to_string(),
    );
    if max_turns == 0 {
        return Err(failure(
            "invalid_limits",
            "model turns require a positive max_turns limit",
            false,
        ));
    }
    Ok(())
}

fn replace_template_placeholder(template: &str, placeholder: &str, value: &str) -> String {
    let quoted = shell_quote(value);
    template
        .replace(&format!("'{placeholder}'"), &quoted)
        .replace(&format!("\\\"{placeholder}\\\""), &quoted)
        .replace(placeholder, &quoted)
}

fn parse_envelope(value: &Value) -> Result<EnvelopeRequest, LoomError> {
    fn strings(value: Option<&Value>) -> Result<Vec<String>, LoomError> {
        let Some(value) = value.filter(|value| !value.is_null()) else {
            return Ok(Vec::new());
        };
        let Some(values) = value.as_array() else {
            return Err(failure(
                "invalid_envelope",
                "envelope fields must be arrays of strings",
                false,
            ));
        };
        values
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    failure(
                        "invalid_envelope",
                        "envelope fields must be arrays of strings",
                        false,
                    )
                })
            })
            .collect()
    }
    Ok(EnvelopeRequest {
        capabilities: strings(value.get("capabilities"))?,
        deny_paths: strings(value.get("deny_paths"))?,
    })
}

async fn download_attachments(
    brief: &LoomBrief,
    client: &Arc<LoomClient>,
    directory: &Path,
) -> Result<(), LoomError> {
    for attachment in &brief.attachments {
        let id = attachment
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("attachment_invalid", "attachment id is missing", false))?
            .to_owned();
        let filename = attachment
            .get("filename")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                failure(
                    "attachment_invalid",
                    "attachment filename is missing",
                    false,
                )
            })?;
        let declared_size = attachment
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                failure(
                    "attachment_invalid",
                    "attachment byte size is missing",
                    false,
                )
            })?;
        if declared_size > 50 * 1024 * 1024 {
            return Err(failure(
                "attachment_too_large",
                "attachment exceeds the worker's 50 MiB safety limit",
                false,
            ));
        }
        let name = Path::new(filename)
            .file_name()
            .and_then(|part| part.to_str())
            .filter(|part| {
                !part.is_empty()
                    && *part != "."
                    && *part != ".."
                    && !part.contains('/')
                    && !part.contains('\\')
            })
            .ok_or_else(|| failure("attachment_invalid", "attachment filename is unsafe", false))?
            .to_owned();
        let client = Arc::clone(client);
        let target = directory.join(name);
        tokio::task::spawn_blocking(move || {
            let bytes = match client.download_attachment(&id, declared_size) {
                Ok(bytes) => bytes,
                Err(LoomError::LeaseLost) => return Err(LoomError::LeaseLost),
                Err(LoomError::AttachmentTooLarge { .. }) => {
                    return Err(failure(
                        "attachment_too_large",
                        "attachment exceeded its declared byte limit",
                        false,
                    ));
                }
                Err(LoomError::Unauthorized) => return Err(LoomError::Unauthorized),
                Err(_) => {
                    return Err(failure(
                        "attachment_download_failed",
                        "attachment download failed after its retry schedule",
                        true,
                    ));
                }
            };
            write_private(&target, &bytes)
        })
        .await
        .map_err(|_| {
            failure(
                "attachment_download_failed",
                "attachment worker task stopped unexpectedly",
                true,
            )
        })??;
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), LoomError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| {
            failure(
                "scratch_write_failed",
                "could not create a private turn file",
                false,
            )
        })?;
    file.write_all(bytes).map_err(|_| {
        failure(
            "scratch_write_failed",
            "could not write a private turn file",
            false,
        )
    })
}

fn extract_output(harness: WeftHarness, result: &ExecutionResult) -> (String, u64) {
    let mut output = String::new();
    let mut turns = 0u64;
    for line in result.stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            if harness == WeftHarness::ClaudePrint && !line.trim().is_empty() {
                output.push_str(line);
                output.push('\n');
            }
            continue;
        };
        match harness {
            WeftHarness::ClaudePrint => {
                if value.get("type").and_then(Value::as_str) == Some("result") {
                    if let Some(text) = value.get("result").and_then(Value::as_str) {
                        output.push_str(text);
                    }
                    turns = turns.saturating_add(1);
                }
            }
            WeftHarness::Codex => match value.get("type").and_then(Value::as_str) {
                Some("turn.completed") => turns = turns.saturating_add(1),
                Some("item.completed") => {
                    let item = value.get("item").unwrap_or(&Value::Null);
                    if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            output.push_str(text);
                        }
                    }
                }
                _ => {}
            },
        }
    }
    (output, turns)
}

fn failure(code: &'static str, detail: &'static str, retryable: bool) -> LoomError {
    LoomError::TurnFailure {
        code,
        detail,
        retryable,
    }
}

struct ScratchTurn {
    path: PathBuf,
}

impl ScratchTurn {
    fn create(root: &Path, turn_id: &str) -> Result<Self, LoomError> {
        if turn_id.is_empty()
            || !turn_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(failure(
                "scratch_path_invalid",
                "turn id is unsafe for scratch storage",
                false,
            ));
        }
        fs::create_dir_all(root)
            .map_err(|_| failure("scratch_unavailable", "scratch root is unavailable", false))?;
        let path = root.join(turn_id);
        fs::DirBuilder::new()
            .recursive(false)
            .mode(0o700)
            .create(&path)
            .map_err(|_| {
                failure(
                    "scratch_unavailable",
                    "turn scratch directory already exists or cannot be created",
                    false,
                )
            })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchTurn {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            tracing::warn!(error = %error, "failed to remove Weft turn scratch directory");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn turn_scratch_rejects_path_escape_and_is_removed_on_drop() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            ScratchTurn::create(root.path(), "../outside"),
            Err(LoomError::TurnFailure {
                code: "scratch_path_invalid",
                ..
            })
        ));
        let path;
        {
            let scratch = ScratchTurn::create(root.path(), "turn-safe_123").unwrap();
            path = scratch.path().to_path_buf();
            write_private(&path.join("attachment.bin"), b"fixture").unwrap();
            assert_eq!(
                fs::metadata(path.join("attachment.bin"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(!path.exists());
    }
}
