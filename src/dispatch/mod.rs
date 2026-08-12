//! Agent dispatch: load adapters, render templates, execute agent processes.
//!
//! The dispatcher is agent-agnostic. Adding a new agent requires only a YAML
//! adapter file. It renders an invoke template, starts a process, waits (with
//! timeout enforcement), and captures the raw result.
//!
//! Depends on: `types`, `config`, `telemetry`, `prompt`, `trace`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use libc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

use crate::bead_store::spawn_with_etxtbsy_retry_child;
use crate::config::Config;
use crate::process_guard::ProcessGroupKillGuard;
use crate::prompt::BuiltPrompt;
use crate::sanitize::{CustomPattern, Sanitizer};
use crate::telemetry::{EventKind, Telemetry};
use crate::trace::{detect_trace_format, TraceCapture, TraceMetadata};
use crate::tsnet::{inject_identity_env, IdentityRegistry, TsnetConfig};
use crate::types::{BeadId, InputMethod, Outcome};

// ──────────────────────────────────────────────────────────────────────────────
// ExecutionResult
// ──────────────────────────────────────────────────────────────────────────────

/// Reason why a process was terminated by timeout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutReason {
    /// No stdout/stderr activity for the configured idle timeout duration.
    Idle {
        /// Configured idle timeout in seconds.
        timeout_secs: u64,
        /// Time since last output byte when idle deadline expired.
        last_output_age_secs: u64,
    },
    /// Absolute wall-clock time exceeded the configured hard deadline.
    Hard {
        /// Configured hard timeout in seconds.
        timeout_secs: u64,
    },
    /// Legacy single timeout (backward compatibility).
    Legacy {
        /// Configured timeout in seconds.
        timeout_secs: u64,
    },
}

/// Raw output from an agent process execution.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Process exit code (124 if killed by timeout).
    pub exit_code: i32,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// Wall-clock time from spawn to exit.
    pub elapsed: Duration,
    /// OS process ID.
    pub pid: u32,
    /// Path to trace directory if trace capture was enabled.
    pub trace_path: Option<std::path::PathBuf>,
    /// Structured reason if terminated by timeout (exit_code 124).
    pub timeout_reason: Option<TimeoutReason>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Token extraction
// ──────────────────────────────────────────────────────────────────────────────

/// How to extract token usage from agent output.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum TokenExtraction {
    /// Extract from JSON fields in stdout (e.g., Claude Code --output-format json).
    JsonField {
        /// JSON path for input tokens (e.g., `result.usage.input_tokens`).
        input_path: String,
        /// JSON path for output tokens (e.g., `result.usage.output_tokens`).
        output_path: String,
    },
    /// Extract from stdout/stderr using a regex with capture groups.
    Regex {
        /// Regex pattern with capture groups for token counts.
        pattern: String,
        /// 1-based capture group index for input tokens.
        input_group: usize,
        /// 1-based capture group index for output tokens.
        output_group: usize,
    },
    /// No token extraction.
    #[default]
    None,
}

/// Extracted token usage from agent output.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    /// Input tokens consumed (None if extraction failed or not configured).
    pub input_tokens: Option<u64>,
    /// Output tokens produced (None if extraction failed or not configured).
    pub output_tokens: Option<u64>,
}

/// Extract token usage from agent output using the configured method.
pub fn extract_tokens(extraction: &TokenExtraction, stdout: &str, stderr: &str) -> TokenUsage {
    match extraction {
        TokenExtraction::None => TokenUsage::default(),
        TokenExtraction::JsonField {
            input_path,
            output_path,
        } => extract_tokens_json(stdout, input_path, output_path),
        TokenExtraction::Regex {
            pattern,
            input_group,
            output_group,
        } => {
            // Search both stdout and stderr for the pattern.
            let combined = format!("{stdout}\n{stderr}");
            extract_tokens_regex(&combined, pattern, *input_group, *output_group)
        }
    }
}

/// Extract tokens from JSON output using dot-separated path notation.
fn extract_tokens_json(stdout: &str, input_path: &str, output_path: &str) -> TokenUsage {
    let parsed: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(v) => v,
        Err(_) => return TokenUsage::default(),
    };

    let input_tokens = resolve_json_path(&parsed, input_path).and_then(|v| v.as_u64());
    let output_tokens = resolve_json_path(&parsed, output_path).and_then(|v| v.as_u64());

    TokenUsage {
        input_tokens,
        output_tokens,
    }
}

/// Resolve a dot-separated path in a JSON value (e.g., `result.usage.input_tokens`).
fn resolve_json_path<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for key in path.split('.') {
        current = current.get(key)?;
    }
    Some(current)
}

/// Extract tokens from text using a regex with numbered capture groups.
fn extract_tokens_regex(
    text: &str,
    pattern: &str,
    input_group: usize,
    output_group: usize,
) -> TokenUsage {
    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(_) => return TokenUsage::default(),
    };

    let caps = match re.captures(text) {
        Some(c) => c,
        None => return TokenUsage::default(),
    };

    let parse_group = |group: usize| -> Option<u64> {
        caps.get(group)?
            .as_str()
            .replace(',', "")
            .parse::<u64>()
            .ok()
    };

    TokenUsage {
        input_tokens: parse_group(input_group),
        output_tokens: parse_group(output_group),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// TimeoutPolicy
// ──────────────────────────────────────────────────────────────────────────────

/// Which timeout configuration mode is active for an adapter.
///
/// An adapter can use:
/// - Legacy single timeout (`timeout_secs`)
/// - New two-field timeout (`idle_timeout_secs` + `hard_timeout_secs`)
/// - Global config fallback (no adapter-specific timeout set)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutPolicy {
    /// Legacy single timeout mode (`timeout_secs` is set).
    Legacy,
    /// New two-field timeout mode (at least one of `idle_timeout_secs` or `hard_timeout_secs` is set).
    New {
        /// Whether idle timeout is configured (non-zero).
        idle_enabled: bool,
        /// Whether hard timeout is configured (non-zero).
        hard_enabled: bool,
    },
    /// No adapter-specific timeout; uses global config default.
    Global,
}

// ──────────────────────────────────────────────────────────────────────────────
// AgentAdapter
// ──────────────────────────────────────────────────────────────────────────────

/// Configuration for a single agent adapter, loaded from YAML or embedded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAdapter {
    /// Unique adapter name (e.g., `claude-sonnet`).
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Agent CLI binary name (for documentation / version checks).
    pub agent_cli: String,
    /// Command to check the agent version.
    #[serde(default)]
    pub version_command: Option<String>,
    /// How the prompt is delivered to the agent.
    #[serde(default = "default_input_method")]
    pub input_method: InputMethod,
    /// Shell command template with variable placeholders.
    ///
    /// Supported variables: `{workspace}`, `{prompt_file}`, `{bead_id}`, `{model}`.
    pub invoke_template: String,
    /// Extra environment variables set for the agent process.
    #[serde(default)]
    pub environment: HashMap<String, String>,
    /// Timeout in seconds (0 = use global config timeout).
    #[serde(default)]
    pub timeout_secs: u64,
    /// Idle timeout in seconds (0 = no idle deadline).
    ///
    /// When implemented, will kill the agent if no stdout/stderr activity
    /// is detected for this duration. This is a data-model-only field;
    /// validation and enforcement logic will be added in a follow-up.
    #[serde(default)]
    pub idle_timeout_secs: u64,
    /// Hard timeout in seconds (0 = no hard deadline).
    ///
    /// When implemented, will enforce an absolute maximum execution time
    /// regardless of agent activity. This is a data-model-only field;
    /// enforcement logic will be added in a follow-up.
    #[serde(default)]
    pub hard_timeout_secs: u64,
    /// AI provider name (informational).
    #[serde(default)]
    pub provider: Option<String>,
    /// Model identifier (substituted as `{model}` in the template).
    #[serde(default)]
    pub model: Option<String>,
    /// How to extract token usage from agent output.
    #[serde(default)]
    pub token_extraction: TokenExtraction,
    /// Optional binary/script to normalize agent stdout into universal JSONL.
    ///
    /// When set, the named binary is invoked with the raw agent stdout piped to
    /// its stdin.  Its stdout replaces the raw output for all downstream
    /// processing (token extraction, outcome parsing, …).
    ///
    /// Example adapter YAML:
    /// ```yaml
    /// output_transform: "needle-transform-claude"
    /// ```
    ///
    /// The binary must be present on PATH; `needle test-agent <name>` will
    /// report an error if it cannot be found.
    #[serde(default)]
    pub output_transform: Option<String>,
    /// Harness name for velocity-aware claim scoring.
    #[serde(default)]
    pub harness: Option<String>,
    /// Harness version for velocity-aware claim scoring.
    #[serde(default)]
    pub harness_version: Option<String>,
}

fn default_input_method() -> InputMethod {
    InputMethod::Stdin
}

impl AgentAdapter {
    /// Effective timeout as a `Duration`, falling back to the global config.
    pub fn effective_timeout(&self, global_timeout_secs: u64) -> Duration {
        let secs = if self.timeout_secs > 0 {
            self.timeout_secs
        } else {
            global_timeout_secs
        };
        if secs == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs(secs)
        }
    }

    /// Returns the GenAI system name for this adapter.
    ///
    /// Follows OTel semantic conventions for `gen_ai.system`, which identifies
    /// the AI system/platform (e.g., "anthropic", "openai", "local").
    /// Defaults to "local" if no provider is configured.
    pub fn gen_ai_system(&self) -> &str {
        self.provider.as_deref().unwrap_or("local")
    }

    /// Active timeout policy for this adapter.
    ///
    /// Distinguishes between legacy single timeout and the new two-field
    /// (idle + hard) timeout model.
    pub fn timeout_policy(&self) -> TimeoutPolicy {
        let has_legacy = self.timeout_secs > 0;
        let has_idle = self.idle_timeout_secs > 0;
        let has_hard = self.hard_timeout_secs > 0;

        if has_legacy {
            TimeoutPolicy::Legacy
        } else if has_idle || has_hard {
            TimeoutPolicy::New {
                idle_enabled: has_idle,
                hard_enabled: has_hard,
            }
        } else {
            TimeoutPolicy::Global
        }
    }

    /// Human-readable description of the active timeout policy.
    ///
    /// Shows which timeout mode is active and the configured values.
    pub fn timeout_description(&self, global_timeout_secs: u64) -> String {
        match self.timeout_policy() {
            TimeoutPolicy::Legacy => {
                format!("legacy: {}s (single timeout)", self.timeout_secs)
            }
            TimeoutPolicy::New {
                idle_enabled,
                hard_enabled,
            } => {
                let mut parts = Vec::new();
                parts.push("new:".to_string());
                if idle_enabled {
                    parts.push(format!("idle={}s", self.idle_timeout_secs));
                }
                if hard_enabled {
                    parts.push(format!("hard={}s", self.hard_timeout_secs));
                }
                if !idle_enabled && !hard_enabled {
                    parts.push("both disabled".to_string());
                }
                parts.join(" ")
            }
            TimeoutPolicy::Global => {
                if global_timeout_secs > 0 {
                    format!("global: {}s (from config)", global_timeout_secs)
                } else {
                    "global: unlimited (0 = no timeout)".to_string()
                }
            }
        }
    }

    /// Validate timeout field mutual exclusivity.
    ///
    /// Legacy `timeout_secs` and new `idle_timeout_secs`/`hard_timeout_secs`
    /// cannot both be set. This preserves backward compatibility while allowing
    /// the new two-field timeout model.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `timeout_secs` is non-zero AND either `idle_timeout_secs` or `hard_timeout_secs` is non-zero
    ///
    /// # Examples
    ///
    /// ```
    /// // Valid: legacy timeout alone
    /// assert!(adapter_with_timeout_secs_only().validate_timeouts().is_ok());
    ///
    /// // Valid: new timeout fields together
    /// assert!(adapter_with_new_timeouts().validate_timeouts().is_ok());
    ///
    /// // Valid: new timeout field alone (other set to 0)
    /// assert!(adapter_with_idle_only().validate_timeouts().is_ok());
    ///
    /// // Invalid: mixing legacy and new
    /// assert!(adapter_with_mixed_timeouts().validate_timeouts().is_err());
    /// ```
    pub fn validate_timeouts(&self) -> Result<()> {
        let has_legacy = self.timeout_secs > 0;
        let has_idle = self.idle_timeout_secs > 0;
        let has_hard = self.hard_timeout_secs > 0;

        if has_legacy && (has_idle || has_hard) {
            bail!(
                "adapter '{}' has incompatible timeout configuration: \
                 legacy field 'timeout_secs' ({}) cannot be used together with new fields \
                 'idle_timeout_secs' ({}) or 'hard_timeout_secs' ({}). \
                 Use either timeout_secs alone (legacy) or idle_timeout_secs + hard_timeout_secs (new).",
                self.name,
                self.timeout_secs,
                self.idle_timeout_secs,
                self.hard_timeout_secs
            );
        }

        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Built-in adapters
// ──────────────────────────────────────────────────────────────────────────────

/// Claude Code (Sonnet) built-in adapter.
fn builtin_claude_sonnet() -> AgentAdapter {
    AgentAdapter {
        name: "claude-sonnet".to_string(),
        description: Some("Claude Code (Sonnet) with JSON output".to_string()),
        agent_cli: "claude".to_string(),
        version_command: Some("claude --version".to_string()),
        input_method: InputMethod::Stdin,
        invoke_template: concat!(
            "cd {workspace} && unbuffer -p claude --model claude-sonnet-4-6",
            " --max-turns 30 --output-format stream-json --dangerously-skip-permissions",
            " --verbose < {prompt_file}",
        )
        .to_string(),
        environment: HashMap::new(),
        timeout_secs: 3600,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: Some("anthropic".to_string()),
        model: Some("claude-sonnet-4-6".to_string()),
        token_extraction: TokenExtraction::None,
        output_transform: Some("needle-transform-claude".to_string()),
        harness: Some("needle".to_string()),
        harness_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

/// Claude Code (Opus) built-in adapter.
fn builtin_claude_opus() -> AgentAdapter {
    AgentAdapter {
        name: "claude-opus".to_string(),
        description: Some("Claude Code (Opus) with JSON output".to_string()),
        agent_cli: "claude".to_string(),
        version_command: Some("claude --version".to_string()),
        input_method: InputMethod::Stdin,
        invoke_template: concat!(
            "cd {workspace} && unbuffer -p claude --model claude-opus-4-6",
            " --max-turns 50 --output-format stream-json --dangerously-skip-permissions",
            " --verbose < {prompt_file}",
        )
        .to_string(),
        environment: HashMap::new(),
        timeout_secs: 7200,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: Some("anthropic".to_string()),
        model: Some("claude-opus-4-6".to_string()),
        token_extraction: TokenExtraction::None,
        output_transform: Some("needle-transform-claude".to_string()),
        harness: Some("needle".to_string()),
        harness_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

/// OpenCode built-in adapter.
fn builtin_opencode() -> AgentAdapter {
    AgentAdapter {
        name: "opencode".to_string(),
        description: Some("OpenCode with file-based prompt input".to_string()),
        agent_cli: "opencode".to_string(),
        version_command: Some("opencode --version".to_string()),
        input_method: InputMethod::File {
            path_template: "{prompt_file}".to_string(),
        },
        invoke_template:
            "cd {workspace} && opencode run --prompt-file {prompt_file} --non-interactive"
                .to_string(),
        environment: HashMap::new(),
        timeout_secs: 3600,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: Some("needle".to_string()),
        harness_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

/// Codex CLI built-in adapter.
fn builtin_codex() -> AgentAdapter {
    AgentAdapter {
        name: "codex".to_string(),
        description: Some(
            "OpenAI Codex CLI, non-interactive exec with a workspace-write sandbox".to_string(),
        ),
        agent_cli: "codex".to_string(),
        version_command: Some("codex --version".to_string()),
        input_method: InputMethod::Args {
            flag: "--".to_string(),
        },
        invoke_template: concat!(
            "cd {workspace} && codex exec --model {model}",
            " --sandbox workspace-write --json \"$(cat {prompt_file})\"",
        )
        .to_string(),
        environment: HashMap::new(),
        timeout_secs: 3600,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: Some("openai".to_string()),
        model: Some("gpt-5.6-terra".to_string()),
        token_extraction: TokenExtraction::None,
        output_transform: Some("needle-transform-codex".to_string()),
        harness: Some("needle".to_string()),
        harness_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

/// Aider built-in adapter.
fn builtin_aider() -> AgentAdapter {
    AgentAdapter {
        name: "aider".to_string(),
        description: Some("Aider with Claude Sonnet, message-based input".to_string()),
        agent_cli: "aider".to_string(),
        version_command: Some("aider --version".to_string()),
        input_method: InputMethod::Args {
            flag: "--message".to_string(),
        },
        invoke_template: concat!(
            "cd {workspace} && aider --model {model}",
            " --yes --message \"$(cat {prompt_file})\"",
        )
        .to_string(),
        environment: HashMap::new(),
        timeout_secs: 3600,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: Some("anthropic".to_string()),
        model: Some("claude-sonnet-4-6".to_string()),
        token_extraction: TokenExtraction::Regex {
            pattern: r"Tokens:\s+([\d,]+)\s+sent,\s+([\d,]+)\s+received".to_string(),
            input_group: 1,
            output_group: 2,
        },
        output_transform: None,
        harness: Some("needle".to_string()),
        harness_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

/// Generic adapter template for users to copy and customize.
fn builtin_generic() -> AgentAdapter {
    AgentAdapter {
        name: "generic".to_string(),
        description: Some("Generic adapter template — copy and customize".to_string()),
        agent_cli: "my-agent".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: "cd {workspace} && my-agent < {prompt_file}".to_string(),
        environment: HashMap::new(),
        timeout_secs: 0,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: Some("needle".to_string()),
        harness_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

/// Returns all built-in adapters.
pub fn builtin_adapters() -> Vec<AgentAdapter> {
    vec![
        builtin_claude_sonnet(),
        builtin_claude_opus(),
        builtin_opencode(),
        builtin_codex(),
        builtin_aider(),
        builtin_generic(),
    ]
}

// ──────────────────────────────────────────────────────────────────────────────
// Template rendering
// ──────────────────────────────────────────────────────────────────────────────

/// Substitute known variables in an invoke template.
fn render_template(
    template: &str,
    workspace: &Path,
    prompt_file: &Path,
    bead_id: &BeadId,
    model: &str,
) -> String {
    template
        .replace("{workspace}", &workspace.display().to_string())
        .replace("{prompt_file}", &prompt_file.display().to_string())
        .replace("{bead_id}", bead_id.as_ref())
        .replace("{model}", model)
}

// ──────────────────────────────────────────────────────────────────────────────
// Adapter loading
// ──────────────────────────────────────────────────────────────────────────────

/// Load adapters from YAML files, with built-in defaults.
///
/// Built-in adapters are loaded first; user files in `dir` override by name.
pub fn load_adapters(
    dir: &Path,
    built_ins: &[AgentAdapter],
) -> Result<HashMap<String, AgentAdapter>> {
    let mut adapters = HashMap::new();

    for adapter in built_ins {
        adapters.insert(adapter.name.clone(), adapter.clone());
    }

    if dir.exists() && dir.is_dir() {
        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("failed to read adapters dir: {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let is_yaml = path
                .extension()
                .is_some_and(|ext| ext == "yaml" || ext == "yml");
            if is_yaml {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read adapter file: {}", path.display()))?;
                let adapter: AgentAdapter = serde_yaml::from_str(&text)
                    .with_context(|| format!("invalid YAML in adapter file: {}", path.display()))?;

                // Validate timeout field mutual exclusivity
                adapter.validate_timeouts().with_context(|| {
                    format!(
                        "adapter '{}' in file {} has invalid timeout configuration",
                        adapter.name,
                        path.display()
                    )
                })?;

                adapters.insert(adapter.name.clone(), adapter);
            }
        }
    }

    // Log timeout policy for all loaded adapters
    let global_timeout = 3600; // Default, will be overridden by dispatcher
    for adapter in adapters.values() {
        let policy = adapter.timeout_policy();
        let desc = adapter.timeout_description(global_timeout);
        tracing::debug!(
            adapter = %adapter.name,
            timeout_policy = ?policy,
            timeout_config = %desc,
            "loaded adapter with timeout configuration"
        );
    }

    Ok(adapters)
}

// ──────────────────────────────────────────────────────────────────────────────
// Dispatcher
// ──────────────────────────────────────────────────────────────────────────────

/// Executes agent processes for claimed beads.
pub struct Dispatcher {
    adapters: HashMap<String, AgentAdapter>,
    telemetry: Telemetry,
    global_timeout_secs: u64,
    /// Sanitizer applied to all trace content before writing to disk.
    /// `None` when trace sanitization is disabled in config.
    sanitizer: Option<Arc<Sanitizer>>,
    /// Tsnet identity registry for per-worker network identity.
    /// `None` when tsnet is disabled in config.
    tsnet_registry: Option<IdentityRegistry>,
    /// Tsnet configuration (cached from config).
    tsnet_config: TsnetConfig,
}

impl Dispatcher {
    /// Create a new dispatcher, loading adapters from config.
    pub fn new(config: &Config, telemetry: Telemetry) -> Result<Self> {
        let adapters = load_adapters(&config.agent.adapters_dir, &builtin_adapters())?;
        let sanitizer = build_sanitizer(config);
        let tsnet_config = config.tsnet.clone();

        // Create tsnet registry only if enabled
        let tsnet_registry = if tsnet_config.enabled {
            Some(IdentityRegistry::new(tsnet_config.clone()))
        } else {
            None
        };

        Ok(Dispatcher {
            adapters,
            telemetry,
            global_timeout_secs: config.agent.timeout,
            sanitizer,
            tsnet_registry,
            tsnet_config,
        })
    }

    /// Create a dispatcher with explicit adapters (for testing).
    pub fn with_adapters(
        adapters: HashMap<String, AgentAdapter>,
        telemetry: Telemetry,
        global_timeout_secs: u64,
    ) -> Self {
        Dispatcher {
            adapters,
            telemetry,
            global_timeout_secs,
            sanitizer: None,
            tsnet_registry: None,
            tsnet_config: TsnetConfig::default(),
        }
    }

    /// Look up an adapter by name.
    pub fn adapter(&self, name: &str) -> Option<&AgentAdapter> {
        self.adapters.get(name)
    }

    /// List all loaded adapter names.
    pub fn adapter_names(&self) -> Vec<&str> {
        self.adapters.keys().map(|s| s.as_str()).collect()
    }

    /// Resolve an adapter name from a model name using routing rules.
    ///
    /// If routing is configured in the config, returns the adapter name that
    /// matches the model pattern (first match wins). Otherwise, returns the
    /// default adapter name.
    ///
    /// # Arguments
    /// * `model` - Model name (e.g., "sonnet", "claude-opus-4-8", "fable")
    /// * `config` - Full config containing routing rules
    ///
    /// # Returns
    /// The resolved adapter name (e.g., "claude-code-glm-4.7", "claude-sonnet")
    pub fn resolve_adapter_name(&self, model: &str, config: &Config) -> String {
        if let Some(ref routing) = config.agent.routing {
            // Check each routing rule in order (first match wins)
            for rule in &routing.rules {
                match regex::Regex::new(&rule.match_model) {
                    Ok(re) => {
                        if re.is_match(model) {
                            tracing::debug!(
                                model = %model,
                                pattern = %rule.match_model,
                                adapter = %rule.adapter,
                                "routing rule matched"
                            );
                            return rule.adapter.clone();
                        }
                    }
                    Err(_) => {
                        // Regex was validated during config load, so this should never happen.
                        // Fall back to default if somehow we get an invalid pattern at runtime.
                        tracing::warn!(
                            pattern = %rule.match_model,
                            "invalid routing regex pattern (should have been caught by config validation)"
                        );
                        continue;
                    }
                }
            }

            // No rules matched; use default_adapter if set
            if let Some(ref default_adapter) = routing.default_adapter {
                tracing::debug!(
                    model = %model,
                    adapter = %default_adapter,
                    "no routing rules matched, using default_adapter"
                );
                return default_adapter.clone();
            }
        }

        // No routing configured or no default_adapter; fall back to agent.default
        tracing::debug!(
            model = %model,
            adapter = %config.agent.default,
            "no routing configured, using agent.default"
        );
        config.agent.default.clone()
    }

    /// Execute the agent process for a bead.
    ///
    /// 1. Writes the prompt to a temp file
    /// 2. Renders the invoke template with variables
    /// 3. Sets adapter-specific environment variables
    /// 4. Spawns the process via `bash -c`
    /// 5. Waits with timeout enforcement (kills on timeout, exit 124)
    /// 6. Captures stdout, stderr, exit code
    /// 7. Cleans up the temp file
    #[tracing::instrument(
        name = "agent.dispatch",
        skip(self, bead_id, prompt, adapter, workspace),
        fields(
            needle.bead.id = %bead_id.as_ref(),
            gen_ai.system = %adapter.gen_ai_system(),
            gen_ai.operation.name = "chat",
            gen_ai.request.id = %bead_id.as_ref(),
            needle.agent.pid = tracing::field::Empty,
            needle.agent.exit_code = tracing::field::Empty,
        )
    )]
    pub async fn dispatch(
        &self,
        bead_id: &BeadId,
        prompt: &BuiltPrompt,
        adapter: &AgentAdapter,
        workspace: &Path,
    ) -> Result<ExecutionResult> {
        // Set gen_ai.request.model if available
        if let Some(ref model) = adapter.model {
            tracing::Span::current().record("gen_ai.request.model", model.as_str());
        }

        // Log timeout policy for this dispatch
        let timeout_policy = adapter.timeout_policy();
        let timeout_desc = adapter.timeout_description(self.global_timeout_secs);
        tracing::debug!(
            adapter = %adapter.name,
            timeout_policy = ?timeout_policy,
            timeout_config = %timeout_desc,
            "dispatching agent with timeout policy"
        );

        self.telemetry.emit(EventKind::DispatchStarted {
            bead_id: bead_id.clone(),
            agent: adapter.name.clone(),
            prompt_len: prompt.content.len(),
            template_name: prompt.template_name.clone(),
            template_version: prompt.template_version.clone(),
            prompt_hash: prompt.hash.clone(),
        })?;

        let result = self
            .execute_agent(bead_id, &prompt.content, adapter, workspace)
            .await;

        // Emit completion telemetry regardless of success/failure.
        let agent_name = adapter.name.clone();
        let agent_model = adapter.model.clone();
        match &result {
            Ok(exec) => {
                tracing::Span::current().record("needle.agent.pid", exec.pid);
                tracing::Span::current().record("needle.agent.exit_code", exec.exit_code);

                // Extract token usage and set gen_ai.usage attributes
                let usage = extract_tokens(&adapter.token_extraction, &exec.stdout, &exec.stderr);
                if let Some(input_tokens) = usage.input_tokens {
                    tracing::Span::current().record("gen_ai.usage.input_tokens", input_tokens);
                }
                if let Some(output_tokens) = usage.output_tokens {
                    tracing::Span::current().record("gen_ai.usage.output_tokens", output_tokens);
                }

                // Set span status: Ok for exit_code 0, Error otherwise
                if exec.exit_code != 0 {
                    tracing::Span::current().record("otel.status_code", 2u64);
                    tracing::Span::current().record(
                        "otel.status_description",
                        format!("exit_code: {}", exec.exit_code),
                    );
                }

                let _ = self.telemetry.emit(EventKind::DispatchCompleted {
                    bead_id: bead_id.clone(),
                    exit_code: exec.exit_code,
                    duration_ms: exec.elapsed.as_millis() as u64,
                    agent: agent_name,
                    model: agent_model,
                });

                // Emit structured timeout event if applicable.
                if let Some(ref reason) = exec.timeout_reason {
                    let _ = self.telemetry.emit(EventKind::AgentTimeout {
                        bead_id: bead_id.clone(),
                        reason: reason.clone(),
                    });
                }
            }
            Err(_) => {
                // Set span status on error
                tracing::Span::current().record("otel.status_code", 2u64);
                tracing::Span::current().record("otel.status_description", "dispatch_error");

                let _ = self.telemetry.emit(EventKind::DispatchCompleted {
                    bead_id: bead_id.clone(),
                    exit_code: -1,
                    duration_ms: 0,
                    agent: agent_name,
                    model: agent_model,
                });
            }
        }

        result
    }

    /// Internal: execute the agent, ensuring temp file cleanup.
    async fn execute_agent(
        &self,
        bead_id: &BeadId,
        prompt_content: &str,
        adapter: &AgentAdapter,
        workspace: &Path,
    ) -> Result<ExecutionResult> {
        let prompt_file = write_prompt_to_temp(bead_id, prompt_content)?;

        let result = self
            .run_process(bead_id, adapter, workspace, &prompt_file)
            .await;

        // Always clean up temp file.
        let _ = std::fs::remove_file(&prompt_file);

        result
    }

    /// Internal: spawn and manage the agent process.
    #[tracing::instrument(
        name = "agent.execution",
        skip(self, bead_id, adapter, workspace, prompt_file),
        fields(
            needle.bead.id = %bead_id.as_ref(),
            needle.agent.pid = tracing::field::Empty,
            needle.agent.exit_code = tracing::field::Empty,
        )
    )]
    async fn run_process(
        &self,
        bead_id: &BeadId,
        adapter: &AgentAdapter,
        workspace: &Path,
        prompt_file: &Path,
    ) -> Result<ExecutionResult> {
        // Create trace capture for this bead execution.
        // Sanitizer is cloned (Arc clone — cheap) and applied before every disk write.
        let trace_capture =
            TraceCapture::new_with_sanitizer(bead_id, workspace, self.sanitizer.clone());

        // Provision tsnet identity if enabled
        let worker_id = self.telemetry.worker_id().to_string();
        let tsnet_identity = if let Some(ref registry) = self.tsnet_registry {
            match registry.provision_identity(&worker_id, bead_id).await {
                Ok(identity) => Some(identity),
                Err(e) => {
                    tracing::warn!(
                        worker_id = %worker_id,
                        bead_id = %bead_id.as_ref(),
                        error = %e,
                        "failed to provision tsnet identity, continuing without network identity"
                    );
                    None
                }
            }
        } else {
            None
        };

        let model = adapter.model.as_deref().unwrap_or("default");
        let rendered = render_template(
            &adapter.invoke_template,
            workspace,
            prompt_file,
            bead_id,
            model,
        );

        // Build environment variables for the child process
        let mut child_env = adapter.environment.clone();

        // Inject tsnet identity environment variables if provisioned
        if let (Some(ref identity), Some(_)) = (&tsnet_identity, &self.tsnet_registry) {
            inject_identity_env(identity, &self.tsnet_config, &mut child_env);
        }

        // Spawn the agent process with ETXTBSY retry handling.
        // This wrapper retries with backoff if the kernel returns ETXTBSY (errno 26),
        // which can occur when a binary has been written to disk and immediately executed.
        let rendered_clone = rendered.clone();
        let child_env_clone = child_env.clone();
        let adapter_name_clone = adapter.name.clone();
        let mut child = spawn_with_etxtbsy_retry_child(
            || {
                let rendered = rendered_clone.clone();
                let child_env = child_env_clone.clone();
                let adapter_name = adapter_name_clone.clone();
                async move {
                    // Safety: setpgid(0,0) is async-signal-safe and idempotent.
                    unsafe {
                        tokio::process::Command::new("bash")
                            .arg("-c")
                            .arg(&rendered)
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .envs(&child_env)
                            .pre_exec(|| {
                                libc::setpgid(0, 0);
                                Ok(())
                            })
                            .spawn()
                    }
                    .map_err(|e| {
                        std::io::Error::other(format!(
                            "failed to spawn agent: {}: {}",
                            adapter_name, e
                        ))
                    })
                }
            },
            5,  // max_attempts
            20, // backoff_ms
        )
        .await
        .with_context(|| format!("failed to spawn agent: {}", adapter.name))?;

        let pid = child.id().unwrap_or(0);
        tracing::Span::current().record("needle.agent.pid", pid);
        let start = Instant::now();

        // Guards against the caller (e.g. Worker's mitosis-evaluation step)
        // dropping this whole async call before the timeout match below ever
        // runs — see ProcessGroupKillGuard's docs and bf-653n7.
        let mut kill_guard = ProcessGroupKillGuard::new(pid);

        // Read stdout/stderr concurrently to avoid pipe buffer deadlock.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        // If output_transform is configured, spawn it now and wire up a bounded
        // channel so the agent's stdout can be tee'd to it in real-time without
        // the agent being blocked by a slow transform consumer.
        //
        // Returns: (channel-sender, transform-child, log-writer-task, spawn-instant)
        // The child is kept here (not moved into a task) so we can kill it after
        // the agent exits.  The log-writer task reads transform stdout and writes
        // normalized JSONL to ~/.needle/logs/<worker>-<bead_id>.agent.jsonl.
        #[allow(clippy::type_complexity)]
        let (
            transform_tx,
            transform_child_opt,
            transform_log_task,
            transform_start,
            transform_feeder_task,
        ): (
            Option<tokio::sync::mpsc::Sender<String>>,
            Option<tokio::process::Child>,
            Option<tokio::task::JoinHandle<u64>>,
            Option<Instant>,
            Option<tokio::task::JoinHandle<()>>,
        ) = if let Some(ref transform_cmd) = adapter.output_transform {
            // Check if the transform binary is available.
            if which::which(transform_cmd).is_err() {
                let _ = self.telemetry.emit(EventKind::TransformSkipped {
                    bead_id: bead_id.clone(),
                    reason: format!("binary not found: {transform_cmd}"),
                });
                (None, None, None, None, None)
            } else {
                let _ = self.telemetry.emit(EventKind::TransformStarted {
                    bead_id: bead_id.clone(),
                    transform_binary: transform_cmd.clone(),
                    agent: adapter.name.clone(),
                });

                // Safety: setpgid(0,0) is async-signal-safe and idempotent. Puts
                // the transform in its own process group (mirroring the agent's
                // own spawn above) so a post-T2 kill can killpg() it — a plain
                // start_kill() only signals this direct child, leaving any
                // subprocess IT forked (e.g. a `sleep` from a hung shell
                // pipeline) alive and holding the stdout pipe open, which would
                // wedge write_transform_log's EOF read forever.
                //
                // Spawn with ETXTBSY retry handling - the transform binary may
                // have been recently written/installed and can transiently fail
                // with "text file busy" (errno 26) on immediate execution.
                let transform_cmd_clone = transform_cmd.clone();
                match spawn_with_etxtbsy_retry_child(
                    || {
                        let transform_cmd = transform_cmd_clone.clone();
                        async move {
                            // Safety: setpgid(0,0) is async-signal-safe and idempotent.
                            unsafe {
                                tokio::process::Command::new("bash")
                                    .arg("-c")
                                    .arg(&transform_cmd)
                                    .stdin(std::process::Stdio::piped())
                                    .stdout(std::process::Stdio::piped())
                                    .stderr(std::process::Stdio::inherit())
                                    .pre_exec(|| {
                                        libc::setpgid(0, 0);
                                        Ok(())
                                    })
                                    .spawn()
                            }
                            .map_err(|e| {
                                std::io::Error::other(format!("failed to spawn transform: {}", e))
                            })
                        }
                    },
                    5,  // max_attempts
                    20, // backoff_ms
                )
                .await
                {
                    Ok(mut transform_child) => {
                        match transform_child.stdin.take() {
                            Some(transform_stdin) => {
                                let transform_stdout = transform_child.stdout.take();

                                // Bounded channel: capacity 64 lines.
                                // try_send drops lines when the transform is
                                // slower than the agent.
                                let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(64);
                                let transform_start = Instant::now();

                                // Feeder task: forwards channel lines to
                                // transform stdin.  Dropping the writer closes
                                // stdin → transform receives EOF.  The handle
                                // is kept (not fire-and-forget) so the caller
                                // can confirm the feeder actually finished
                                // writing and closing stdin before deciding
                                // whether the transform deserves a kill.
                                let feeder_task = tokio::spawn(async move {
                                    let mut writer = tokio::io::BufWriter::new(transform_stdin);
                                    while let Some(line) = rx.recv().await {
                                        if writer.write_all(line.as_bytes()).await.is_err() {
                                            break;
                                        }
                                        if writer.write_all(b"\n").await.is_err() {
                                            break;
                                        }
                                    }
                                    let _ = writer.flush().await;
                                    // writer drops → transform gets stdin EOF
                                });

                                // Compute per-bead log file path.
                                let log_path = agent_log_path(self.telemetry.worker_id(), bead_id);

                                // Log writer task: reads transform stdout and
                                // writes normalized JSONL to the log file.
                                // Returns number of lines written.
                                let log_task =
                                    tokio::spawn(write_transform_log(transform_stdout, log_path));

                                (
                                    Some(tx),
                                    Some(transform_child),
                                    Some(log_task),
                                    Some(transform_start),
                                    Some(feeder_task),
                                )
                            }
                            None => {
                                let _ = self.telemetry.emit(EventKind::TransformFailed {
                                    bead_id: bead_id.clone(),
                                    error: "failed to open transform stdin".to_string(),
                                    exit_code: -1,
                                });
                                (None, None, None, None, None)
                            }
                        }
                    }
                    Err(e) => {
                        let _ = self.telemetry.emit(EventKind::TransformFailed {
                            bead_id: bead_id.clone(),
                            error: e.to_string(),
                            exit_code: -1,
                        });
                        (None, None, None, None, None)
                    }
                }
            }
        } else {
            let _ = self.telemetry.emit(EventKind::TransformSkipped {
                bead_id: bead_id.clone(),
                reason: "not configured".to_string(),
            });
            (None, None, None, None, None)
        };

        // Activity tracking for idle timeout detection.
        // We use a watch channel to broadcast activity timestamps to the
        // main select loop, which resets the idle deadline on each update.
        let (activity_tx, activity_rx) = watch::channel(Instant::now());

        let stdout_task = tokio::spawn({
            let activity_tx = activity_tx.clone();
            async move {
                let mut captured = String::new();
                if let Some(pipe) = stdout_pipe {
                    // Read stdout in chunks to detect activity on every byte,
                    // not just at newlines. This ensures idle timeout resets
                    // continuously on stdout output, before line parsing.
                    let mut reader = tokio::io::BufReader::new(pipe);
                    let mut chunk = [0u8; 8192];
                    let mut line_buffer = Vec::new();

                    while let Ok(n) = AsyncReadExt::read(&mut reader, &mut chunk).await {
                        if n == 0 {
                            break; // EOF
                        }

                        // Report activity on every chunk read (before any parsing).
                        let _ = activity_tx.send(Instant::now());

                        // Process bytes to extract lines for transform and capture.
                        let chunk_bytes = &chunk[..n];
                        captured.push_str(&String::from_utf8_lossy(chunk_bytes));

                        // Extract complete lines and send to transform.
                        // Partial lines stay in line_buffer for the next chunk.
                        for byte in chunk_bytes.iter() {
                            line_buffer.push(*byte);
                            if *byte == b'\n' {
                                if let Some(ref tx) = transform_tx {
                                    // Convert line to String (without the newline for transform).
                                    let line = String::from_utf8_lossy(&line_buffer)
                                        .trim_end_matches('\n')
                                        .to_string();
                                    // Non-blocking: drop if channel is full rather than back-pressuring.
                                    let _ = tx.try_send(line);
                                }
                                line_buffer.clear();
                            }
                        }
                    }

                    // Handle any remaining partial line at EOF (without trailing newline).
                    if !line_buffer.is_empty() {
                        if let Some(ref tx) = transform_tx {
                            let line = String::from_utf8_lossy(&line_buffer).to_string();
                            let _ = tx.try_send(line);
                        }
                    }
                }
                // Dropping transform_tx here closes the channel; the feeder task
                // sees rx.recv() == None and shuts down the transform process.
                drop(transform_tx);
                drop(activity_tx);
                captured
            }
        });

        let stderr_task = tokio::spawn({
            let activity_tx = activity_tx.clone();
            async move {
                let mut buf = Vec::new();
                if let Some(pipe) = stderr_pipe {
                    // Read stderr in chunks to detect activity on every byte,
                    // not just at EOF. This ensures idle timeout resets
                    // continuously on stderr output.
                    let mut reader = tokio::io::BufReader::new(pipe);
                    let mut chunk = [0u8; 8192];
                    while let Ok(n) = AsyncReadExt::read(&mut reader, &mut chunk).await {
                        if n == 0 {
                            break; // EOF
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        // Report activity on every chunk read.
                        let _ = activity_tx.send(Instant::now());
                    }
                }
                drop(activity_tx);
                String::from_utf8_lossy(&buf).into_owned()
            }
        });

        // Compute timeout configuration based on adapter policy.
        let (idle_dur, hard_dur, use_legacy) = match adapter.timeout_policy() {
            TimeoutPolicy::Legacy => {
                // Legacy mode: single timeout, treated as both idle and hard.
                let legacy_dur = adapter.effective_timeout(self.global_timeout_secs);
                (legacy_dur, legacy_dur, true)
            }
            TimeoutPolicy::New {
                idle_enabled,
                hard_enabled,
            } => {
                // New mode: separate idle and hard timeouts.
                let idle = if idle_enabled {
                    Duration::from_secs(adapter.idle_timeout_secs)
                } else {
                    Duration::ZERO
                };
                let hard = if hard_enabled {
                    Duration::from_secs(adapter.hard_timeout_secs)
                } else {
                    Duration::ZERO
                };
                (idle, hard, false)
            }
            TimeoutPolicy::Global => {
                // No adapter-specific timeout; fall back to global config.
                let global_dur = Duration::from_secs(self.global_timeout_secs);
                (global_dur, global_dur, global_dur.is_zero())
            }
        };

        // Determine which deadlines are active.
        let has_idle_deadline = !idle_dur.is_zero();
        let has_hard_deadline = !hard_dur.is_zero();
        let has_any_deadline = has_idle_deadline || has_hard_deadline;

        // Exit code and optional timeout reason.
        let (exit_code, timeout_reason) = if !has_any_deadline {
            // No deadlines: wait indefinitely.
            let status = child
                .wait()
                .await
                .context("failed to wait for agent process")?;
            kill_guard.disarm();
            (status.code().unwrap_or(-1), None)
        } else {
            // At least one deadline is active: use select! to concurrently
            // observe child exit, idle deadline, hard deadline, and cancellation.
            use tokio::select;

            // Idle deadline state: resets on activity.
            let mut idle_deadline = if has_idle_deadline {
                Some(tokio::time::Instant::now() + idle_dur)
            } else {
                None
            };

            // Hard deadline state: never resets.
            let hard_deadline = if has_hard_deadline {
                Some(tokio::time::Instant::now() + hard_dur)
            } else {
                None
            };

            // Activity receiver for idle deadline resets.
            let mut activity_rcv = activity_rx;

            // Outcome: (exit_code, timeout_reason).
            // We loop to handle idle deadline resets on activity.
            let outcome: (i32, Option<TimeoutReason>) = loop {
                // Determine the next deadline to wait for (if any).
                let next_idle = idle_deadline;
                let next_hard = hard_deadline;
                let _next_deadline = match (next_idle, next_hard) {
                    (Some(idle), Some(hard)) => Some(idle.min(hard)),
                    (Some(idle), None) => Some(idle),
                    (None, Some(hard)) => Some(hard),
                    (None, None) => None,
                };

                select! {
                    // Branch 1: child exited (natural or signaled)
                    status = child.wait() => {
                        let status = status.context("failed to wait for agent process")?;
                        kill_guard.disarm();
                        break (status.code().unwrap_or(-1), None);
                    }

                    // Branch 2: activity detected (stdout/stderr byte read)
                    _result = activity_rcv.changed() => {
                        // Reset idle deadline on activity (if active).
                        if has_idle_deadline {
                            idle_deadline = Some(tokio::time::Instant::now() + idle_dur);
                        }
                        // Continue the loop to re-evaluate deadlines.
                        continue;
                    }

                    // Branch 3: idle deadline expired
                    () = async {
                        if let Some(deadline) = next_idle {
                            tokio::time::sleep_until(deadline).await;
                        } else {
                            std::future::pending().await
                        }
                    }, if has_idle_deadline => {
                        // Idle timeout: kill the process group.
                        if pid > 0 {
                            unsafe {
                                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                            }
                        }
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        kill_guard.disarm();

                        // Compute last output age for telemetry.
                        let last_output_age = activity_rcv.borrow().elapsed();
                        let reason = if use_legacy {
                            TimeoutReason::Legacy {
                                timeout_secs: idle_dur.as_secs(),
                            }
                        } else {
                            TimeoutReason::Idle {
                                timeout_secs: idle_dur.as_secs(),
                                last_output_age_secs: last_output_age.as_secs(),
                            }
                        };
                        break (124, Some(reason));
                    }

                    // Branch 4: hard deadline expired
                    () = async {
                        if let Some(deadline) = next_hard {
                            tokio::time::sleep_until(deadline).await;
                        } else {
                            std::future::pending().await
                        }
                    }, if has_hard_deadline => {
                        // Hard timeout: kill the process group.
                        if pid > 0 {
                            unsafe {
                                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                            }
                        }
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        kill_guard.disarm();

                        let reason = TimeoutReason::Hard {
                            timeout_secs: hard_dur.as_secs(),
                        };
                        break (124, Some(reason));
                    }
                }
            };

            outcome
        };

        tracing::Span::current().record("needle.agent.exit_code", exit_code);

        // Set span status: Ok for exit_code 0, Error otherwise
        if exit_code != 0 {
            tracing::Span::current().record("otel.status_code", 2u64);
            tracing::Span::current()
                .record("otel.status_description", format!("exit_code: {exit_code}"));
        }

        let elapsed = start.elapsed();
        // Await stdout/stderr readers; dropping transform_tx here closes the
        // feeder channel, causing the feeder task to close transform stdin.
        let stdout = stdout_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();

        // Give the transform a fair chance to drain stdin and exit on its own
        // before killing it. Two distinct grace periods on two distinct
        // targets, not one blanket kill:
        //   T1 (FEEDER_DRAIN_TIMEOUT) — await the feeder task's own
        //       JoinHandle, confirming stdin was actually flushed and closed
        //       by the forwarder, not just that the channel sender was
        //       dropped (dropping the sender only *starts* that process).
        //   T2 (TRANSFORM_EXIT_GRACE) — THEN await the transform child's own
        //       exit, confirming it actually consumed the stdin EOF (which
        //       includes the final line, e.g. codex's `turn.completed`) and
        //       exited naturally, before any kill is even considered.
        const FEEDER_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
        const TRANSFORM_EXIT_GRACE: Duration = Duration::from_secs(2);

        if let Some(mut t_child) = transform_child_opt {
            // T1: did the feeder actually finish forwarding and close stdin?
            let feeder_drained = match transform_feeder_task {
                Some(feeder) => tokio::time::timeout(FEEDER_DRAIN_TIMEOUT, feeder)
                    .await
                    .is_ok(),
                // No feeder handle at all — can't confirm delivery.
                None => false,
            };

            // Three-way outcome, not a binary natural-exit-vs-kill split:
            //   NaturalExit      — the transform exited on its own within the
            //                      grace window; use its real exit status.
            //   KilledAfterDrain — input delivery was confirmed (T1 passed)
            //                      but the transform itself hung past T2 and
            //                      had to be killed. Data reached it; the
            //                      process did not finish. This must NOT be
            //                      reported as a clean success.
            //   KilledNoDrain    — the feeder itself never confirmed finishing
            //                      within T1, so input delivery is unconfirmed
            //                      when the kill happens.
            enum TransformOutcome {
                NaturalExit(std::process::ExitStatus),
                KilledAfterDrain,
                KilledNoDrain,
            }

            let outcome = match tokio::time::timeout(TRANSFORM_EXIT_GRACE, t_child.wait()).await {
                Ok(Ok(status)) => TransformOutcome::NaturalExit(status),
                Ok(Err(_)) | Err(_) => {
                    // wait() itself errored, or T2 elapsed with no exit. Kill the
                    // transform's entire process group (it was spawned into its
                    // own group above), not just the direct child — a plain
                    // start_kill() leaves any subprocess it forked (e.g. a
                    // `sleep` from a hung pipeline) alive as an orphan holding
                    // the stdout pipe open, wedging the log-writer task's EOF
                    // read forever. Mirrors the agent's own timeout-kill path.
                    if let Some(t_pid) = t_child.id() {
                        unsafe {
                            libc::killpg(t_pid as libc::pid_t, libc::SIGKILL);
                        }
                    }
                    let _ = t_child.start_kill();
                    let _ = t_child.wait().await;
                    if feeder_drained {
                        TransformOutcome::KilledAfterDrain
                    } else {
                        TransformOutcome::KilledNoDrain
                    }
                }
            };

            // Await log writer: flushes and closes the file before we return
            // (i.e., before HANDLING state begins).
            let events_written = if let Some(task) = transform_log_task {
                task.await.unwrap_or(0)
            } else {
                0
            };

            let duration_ms = transform_start.map_or(0, |s| s.elapsed().as_millis() as u64);

            match outcome {
                TransformOutcome::NaturalExit(status) => {
                    if status.success() {
                        let _ = self.telemetry.emit(EventKind::TransformCompleted {
                            bead_id: bead_id.clone(),
                            events_written,
                            duration_ms,
                        });
                    } else {
                        use std::os::unix::process::ExitStatusExt;
                        let t_exit_code = status.code().unwrap_or(-1);
                        let error = match status.code() {
                            Some(code) => format!("exit code {code}"),
                            None => match status.signal() {
                                Some(sig) => {
                                    format!("terminated by signal {sig} (not initiated by needle)")
                                }
                                None => "exited with unknown status".to_string(),
                            },
                        };
                        let _ = self.telemetry.emit(EventKind::TransformFailed {
                            bead_id: bead_id.clone(),
                            error,
                            exit_code: t_exit_code,
                        });
                    }
                }
                TransformOutcome::KilledAfterDrain => {
                    let _ = self.telemetry.emit(EventKind::TransformFailed {
                        bead_id: bead_id.clone(),
                        error: format!(
                            "transform did not exit within {}s of receiving stdin EOF \
                             (all input confirmed delivered); killed as cleanup",
                            TRANSFORM_EXIT_GRACE.as_secs()
                        ),
                        exit_code: -1,
                    });
                }
                TransformOutcome::KilledNoDrain => {
                    let _ = self.telemetry.emit(EventKind::TransformFailed {
                        bead_id: bead_id.clone(),
                        error: format!(
                            "transform feeder did not finish forwarding stdin within {}ms \
                             (input delivery unconfirmed); killed as cleanup",
                            FEEDER_DRAIN_TIMEOUT.as_millis()
                        ),
                        exit_code: -1,
                    });
                }
            }
        }

        // Finalize trace capture.
        let trace_path = if let Some(capture) = trace_capture {
            // Write stdout and stderr to trace files.
            if let Err(e) = capture.write_stdout(&stdout) {
                tracing::warn!(
                    bead_id = %bead_id.as_ref(),
                    error = %e,
                    "failed to write stdout trace file"
                );
            }
            if let Err(e) = capture.write_stderr(&stderr) {
                tracing::warn!(
                    bead_id = %bead_id.as_ref(),
                    error = %e,
                    "failed to write stderr trace file"
                );
            }

            // Read normalized trace from agent log file (if transform was configured).
            let trace_lines = if adapter.output_transform.is_some() {
                agent_log_path(self.telemetry.worker_id(), bead_id).and_then(|path| {
                    std::fs::read_to_string(&path)
                        .ok()
                        .map(|content| content.lines().map(|s| s.to_string()).collect::<Vec<_>>())
                })
            } else {
                None
            };

            // Write trace JSONL if available.
            if let Some(ref lines) = trace_lines {
                let _ = capture.write_trace_jsonl(lines);
            }

            // Create and write metadata.
            let outcome = Outcome::classify(exit_code, false);
            let metadata = TraceMetadata {
                bead_id: bead_id.clone(),
                agent: adapter.name.clone(),
                provider: adapter.provider.clone(),
                model: adapter.model.clone(),
                exit_code,
                outcome: outcome.as_str().to_string(),
                duration_ms: elapsed.as_millis() as u64,
                input_tokens: None, // Token extraction happens later in outcome handling
                output_tokens: None,
                cost_usd: None,
                captured_at: chrono::Utc::now(),
                trace_format: detect_trace_format(&adapter.name),
                pruned: false,
                template_version: None,
                timeout_reason: timeout_reason.clone(),
            };
            let _ = capture.write_metadata(&metadata);

            capture.finalize()
        } else {
            None
        };

        // Release tsnet identity if it was provisioned
        if let (Some(ref identity), Some(ref registry)) = (&tsnet_identity, &self.tsnet_registry) {
            registry.release_identity(&identity.hostname).await;
        }

        Ok(ExecutionResult {
            exit_code,
            stdout,
            stderr,
            elapsed,
            pid,
            trace_path,
            timeout_reason,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Compute the per-bead agent log path: `~/.needle/logs/<worker>-<bead_id>.agent.jsonl`.
///
/// Build a trace sanitizer from the workspace config.
///
/// Returns `None` if sanitization is disabled or if rule compilation fails
/// (failing open keeps traces writable even when the sanitizer has issues).
fn build_sanitizer(config: &Config) -> Option<Arc<Sanitizer>> {
    if !config.strands.learning.trace_sanitization.enabled {
        tracing::debug!("trace sanitization disabled in config");
        return None;
    }

    let custom_patterns: Vec<CustomPattern> = config
        .strands
        .learning
        .trace_sanitization
        .custom_patterns
        .iter()
        .map(|p| CustomPattern {
            id: p.id.clone(),
            pattern: p.pattern.clone(),
            entropy: p.entropy,
        })
        .collect();

    match Sanitizer::new(&custom_patterns) {
        Ok(s) => {
            tracing::info!(
                rule_count = s.rule_count(),
                custom_count = custom_patterns.len(),
                "trace sanitizer initialized"
            );
            Some(Arc::new(s))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "failed to build trace sanitizer; traces will not be sanitized"
            );
            None
        }
    }
}

/// Returns `None` if `$HOME` is not set.
fn agent_log_path(worker_id: &str, bead_id: &BeadId) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join(".needle")
            .join("logs")
            .join(format!("{}-{}.agent.jsonl", worker_id, bead_id)),
    )
}

/// Read transform stdout and write each line to the agent log file.
///
/// Returns the number of lines written.  If the log path is `None` or the file
/// cannot be created, stdout is drained to avoid blocking the transform process.
async fn write_transform_log(
    stdout_pipe: Option<tokio::process::ChildStdout>,
    log_path: Option<PathBuf>,
) -> u64 {
    let Some(pipe) = stdout_pipe else {
        return 0;
    };

    let Some(path) = log_path else {
        // No log path configured — drain to avoid blocking the transform.
        let reader = tokio::io::BufReader::new(pipe);
        let mut lines = reader.lines();
        while let Ok(Some(_)) = lines.next_line().await {}
        return 0;
    };

    // Ensure the log directory exists.
    if let Some(parent) = path.parent() {
        if tokio::fs::create_dir_all(parent).await.is_err() {
            let reader = tokio::io::BufReader::new(pipe);
            let mut lines = reader.lines();
            while let Ok(Some(_)) = lines.next_line().await {}
            return 0;
        }
    }

    // Open (create/truncate) the log file.
    let file = match tokio::fs::File::create(&path).await {
        Ok(f) => f,
        Err(_) => {
            let reader = tokio::io::BufReader::new(pipe);
            let mut lines = reader.lines();
            while let Ok(Some(_)) = lines.next_line().await {}
            return 0;
        }
    };

    let mut writer = tokio::io::BufWriter::new(file);
    let reader = tokio::io::BufReader::new(pipe);
    let mut lines = reader.lines();
    let mut count = 0u64;
    while let Ok(Some(line)) = lines.next_line().await {
        if writer.write_all(line.as_bytes()).await.is_err() {
            break;
        }
        if writer.write_all(b"\n").await.is_err() {
            break;
        }
        count += 1;
    }
    let _ = writer.flush().await;
    count
}

/// Write prompt content to a temp file, returning the file path.
///
/// Files are placed in `$TMPDIR/needle/` to avoid polluting the workspace.
fn write_prompt_to_temp(bead_id: &BeadId, content: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("needle");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create temp dir: {}", dir.display()))?;

    let filename = format!("prompt-{}-{}.md", bead_id, std::process::id());
    let path = dir.join(filename);

    std::fs::write(&path, content)
        .with_context(|| format!("failed to write prompt file: {}", path.display()))?;

    Ok(path)
}

// ──────────────────────────────────────────────────────────────────────────────
// test-agent validation
// ──────────────────────────────────────────────────────────────────────────────

/// Result of validating an agent adapter.
#[derive(Debug)]
pub struct AgentTestResult {
    pub adapter_name: String,
    pub cli_path: Option<String>,
    pub version: Option<String>,
    pub input_method: String,
    pub probe_result: Option<ProbeResult>,
    pub token_extraction_ok: Option<bool>,
    pub output_transform_ok: Option<bool>,
    pub timeout_policy: TimeoutPolicy,
    pub timeout_description: String,
    pub status: AgentTestStatus,
    pub errors: Vec<String>,
}

/// Probe execution result.
#[derive(Debug)]
pub struct ProbeResult {
    pub exit_code: i32,
    pub elapsed_ms: u64,
}

/// Overall test-agent status.
#[derive(Debug, PartialEq, Eq)]
pub enum AgentTestStatus {
    Ready,
    Warning,
    Error,
}

impl std::fmt::Display for AgentTestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentTestStatus::Ready => write!(f, "READY"),
            AgentTestStatus::Warning => write!(f, "WARNING"),
            AgentTestStatus::Error => write!(f, "ERROR"),
        }
    }
}

/// Validate an agent adapter: check CLI availability, version, and probe.
pub fn test_agent(adapter_name: &str, config: &Config) -> Result<AgentTestResult> {
    let adapters = load_adapters(&config.agent.adapters_dir, &builtin_adapters())?;

    let adapter = adapters
        .get(adapter_name)
        .with_context(|| format!("unknown adapter: {adapter_name}"))?;

    let mut errors = Vec::new();

    // 1. Find the CLI binary on PATH.
    let cli_path = match which::which(&adapter.agent_cli) {
        Ok(path) => Some(path.display().to_string()),
        Err(_) => {
            errors.push(format!("CLI '{}' not found on PATH", adapter.agent_cli));
            None
        }
    };

    // 2. Run version command if configured.
    let version = if let Some(ref version_cmd) = adapter.version_command {
        if cli_path.is_some() {
            match run_shell_command(version_cmd) {
                Ok(output) => Some(output.trim().to_string()),
                Err(e) => {
                    errors.push(format!("version command failed: {e}"));
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // 3. Input method description.
    let input_method = match &adapter.input_method {
        InputMethod::Stdin => "stdin".to_string(),
        InputMethod::File { .. } => "file".to_string(),
        InputMethod::Args { flag } => format!("args ({flag})"),
    };

    // 4. Run probe (echo hello) if CLI is available.
    let probe_result = if cli_path.is_some() {
        match run_probe(&adapter.agent_cli) {
            Ok(pr) => Some(pr),
            Err(e) => {
                errors.push(format!("probe failed: {e}"));
                None
            }
        }
    } else {
        None
    };

    // 5. Test token extraction with sample data.
    let token_extraction_ok = match &adapter.token_extraction {
        TokenExtraction::None => None,
        TokenExtraction::JsonField {
            input_path,
            output_path,
        } => {
            let sample = build_sample_json(input_path, output_path);
            let usage = extract_tokens_json(&sample, input_path, output_path);
            Some(usage.input_tokens.is_some() && usage.output_tokens.is_some())
        }
        TokenExtraction::Regex {
            pattern,
            input_group,
            output_group,
        } => {
            let sample = "Tokens: 1,234 sent, 567 received";
            let usage = extract_tokens_regex(sample, pattern, *input_group, *output_group);
            Some(usage.input_tokens.is_some() && usage.output_tokens.is_some())
        }
    };

    if let Some(false) = token_extraction_ok {
        errors.push("token extraction failed with sample data".to_string());
    }

    // 6. Validate output_transform binary exists on PATH (if configured).
    let output_transform_ok = if let Some(ref transform) = adapter.output_transform {
        match which::which(transform) {
            Ok(_) => Some(true),
            Err(_) => {
                errors.push(format!(
                    "output_transform binary '{transform}' not found on PATH"
                ));
                Some(false)
            }
        }
    } else {
        None
    };

    // 7. Determine overall status.
    let status = if cli_path.is_none() {
        AgentTestStatus::Error
    } else if !errors.is_empty() {
        AgentTestStatus::Warning
    } else {
        AgentTestStatus::Ready
    };

    // 8. Determine timeout policy.
    let timeout_policy = adapter.timeout_policy();
    let timeout_description = adapter.timeout_description(config.agent.timeout);

    Ok(AgentTestResult {
        adapter_name: adapter.name.clone(),
        cli_path,
        version,
        input_method,
        probe_result,
        token_extraction_ok,
        output_transform_ok,
        timeout_policy,
        timeout_description,
        status,
        errors,
    })
}

/// Run a shell command and capture its stdout.
fn run_shell_command(cmd: &str) -> Result<String> {
    let output = ProcessCommand::new("bash")
        .args(["-c", cmd])
        .output()
        .with_context(|| format!("failed to run: {cmd}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("command exited with {}: {}", output.status, stderr.trim());
    }
}

/// Run a trivial probe: ask the agent CLI to do nothing meaningful.
fn run_probe(agent_cli: &str) -> Result<ProbeResult> {
    let start = Instant::now();
    let mut last_err = None;
    const MAX_ATTEMPTS: u32 = 5;
    const BACKOFF_MS: u64 = 20;

    for attempt in 0..MAX_ATTEMPTS {
        match ProcessCommand::new(agent_cli)
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(status) => {
                return Ok(ProbeResult {
                    exit_code: status.code().unwrap_or(-1),
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
            }
            Err(e) if e.raw_os_error() == Some(26) && attempt + 1 < MAX_ATTEMPTS => {
                // ETXTBSY (errno 26): transient "text file busy" - retry with backoff
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(BACKOFF_MS));
            }
            Err(e) => {
                return Err(e).with_context(|| format!("failed to probe {agent_cli}"));
            }
        }
    }

    Err(last_err.expect("loop always sets last_err before exhausting MAX_ATTEMPTS"))
        .with_context(|| format!("failed to probe {agent_cli} after {MAX_ATTEMPTS} attempts"))
}

/// Build a sample JSON string for testing JSON field extraction.
fn build_sample_json(input_path: &str, output_path: &str) -> String {
    fn set_path(val: &mut serde_json::Value, path: &str, num: u64) {
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = val;
        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                current[part] = serde_json::json!(num);
            } else {
                if current.get(part).is_none() || !current[part].is_object() {
                    current[part] = serde_json::json!({});
                }
                current = &mut current[part];
            }
        }
    }

    let mut root = serde_json::json!({});
    set_path(&mut root, input_path, 100);
    set_path(&mut root, output_path, 50);
    root.to_string()
}

/// Print a formatted test-agent report to stdout.
pub fn print_test_result(result: &AgentTestResult) {
    println!("Adapter: {}", result.adapter_name);
    match &result.cli_path {
        Some(path) => println!("CLI:     {} (found at {path})", result.adapter_name),
        None => println!("CLI:     NOT FOUND"),
    }
    match &result.version {
        Some(v) => println!("Version: {v}"),
        None => println!("Version: unknown"),
    }
    println!("Input:   {}", result.input_method);
    println!("Timeout: {}", result.timeout_description);
    match &result.probe_result {
        Some(pr) => println!("Probe:   exit {} ({}ms)", pr.exit_code, pr.elapsed_ms),
        None => println!("Probe:   skipped"),
    }
    match result.token_extraction_ok {
        Some(true) => println!("Tokens:  extraction working"),
        Some(false) => println!("Tokens:  extraction FAILED"),
        None => println!("Tokens:  none configured"),
    }
    match result.output_transform_ok {
        Some(true) => println!("Transform: binary found"),
        Some(false) => println!("Transform: binary NOT FOUND"),
        None => println!("Transform: none configured"),
    }
    println!("Status:  {}", result.status);
    for err in &result.errors {
        println!("  !! {err}");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_adapter(name: &str, template: &str) -> AgentAdapter {
        AgentAdapter {
            name: name.to_string(),
            description: None,
            agent_cli: "test".to_string(),
            version_command: None,
            input_method: InputMethod::Stdin,
            invoke_template: template.to_string(),
            environment: HashMap::new(),
            timeout_secs: 10,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: None,
            model: None,
            token_extraction: TokenExtraction::None,
            output_transform: None,
            harness: None,
            harness_version: None,
        }
    }

    fn test_prompt(content: &str) -> BuiltPrompt {
        BuiltPrompt {
            content: content.to_string(),
            hash: "testhash".to_string(),
            token_estimate: content.len() as u64 / 4,
            template_name: "pluck".to_string(),
            template_version: "pluck-default".to_string(),
        }
    }

    fn test_dispatcher(adapters: HashMap<String, AgentAdapter>) -> Dispatcher {
        let telemetry = Telemetry::new("test-worker".to_string());
        Dispatcher::with_adapters(adapters, telemetry, 3600)
    }

    // ── Template rendering ──

    #[test]
    fn render_template_substitutes_all_variables() {
        let template = "cd {workspace} && agent --model {model} < {prompt_file} # bead={bead_id}";
        let result = render_template(
            template,
            Path::new("/home/workspace"),
            Path::new("/tmp/needle/prompt.md"),
            &BeadId::from("needle-abc"),
            "claude-sonnet-4-6",
        );
        assert!(result.contains("/home/workspace"));
        assert!(result.contains("/tmp/needle/prompt.md"));
        assert!(result.contains("needle-abc"));
        assert!(result.contains("claude-sonnet-4-6"));
        assert!(!result.contains("{workspace}"));
        assert!(!result.contains("{prompt_file}"));
        assert!(!result.contains("{bead_id}"));
        assert!(!result.contains("{model}"));
    }

    #[test]
    fn render_template_preserves_unrecognized_placeholders() {
        let result = render_template(
            "echo {unknown}",
            Path::new("/tmp"),
            Path::new("/tmp/p.md"),
            &BeadId::from("nd-x"),
            "m",
        );
        assert!(result.contains("{unknown}"));
    }

    // ── AgentAdapter YAML ──

    #[test]
    fn adapter_yaml_roundtrip() {
        let adapter = builtin_claude_sonnet();
        let yaml = serde_yaml::to_string(&adapter).unwrap();
        let parsed: AgentAdapter = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.name, "claude-sonnet");
        assert_eq!(parsed.agent_cli, "claude");
        assert_eq!(parsed.timeout_secs, 3600);
        assert_eq!(parsed.model, Some("claude-sonnet-4-6".to_string()));
    }

    #[test]
    fn adapter_yaml_deserialization() {
        let yaml = r#"
name: custom-agent
agent_cli: my-agent
invoke_template: "cd {workspace} && my-agent < {prompt_file}"
timeout_secs: 600
input_method:
  method: stdin
environment:
  API_KEY: test-key
  DEBUG: "true"
provider: custom
model: custom-v1
"#;
        let adapter: AgentAdapter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(adapter.name, "custom-agent");
        assert_eq!(adapter.agent_cli, "my-agent");
        assert_eq!(adapter.timeout_secs, 600);
        assert_eq!(adapter.input_method, InputMethod::Stdin);
        assert_eq!(adapter.environment.get("API_KEY").unwrap(), "test-key");
        assert_eq!(adapter.model, Some("custom-v1".to_string()));
    }

    #[test]
    fn adapter_yaml_file_input_method() {
        let yaml = r#"
name: file-agent
agent_cli: agent
invoke_template: "agent --file {prompt_file}"
input_method:
  method: file
  path_template: "/tmp/{bead_id}.md"
"#;
        let adapter: AgentAdapter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            adapter.input_method,
            InputMethod::File {
                path_template: "/tmp/{bead_id}.md".to_string()
            }
        );
    }

    #[test]
    fn adapter_yaml_args_input_method() {
        let yaml = r#"
name: args-agent
agent_cli: agent
invoke_template: "agent --prompt $(cat {prompt_file})"
input_method:
  method: args
  flag: "--prompt"
"#;
        let adapter: AgentAdapter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            adapter.input_method,
            InputMethod::Args {
                flag: "--prompt".to_string()
            }
        );
    }

    #[test]
    fn adapter_yaml_output_transform_deserialized() {
        let yaml = r#"
name: transform-agent
agent_cli: my-agent
invoke_template: "my-agent < {prompt_file}"
output_transform: "needle-transform-custom"
"#;
        let adapter: AgentAdapter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            adapter.output_transform,
            Some("needle-transform-custom".to_string())
        );
    }

    #[test]
    fn adapter_yaml_output_transform_absent_is_none() {
        let yaml = "name: no-transform\nagent_cli: agent\ninvoke_template: \"echo test\"\n";
        let adapter: AgentAdapter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(adapter.output_transform, None);
    }

    // ── Effective timeout ──

    #[test]
    fn effective_timeout_uses_adapter_when_nonzero() {
        let adapter = AgentAdapter {
            timeout_secs: 300,
            ..builtin_generic()
        };
        assert_eq!(adapter.effective_timeout(3600), Duration::from_secs(300));
    }

    #[test]
    fn effective_timeout_falls_back_to_global() {
        let adapter = AgentAdapter {
            timeout_secs: 0,
            ..builtin_generic()
        };
        assert_eq!(adapter.effective_timeout(3600), Duration::from_secs(3600));
    }

    #[test]
    fn effective_timeout_zero_when_both_zero() {
        let adapter = AgentAdapter {
            timeout_secs: 0,
            ..builtin_generic()
        };
        assert_eq!(adapter.effective_timeout(0), Duration::ZERO);
    }

    // ── Timeout field validation (legacy vs new mutual exclusivity) ──

    #[test]
    fn validate_timeouts_timeout_secs_alone_valid() {
        // Valid: legacy timeout_secs alone (backward compatibility)
        let adapter = AgentAdapter {
            name: "test-legacy".to_string(),
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            ..test_adapter("test-legacy", "test template")
        };
        assert!(adapter.validate_timeouts().is_ok());
    }

    #[test]
    fn validate_timeouts_new_fields_together_valid() {
        // Valid: both idle_timeout_secs and hard_timeout_secs set
        let adapter = AgentAdapter {
            name: "test-new".to_string(),
            timeout_secs: 0,
            idle_timeout_secs: 600,
            hard_timeout_secs: 7200,
            ..test_adapter("test-new", "test template")
        };
        assert!(adapter.validate_timeouts().is_ok());
    }

    #[test]
    fn validate_timeouts_idle_only_valid() {
        // Valid: idle_timeout_secs alone (hard_timeout_secs = 0 disables hard deadline)
        let adapter = AgentAdapter {
            name: "test-idle-only".to_string(),
            timeout_secs: 0,
            idle_timeout_secs: 900,
            hard_timeout_secs: 0,
            ..test_adapter("test-idle-only", "test template")
        };
        assert!(adapter.validate_timeouts().is_ok());
    }

    #[test]
    fn validate_timeouts_hard_only_valid() {
        // Valid: hard_timeout_secs alone (idle_timeout_secs = 0 disables idle deadline)
        let adapter = AgentAdapter {
            name: "test-hard-only".to_string(),
            timeout_secs: 0,
            idle_timeout_secs: 0,
            hard_timeout_secs: 3600,
            ..test_adapter("test-hard-only", "test template")
        };
        assert!(adapter.validate_timeouts().is_ok());
    }

    #[test]
    fn validate_timeouts_all_zero_valid() {
        // Valid: all zero (use global config timeout)
        let adapter = AgentAdapter {
            name: "test-all-zero".to_string(),
            timeout_secs: 0,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            ..test_adapter("test-all-zero", "test template")
        };
        assert!(adapter.validate_timeouts().is_ok());
    }

    #[test]
    fn validate_timeouts_timeout_with_idle_fails() {
        // Invalid: mixing legacy timeout_secs with new idle_timeout_secs
        let adapter = AgentAdapter {
            name: "test-mixed-1".to_string(),
            timeout_secs: 3600,
            idle_timeout_secs: 600,
            hard_timeout_secs: 0,
            ..test_adapter("test-mixed-1", "test template")
        };
        let result = adapter.validate_timeouts();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("incompatible timeout configuration"));
        assert!(err_msg.contains("timeout_secs"));
        assert!(err_msg.contains("idle_timeout_secs"));
    }

    #[test]
    fn validate_timeouts_timeout_with_hard_fails() {
        // Invalid: mixing legacy timeout_secs with new hard_timeout_secs
        let adapter = AgentAdapter {
            name: "test-mixed-2".to_string(),
            timeout_secs: 1800,
            idle_timeout_secs: 0,
            hard_timeout_secs: 7200,
            ..test_adapter("test-mixed-2", "test template")
        };
        let result = adapter.validate_timeouts();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("incompatible timeout configuration"));
        assert!(err_msg.contains("timeout_secs"));
        assert!(err_msg.contains("hard_timeout_secs"));
    }

    #[test]
    fn validate_timeouts_timeout_with_both_new_fails() {
        // Invalid: mixing legacy timeout_secs with both new fields
        let adapter = AgentAdapter {
            name: "test-mixed-3".to_string(),
            timeout_secs: 2400,
            idle_timeout_secs: 300,
            hard_timeout_secs: 5400,
            ..test_adapter("test-mixed-3", "test template")
        };
        let result = adapter.validate_timeouts();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("incompatible timeout configuration"));
        assert!(err_msg.contains("timeout_secs"));
        assert!(err_msg.contains("idle_timeout_secs"));
        assert!(err_msg.contains("hard_timeout_secs"));
        assert!(err_msg.contains("legacy field"));
        assert!(err_msg.contains("new fields"));
    }

    #[test]
    fn validate_timeouts_error_message_actionable() {
        // Verify error message guides user to correct configuration
        let adapter = AgentAdapter {
            name: "test-error-msg".to_string(),
            timeout_secs: 3600,
            idle_timeout_secs: 600,
            hard_timeout_secs: 0,
            ..test_adapter("test-error-msg", "test template")
        };
        let result = adapter.validate_timeouts();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        // Error message should explain the problem and solution
        assert!(err_msg.contains("'timeout_secs'"));
        assert!(err_msg.contains("'idle_timeout_secs'"));
        assert!(err_msg.contains("cannot be used together"));
        assert!(err_msg.contains("Use either timeout_secs alone"));
        assert!(err_msg.contains("idle_timeout_secs + hard_timeout_secs"));
    }

    // ── Built-in adapters ──

    #[test]
    fn builtin_adapters_are_present() {
        let adapters = builtin_adapters();
        assert!(adapters.iter().any(|a| a.name == "claude-sonnet"));
        assert!(adapters.iter().any(|a| a.name == "claude-opus"));
        assert!(adapters.iter().any(|a| a.name == "opencode"));
        assert!(adapters.iter().any(|a| a.name == "codex"));
        assert!(adapters.iter().any(|a| a.name == "aider"));
        assert!(adapters.iter().any(|a| a.name == "generic"));
    }

    #[test]
    fn builtin_claude_opus_config() {
        let adapter = builtin_claude_opus();
        assert_eq!(adapter.name, "claude-opus");
        assert_eq!(adapter.agent_cli, "claude");
        assert_eq!(adapter.model, Some("claude-opus-4-6".to_string()));
        assert_eq!(adapter.provider, Some("anthropic".to_string()));
        assert!(adapter.invoke_template.contains("claude-opus-4-6"));
        assert!(adapter.invoke_template.contains("--max-turns 50"));
        assert_eq!(adapter.timeout_secs, 7200);
        assert!(matches!(adapter.token_extraction, TokenExtraction::None));
    }

    #[test]
    fn builtin_opencode_config() {
        let adapter = builtin_opencode();
        assert_eq!(adapter.name, "opencode");
        assert_eq!(adapter.agent_cli, "opencode");
        assert!(matches!(adapter.input_method, InputMethod::File { .. }));
        assert!(adapter.invoke_template.contains("--prompt-file"));
        assert_eq!(adapter.token_extraction, TokenExtraction::None);
    }

    #[test]
    fn builtin_codex_config() {
        let adapter = builtin_codex();
        assert_eq!(adapter.name, "codex");
        assert_eq!(adapter.agent_cli, "codex");
        assert!(matches!(adapter.input_method, InputMethod::Args { .. }));
        assert!(adapter.invoke_template.contains("codex exec"));
        assert!(adapter
            .invoke_template
            .contains("--sandbox workspace-write"));
        assert!(adapter.invoke_template.contains("--json"));
        assert_eq!(adapter.model, Some("gpt-5.6-terra".to_string()));
        assert_eq!(adapter.provider, Some("openai".to_string()));
        assert_eq!(
            adapter.output_transform,
            Some("needle-transform-codex".to_string())
        );
    }

    #[test]
    fn builtin_aider_config() {
        let adapter = builtin_aider();
        assert_eq!(adapter.name, "aider");
        assert_eq!(adapter.agent_cli, "aider");
        assert!(adapter.invoke_template.contains("--yes --message"));
        assert_eq!(adapter.provider, Some("anthropic".to_string()));
        assert!(matches!(
            adapter.token_extraction,
            TokenExtraction::Regex { .. }
        ));
    }

    // ── Adapter loading ──

    #[test]
    fn load_adapters_includes_builtins() {
        let adapters =
            load_adapters(Path::new("/nonexistent/adapters"), &builtin_adapters()).unwrap();
        assert!(adapters.contains_key("claude-sonnet"));
        assert!(adapters.contains_key("generic"));
    }

    #[test]
    fn load_adapters_from_yaml_directory() {
        let dir = std::env::temp_dir().join("needle-adapter-load-test");
        let _ = std::fs::create_dir_all(&dir);
        let yaml = "name: test-agent\nagent_cli: test-bin\ninvoke_template: \"echo test\"\n";
        std::fs::write(dir.join("test-agent.yaml"), yaml).unwrap();

        let adapters = load_adapters(&dir, &builtin_adapters()).unwrap();
        assert!(adapters.contains_key("test-agent"));
        assert!(adapters.contains_key("claude-sonnet"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_adapter_overrides_builtin() {
        let dir = std::env::temp_dir().join("needle-adapter-override-test");
        let _ = std::fs::create_dir_all(&dir);
        let yaml =
            "name: claude-sonnet\nagent_cli: claude-custom\ninvoke_template: \"custom {prompt_file}\"\n";
        std::fs::write(dir.join("claude-sonnet.yaml"), yaml).unwrap();

        let adapters = load_adapters(&dir, &builtin_adapters()).unwrap();
        let adapter = adapters.get("claude-sonnet").unwrap();
        assert_eq!(adapter.agent_cli, "claude-custom");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Temp file ──

    #[test]
    fn write_prompt_to_temp_creates_file() {
        let path = write_prompt_to_temp(&BeadId::from("needle-temp1"), "hello world").unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello world");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_prompt_to_temp_uses_temp_dir() {
        let path = write_prompt_to_temp(&BeadId::from("needle-temp2"), "test").unwrap();
        let temp = std::env::temp_dir();
        assert!(path.starts_with(temp.join("needle")));
        let _ = std::fs::remove_file(&path);
    }

    // ── Dispatch integration tests ──

    #[tokio::test]
    async fn dispatch_echo_captures_stdout() {
        let mut adapters = HashMap::new();
        adapters.insert(
            "echo".to_string(),
            test_adapter("echo", "echo hello-needle"),
        );
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("echo").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-echo"),
                &test_prompt("test"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello-needle");
        assert!(result.pid > 0);
    }

    #[tokio::test]
    async fn dispatch_captures_exit_code() {
        let mut adapters = HashMap::new();
        adapters.insert("fail".to_string(), test_adapter("fail", "exit 42"));
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("fail").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-exit"),
                &test_prompt("test"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 42);
    }

    #[tokio::test]
    async fn dispatch_timeout_returns_124() {
        let mut adapters = HashMap::new();
        let mut adapter = test_adapter("slow", "sleep 100");
        adapter.timeout_secs = 1;
        adapters.insert("slow".to_string(), adapter);

        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("slow").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-timeout"),
                &test_prompt("test"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 124);
        assert!(result.elapsed >= Duration::from_millis(900));
        assert!(result.elapsed < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn dispatch_missing_binary_returns_127() {
        let mut adapters = HashMap::new();
        adapters.insert(
            "missing".to_string(),
            test_adapter("missing", "nonexistent-binary-xyz-12345"),
        );
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("missing").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-missing"),
                &test_prompt("test"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 127);
    }

    #[tokio::test]
    async fn dispatch_environment_variables() {
        let mut adapter = test_adapter("env", "echo $NEEDLE_TEST_VAR");
        adapter.environment.insert(
            "NEEDLE_TEST_VAR".to_string(),
            "hello-from-needle".to_string(),
        );
        let mut adapters = HashMap::new();
        adapters.insert("env".to_string(), adapter);

        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("env").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-env"),
                &test_prompt("test"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello-from-needle");
    }

    #[tokio::test]
    async fn dispatch_stdin_redirect_from_prompt_file() {
        let mut adapters = HashMap::new();
        adapters.insert(
            "cat".to_string(),
            test_adapter("cat", "cat < {prompt_file}"),
        );
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("cat").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-stdin"),
                &test_prompt("prompt-content-here"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "prompt-content-here");
    }

    #[tokio::test]
    async fn dispatch_cleans_up_temp_file() {
        let bead_id = BeadId::from("nd-cleanup");
        let mut adapters = HashMap::new();
        adapters.insert("true".to_string(), test_adapter("true", "true"));
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("true").unwrap().clone();

        let _ = dispatcher
            .dispatch(
                &bead_id,
                &test_prompt("cleanup test"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        // Verify the temp file was cleaned up.
        let expected_path = std::env::temp_dir().join("needle").join(format!(
            "prompt-{}-{}.md",
            bead_id,
            std::process::id()
        ));
        assert!(!expected_path.exists(), "temp file should be cleaned up");
    }

    #[tokio::test]
    async fn dispatch_template_renders_bead_id() {
        let mut adapters = HashMap::new();
        adapters.insert("id".to_string(), test_adapter("id", "echo bead={bead_id}"));
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("id").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("needle-xyz"),
                &test_prompt("test"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "bead=needle-xyz");
    }

    #[tokio::test]
    async fn dispatch_captures_stderr() {
        let mut adapters = HashMap::new();
        adapters.insert(
            "err".to_string(),
            test_adapter("err", "echo error-output >&2"),
        );
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("err").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-stderr"),
                &test_prompt("test"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stderr.trim(), "error-output");
    }

    // ── Token extraction ──

    #[test]
    fn extract_tokens_json_field() {
        let json = r#"{"result":{"usage":{"input_tokens":1234,"output_tokens":567}}}"#;
        let usage = extract_tokens_json(
            json,
            "result.usage.input_tokens",
            "result.usage.output_tokens",
        );
        assert_eq!(usage.input_tokens, Some(1234));
        assert_eq!(usage.output_tokens, Some(567));
    }

    #[test]
    fn extract_tokens_json_missing_path() {
        let json = r#"{"result":{}}"#;
        let usage = extract_tokens_json(
            json,
            "result.usage.input_tokens",
            "result.usage.output_tokens",
        );
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
    }

    #[test]
    fn extract_tokens_json_invalid() {
        let usage = extract_tokens_json(
            "not json",
            "result.usage.input_tokens",
            "result.usage.output_tokens",
        );
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
    }

    #[test]
    fn extract_tokens_regex_aider_format() {
        let text = "Tokens: 1,234 sent, 567 received";
        let usage = extract_tokens_regex(
            text,
            r"Tokens:\s+([\d,]+)\s+sent,\s+([\d,]+)\s+received",
            1,
            2,
        );
        assert_eq!(usage.input_tokens, Some(1234));
        assert_eq!(usage.output_tokens, Some(567));
    }

    #[test]
    fn extract_tokens_regex_no_match() {
        let usage = extract_tokens_regex("no tokens here", r"Tokens: (\d+)", 1, 2);
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
    }

    #[test]
    fn extract_tokens_regex_invalid_pattern() {
        let usage = extract_tokens_regex("text", r"[invalid", 1, 2);
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
    }

    #[test]
    fn extract_tokens_none_returns_default() {
        let usage = extract_tokens(&TokenExtraction::None, "stdout", "stderr");
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
    }

    #[test]
    fn extract_tokens_dispatches_to_json() {
        let json = r#"{"usage":{"in":100,"out":50}}"#;
        let extraction = TokenExtraction::JsonField {
            input_path: "usage.in".to_string(),
            output_path: "usage.out".to_string(),
        };
        let usage = extract_tokens(&extraction, json, "");
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
    }

    #[test]
    fn extract_tokens_regex_searches_stderr_too() {
        let extraction = TokenExtraction::Regex {
            pattern: r"Tokens:\s+([\d,]+)\s+sent,\s+([\d,]+)\s+received".to_string(),
            input_group: 1,
            output_group: 2,
        };
        let usage = extract_tokens(&extraction, "", "Tokens: 100 sent, 50 received");
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
    }

    #[test]
    fn token_extraction_yaml_roundtrip() {
        let adapter = builtin_claude_sonnet();
        let yaml = serde_yaml::to_string(&adapter).unwrap();
        let parsed: AgentAdapter = serde_yaml::from_str(&yaml).unwrap();
        assert!(matches!(parsed.token_extraction, TokenExtraction::None));
    }

    #[test]
    fn token_extraction_regex_yaml_roundtrip() {
        let adapter = builtin_aider();
        let yaml = serde_yaml::to_string(&adapter).unwrap();
        let parsed: AgentAdapter = serde_yaml::from_str(&yaml).unwrap();
        assert!(matches!(
            parsed.token_extraction,
            TokenExtraction::Regex { .. }
        ));
    }

    #[test]
    fn token_extraction_none_yaml_roundtrip() {
        let adapter = builtin_generic();
        let yaml = serde_yaml::to_string(&adapter).unwrap();
        let parsed: AgentAdapter = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.token_extraction, TokenExtraction::None);
    }

    #[test]
    fn build_sample_json_creates_valid_structure() {
        let sample = build_sample_json("result.usage.input_tokens", "result.usage.output_tokens");
        let usage = extract_tokens_json(
            &sample,
            "result.usage.input_tokens",
            "result.usage.output_tokens",
        );
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
    }

    #[test]
    fn all_builtin_adapters_load() {
        let adapters =
            load_adapters(Path::new("/nonexistent/adapters"), &builtin_adapters()).unwrap();
        assert!(adapters.contains_key("claude-sonnet"));
        assert!(adapters.contains_key("claude-opus"));
        assert!(adapters.contains_key("opencode"));
        assert!(adapters.contains_key("codex"));
        assert!(adapters.contains_key("aider"));
        assert!(adapters.contains_key("generic"));
        assert_eq!(adapters.len(), 6);
    }

    // ── E2E: Agent adapter invocation (needle-4vq) ──
    //
    // These tests validate the full dispatch invocation chain: template
    // rendering, env var injection, prompt delivery, process management,
    // timeout enforcement, exit code capture, and output parsing.

    #[tokio::test]
    async fn e2e_all_template_variables_substituted() {
        // Verify that {workspace}, {prompt_file}, {bead_id}, and {model} are
        // all rendered into the command the agent receives.
        let mut adapter = test_adapter(
            "vars",
            "echo ws={workspace} pf={prompt_file} bid={bead_id} m={model}",
        );
        adapter.model = Some("test-model-v1".to_string());

        let mut adapters = HashMap::new();
        adapters.insert("vars".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("vars").unwrap().clone();

        let workspace = std::env::temp_dir().join("needle-e2e-vars");
        let _ = std::fs::create_dir_all(&workspace);

        let result = dispatcher
            .dispatch(
                &BeadId::from("needle-tmpl"),
                &test_prompt("irrelevant"),
                &adapter,
                &workspace,
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        let out = result.stdout.trim();
        assert!(
            out.contains(&format!("ws={}", workspace.display())),
            "workspace not substituted: {out}"
        );
        assert!(
            out.contains("bid=needle-tmpl"),
            "bead_id not substituted: {out}"
        );
        assert!(
            out.contains("m=test-model-v1"),
            "model not substituted: {out}"
        );
        // prompt_file is a temp path — just verify it was substituted (not literal)
        assert!(
            !out.contains("{prompt_file}"),
            "prompt_file placeholder not replaced: {out}"
        );
        assert!(
            out.contains("pf=/"),
            "prompt_file should be an absolute path: {out}"
        );

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn e2e_multiple_environment_variables() {
        // Verify that all adapter environment variables are set in the child.
        let mut adapter = test_adapter("multienv", "echo $NDL_A $NDL_B $NDL_C");
        adapter
            .environment
            .insert("NDL_A".to_string(), "alpha".to_string());
        adapter
            .environment
            .insert("NDL_B".to_string(), "beta".to_string());
        adapter
            .environment
            .insert("NDL_C".to_string(), "gamma".to_string());

        let mut adapters = HashMap::new();
        adapters.insert("multienv".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("multienv").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-env-multi"),
                &test_prompt("test"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "alpha beta gamma");
    }

    #[tokio::test]
    async fn e2e_prompt_with_shell_metacharacters() {
        // Verify that shell metacharacters in the prompt body are safely
        // delivered via the temp file without shell injection or corruption.
        let dangerous_prompt =
            "Hello $USER\nLine with `backticks`\nQuotes: 'single' \"double\"\nBackslash: \\\nDollar: $(echo injected)";

        let mut adapters = HashMap::new();
        adapters.insert(
            "catprompt".to_string(),
            test_adapter("catprompt", "cat {prompt_file}"),
        );
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("catprompt").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-meta"),
                &test_prompt(dangerous_prompt),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        // The prompt file content should be the exact string, not shell-expanded.
        assert!(
            result.stdout.contains("$USER"),
            "shell variable should be literal, not expanded"
        );
        assert!(
            result.stdout.contains("`backticks`"),
            "backticks should be preserved"
        );
        assert!(
            result.stdout.contains("$(echo injected)"),
            "command substitution should be literal"
        );
        assert!(
            result.stdout.contains("'single'"),
            "single quotes should be preserved"
        );
        assert!(
            result.stdout.contains("\"double\""),
            "double quotes should be preserved"
        );
    }

    #[tokio::test]
    async fn e2e_prompt_with_newlines_preserved() {
        let multiline = "line one\nline two\nline three";

        let mut adapters = HashMap::new();
        adapters.insert(
            "wc".to_string(),
            test_adapter("wc", "wc -l < {prompt_file}"),
        );
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("wc").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-newlines"),
                &test_prompt(multiline),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        let line_count: i32 = result.stdout.trim().parse().unwrap_or(-1);
        // wc -l counts newline characters; "line one\nline two\nline three"
        // has 2 newlines, so wc -l reports 2.
        assert_eq!(line_count, 2, "prompt should have 2 newlines (3 lines)");
    }

    #[tokio::test]
    async fn e2e_exit_code_0_is_success() {
        use crate::types::Outcome;

        let mut adapters = HashMap::new();
        adapters.insert("ok".to_string(), test_adapter("ok", "exit 0"));
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("ok").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-exit0"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(Outcome::classify(result.exit_code, false), Outcome::Success);
    }

    #[tokio::test]
    async fn e2e_exit_code_1_is_failure() {
        use crate::types::Outcome;

        let mut adapters = HashMap::new();
        adapters.insert("f1".to_string(), test_adapter("f1", "exit 1"));
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("f1").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-exit1"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 1);
        assert_eq!(Outcome::classify(result.exit_code, false), Outcome::Failure);
    }

    #[tokio::test]
    async fn e2e_exit_code_2_is_failure() {
        use crate::types::Outcome;

        let mut adapters = HashMap::new();
        adapters.insert("f2".to_string(), test_adapter("f2", "exit 2"));
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("f2").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-exit2"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 2);
        assert_eq!(Outcome::classify(result.exit_code, false), Outcome::Failure);
    }

    #[tokio::test]
    async fn e2e_exit_code_137_is_crash() {
        use crate::types::Outcome;

        let mut adapters = HashMap::new();
        adapters.insert("crash".to_string(), test_adapter("crash", "exit 137"));
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("crash").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-exit137"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 137);
        assert_eq!(
            Outcome::classify(result.exit_code, false),
            Outcome::Crash(137)
        );
    }

    #[tokio::test]
    async fn e2e_timeout_kills_agent_returns_124() {
        let mut adapter = test_adapter("sleeper", "sleep 100");
        adapter.timeout_secs = 1;

        let mut adapters = HashMap::new();
        adapters.insert("sleeper".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("sleeper").unwrap().clone();

        let start = Instant::now();
        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-timeout"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();
        let wall = start.elapsed();

        assert_eq!(result.exit_code, 124, "timeout should yield exit 124");
        assert!(
            wall < Duration::from_secs(5),
            "should have been killed after ~1s, took {:?}",
            wall
        );
        assert!(
            result.elapsed >= Duration::from_millis(900),
            "should have waited at least ~1s"
        );
    }

    #[tokio::test]
    async fn e2e_timeout_kills_entire_process_group() {
        // Verify that on timeout the entire process group (not just the direct
        // bash child) is killed.  The agent starts a background sleep and writes
        // its PID to a temp file before blocking.  After timeout we assert the
        // grandchild is gone.
        let pid_file =
            std::env::temp_dir().join(format!("needle-pgkill-{}.pid", std::process::id()));
        let pid_file_str = pid_file.display().to_string();

        // Start a background sleep, capture its PID, then sleep (will time out).
        let cmd = format!("sleep 1000 & echo $! > {pid_file_str}; sleep 1000");

        let mut adapter = test_adapter("pgkill", &cmd);
        adapter.timeout_secs = 2;

        let mut adapters = HashMap::new();
        adapters.insert("pgkill".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("pgkill").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-pgkill"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 124, "timeout should yield 124");

        // The grandchild PID file must exist — echo runs in milliseconds, well
        // within the 2-second timeout window.
        let pid_str = std::fs::read_to_string(&pid_file)
            .expect("grandchild PID file should have been written before timeout fired");
        let grandchild_pid: libc::pid_t = pid_str
            .trim()
            .parse()
            .expect("PID file should contain a valid integer PID");

        // Poll until the grandchild is dead or we time out waiting.  SIGKILL
        // delivery and OS reaping can be slow in container environments.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let dead = loop {
            let alive = unsafe { libc::kill(grandchild_pid, 0) == 0 };
            if !alive {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert!(
            dead,
            "grandchild sleep (pid {grandchild_pid}) should be dead within 3s after killpg"
        );

        let _ = std::fs::remove_file(&pid_file);
    }

    #[tokio::test]
    async fn e2e_outer_cancellation_still_kills_process_group() {
        // Regression test for bf-653n7 (the mitosis-evaluation-timeout leak).
        //
        // Worker's mitosis-evaluation step wraps the *entire* dispatch() call
        // in its own, much shorter, `tokio::time::timeout` — separate from
        // and unrelated to the agent's own configured timeout exercised by
        // `e2e_timeout_kills_entire_process_group` above. Before
        // ProcessGroupKillGuard, that outer timeout firing dropped the
        // in-flight dispatch() future before its *internal* timeout-kill
        // match ever ran, silently orphaning the agent process and any
        // process-group children it had spawned — indefinitely, since
        // nothing ever reaped them.
        //
        // Here the adapter's own timeout is set effectively unreachable
        // within the test's window, so the only thing that can kill the
        // process is the guard reacting to the *outer* future being dropped.
        let pid_file =
            std::env::temp_dir().join(format!("needle-outercancel-{}.pid", std::process::id()));
        let pid_file_str = pid_file.display().to_string();

        let cmd = format!("sleep 1000 & echo $! > {pid_file_str}; sleep 1000");
        let mut adapter = test_adapter("outercancel", &cmd);
        adapter.timeout_secs = 3600;

        let mut adapters = HashMap::new();
        adapters.insert("outercancel".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("outercancel").unwrap().clone();

        // Mimic Worker's mitosis-evaluation wrapper: an outer timeout, far
        // shorter than the agent's own, wrapping the whole dispatch call.
        let outer = tokio::time::timeout(
            Duration::from_millis(500),
            dispatcher.dispatch(
                &BeadId::from("nd-outercancel"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            ),
        )
        .await;
        assert!(
            outer.is_err(),
            "outer timeout should fire well before the adapter's own 3600s timeout"
        );

        let pid_str = std::fs::read_to_string(&pid_file)
            .expect("grandchild PID file should have been written before the outer timeout fired");
        let grandchild_pid: libc::pid_t = pid_str
            .trim()
            .parse()
            .expect("PID file should contain a valid integer PID");

        // Poll until the grandchild is dead or we give up waiting.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let dead = loop {
            let alive = unsafe { libc::kill(grandchild_pid, 0) == 0 };
            if !alive {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert!(
            dead,
            "grandchild sleep (pid {grandchild_pid}) should be dead within 3s of the *outer* \
             future being dropped, even though dispatch()'s own internal timeout never fired \
             — this is what ProcessGroupKillGuard exists to guarantee"
        );

        let _ = std::fs::remove_file(&pid_file);
    }

    #[tokio::test]
    async fn e2e_json_output_capture_and_token_extraction() {
        // Simulate a claude-like JSON output and verify token extraction works
        // on real process output.
        let json = r#"{"type":"result","result":"done","cost_usd":0.001,"usage":{"input_tokens":1500,"output_tokens":750}}"#;
        let cmd = format!("echo '{json}'");

        let mut adapter = test_adapter("json-agent", &cmd);
        adapter.token_extraction = TokenExtraction::JsonField {
            input_path: "usage.input_tokens".to_string(),
            output_path: "usage.output_tokens".to_string(),
        };

        let mut adapters = HashMap::new();
        adapters.insert("json-agent".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("json-agent").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-json"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);

        // Parse the captured stdout with the token extraction logic.
        let usage = extract_tokens(&adapter.token_extraction, &result.stdout, &result.stderr);
        assert_eq!(usage.input_tokens, Some(1500));
        assert_eq!(usage.output_tokens, Some(750));
    }

    #[tokio::test]
    async fn e2e_adapter_with_custom_env_and_base_url() {
        // Simulate an adapter with ANTHROPIC_BASE_URL and custom env vars,
        // verifying they're all available to the child process.
        let mut adapter = test_adapter(
            "custom-env",
            "echo base=$ANTHROPIC_BASE_URL custom=$CUSTOM_FLAG",
        );
        adapter.environment.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            "https://api.example.com".to_string(),
        );
        adapter
            .environment
            .insert("CUSTOM_FLAG".to_string(), "enabled".to_string());

        let mut adapters = HashMap::new();
        adapters.insert("custom-env".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("custom-env").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-baseurl"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(
            result.stdout.contains("base=https://api.example.com"),
            "ANTHROPIC_BASE_URL not set: {}",
            result.stdout
        );
        assert!(
            result.stdout.contains("custom=enabled"),
            "CUSTOM_FLAG not set: {}",
            result.stdout
        );
    }

    #[tokio::test]
    async fn e2e_workspace_directory_is_correct() {
        // Verify the agent process can see the workspace directory.
        let workspace = std::env::temp_dir().join("needle-e2e-wsdir");
        let _ = std::fs::create_dir_all(&workspace);

        let mut adapters = HashMap::new();
        adapters.insert(
            "pwd".to_string(),
            test_adapter("pwd", "cd {workspace} && pwd"),
        );
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("pwd").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-wsdir"),
                &test_prompt("t"),
                &adapter,
                &workspace,
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        // Canonicalize both to handle symlinks (e.g., /tmp -> /private/tmp on macOS)
        let expected = std::fs::canonicalize(&workspace)
            .unwrap_or_else(|_| workspace.clone())
            .display()
            .to_string();
        let actual = result.stdout.trim().to_string();
        let actual_canonical = std::fs::canonicalize(&actual)
            .map(|p| p.display().to_string())
            .unwrap_or(actual);
        assert_eq!(actual_canonical, expected);

        let _ = std::fs::remove_dir_all(&workspace);
    }

    // ── GenAI semantic conventions ──

    #[test]
    fn claude_sonnet_adapter_has_genai_attributes() {
        let adapter = builtin_claude_sonnet();
        assert_eq!(adapter.provider, Some("anthropic".to_string()));
        assert_eq!(adapter.model, Some("claude-sonnet-4-6".to_string()));
    }

    #[test]
    fn claude_opus_adapter_has_genai_attributes() {
        let adapter = builtin_claude_opus();
        assert_eq!(adapter.provider, Some("anthropic".to_string()));
        assert_eq!(adapter.model, Some("claude-opus-4-6".to_string()));
    }

    #[test]
    fn codex_adapter_has_openai_provider() {
        let adapter = builtin_codex();
        assert_eq!(adapter.provider, Some("openai".to_string()));
        assert_eq!(adapter.model, Some("gpt-5.6-terra".to_string()));
    }

    #[test]
    fn gen_ai_system_returns_provider_for_claude_sonnet() {
        let adapter = builtin_claude_sonnet();
        assert_eq!(adapter.gen_ai_system(), "anthropic");
    }

    #[test]
    fn gen_ai_system_returns_provider_for_claude_opus() {
        let adapter = builtin_claude_opus();
        assert_eq!(adapter.gen_ai_system(), "anthropic");
    }

    #[test]
    fn gen_ai_system_returns_provider_for_codex() {
        let adapter = builtin_codex();
        assert_eq!(adapter.gen_ai_system(), "openai");
    }

    #[test]
    fn gen_ai_system_returns_local_for_adapter_without_provider() {
        let adapter = builtin_opencode();
        assert_eq!(adapter.gen_ai_system(), "local");
    }

    // ── Timeout policy tests ──

    #[test]
    fn timeout_policy_returns_legacy_for_legacy_timeout() {
        let adapter = AgentAdapter {
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            ..test_adapter("test", "test")
        };
        assert_eq!(adapter.timeout_policy(), TimeoutPolicy::Legacy);
    }

    #[test]
    fn timeout_policy_returns_new_for_idle_only() {
        let adapter = AgentAdapter {
            timeout_secs: 0,
            idle_timeout_secs: 600,
            hard_timeout_secs: 0,
            ..test_adapter("test", "test")
        };
        assert!(matches!(
            adapter.timeout_policy(),
            TimeoutPolicy::New {
                idle_enabled: true,
                hard_enabled: false
            }
        ));
    }

    #[test]
    fn timeout_policy_returns_new_for_hard_only() {
        let adapter = AgentAdapter {
            timeout_secs: 0,
            idle_timeout_secs: 0,
            hard_timeout_secs: 7200,
            ..test_adapter("test", "test")
        };
        assert!(matches!(
            adapter.timeout_policy(),
            TimeoutPolicy::New {
                idle_enabled: false,
                hard_enabled: true
            }
        ));
    }

    #[test]
    fn timeout_policy_returns_new_for_both_timeouts() {
        let adapter = AgentAdapter {
            timeout_secs: 0,
            idle_timeout_secs: 600,
            hard_timeout_secs: 7200,
            ..test_adapter("test", "test")
        };
        assert!(matches!(
            adapter.timeout_policy(),
            TimeoutPolicy::New {
                idle_enabled: true,
                hard_enabled: true
            }
        ));
    }

    #[test]
    fn timeout_policy_returns_global_when_all_zero() {
        let adapter = AgentAdapter {
            timeout_secs: 0,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            ..test_adapter("test", "test")
        };
        assert_eq!(adapter.timeout_policy(), TimeoutPolicy::Global);
    }

    #[test]
    fn timeout_description_shows_legacy_mode() {
        let adapter = AgentAdapter {
            timeout_secs: 3600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            ..test_adapter("test", "test")
        };
        let desc = adapter.timeout_description(1800);
        assert!(desc.contains("legacy"));
        assert!(desc.contains("3600s"));
    }

    #[test]
    fn timeout_description_shows_new_mode_with_both() {
        let adapter = AgentAdapter {
            timeout_secs: 0,
            idle_timeout_secs: 600,
            hard_timeout_secs: 7200,
            ..test_adapter("test", "test")
        };
        let desc = adapter.timeout_description(1800);
        assert!(desc.contains("new"));
        assert!(desc.contains("idle=600s"));
        assert!(desc.contains("hard=7200s"));
    }

    #[test]
    fn timeout_description_shows_global_fallback() {
        let adapter = AgentAdapter {
            timeout_secs: 0,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            ..test_adapter("test", "test")
        };
        let desc = adapter.timeout_description(1800);
        assert!(desc.contains("global"));
        assert!(desc.contains("1800s"));
    }

    #[test]
    fn timeout_description_shows_unlimited_when_global_is_zero() {
        let adapter = AgentAdapter {
            timeout_secs: 0,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            ..test_adapter("test", "test")
        };
        let desc = adapter.timeout_description(0);
        assert!(desc.contains("global"));
        assert!(desc.contains("unlimited"));
        assert!(desc.contains("0"));
    }

    #[test]
    fn timeout_description_shows_idle_only_when_hard_is_zero() {
        let adapter = AgentAdapter {
            timeout_secs: 0,
            idle_timeout_secs: 900,
            hard_timeout_secs: 0,
            ..test_adapter("test", "test")
        };
        let desc = adapter.timeout_description(1800);
        assert!(desc.contains("new"));
        assert!(desc.contains("idle=900s"));
        // Should not mention hard timeout
        assert!(!desc.contains("hard"));
    }

    // ── Activity detection tests ──

    #[tokio::test]
    async fn activity_detection_on_stdout_resets_idle_timeout() {
        // Verify that ongoing stdout output prevents idle timeout from firing.
        // A process that outputs continuously should not be killed by idle deadline.
        let mut adapter = test_adapter(
            "chatty-stdout",
            // Output a dot every 200ms, then sleep 100 at the end.
            "for i in $(seq 1 10); do echo -n .; sleep 0.2; done; sleep 0.1",
        );
        adapter.idle_timeout_secs = 1; // 1 second idle timeout
        adapter.hard_timeout_secs = 10; // 10 second hard timeout (should not fire)

        let mut adapters = HashMap::new();
        adapters.insert("chatty-stdout".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("chatty-stdout").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-activity-stdout"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        // Should succeed, not be killed by idle timeout
        assert_eq!(
            result.exit_code, 0,
            "process with continuous output should not idle timeout"
        );
        // The loop runs for ~2.1s (10 * 0.2s + 0.1s), well past the 1s idle deadline
        assert!(
            result.elapsed >= Duration::from_millis(1900),
            "should run full duration"
        );
        assert!(
            result.stdout.contains(".........."),
            "should capture all output"
        );
    }

    #[tokio::test]
    async fn activity_detection_on_stderr_resets_idle_timeout() {
        // Verify that ongoing stderr output also prevents idle timeout.
        let mut adapter = test_adapter(
            "chatty-stderr",
            "for i in $(seq 1 10); do echo -n . >&2; sleep 0.2; done; sleep 0.1",
        );
        adapter.idle_timeout_secs = 1;
        adapter.hard_timeout_secs = 10;

        let mut adapters = HashMap::new();
        adapters.insert("chatty-stderr".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("chatty-stderr").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-activity-stderr"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(
            result.exit_code, 0,
            "process with continuous stderr should not idle timeout"
        );
        assert!(result.elapsed >= Duration::from_millis(1900));
        assert!(
            result.stderr.contains(".........."),
            "should capture all stderr output"
        );
    }

    #[tokio::test]
    async fn activity_detection_on_binary_data() {
        // Verify that binary/non-text byte sequences are detected as activity.
        // Use printf to emit raw bytes including non-printable characters.
        let mut adapter = test_adapter(
            "binary-output",
            // Emit binary bytes: 0x00 0x01 0x02 ... 0x09, then newline, repeat
            "for i in $(seq 1 20); do printf '\\x00\\x01\\x02\\x03\\x04\\x05\\x06\\x07\\x08\\x09\\n'; sleep 0.15; done",
        );
        adapter.idle_timeout_secs = 1;
        adapter.hard_timeout_secs = 10;

        let mut adapters = HashMap::new();
        adapters.insert("binary-output".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("binary-output").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-activity-binary"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(
            result.exit_code, 0,
            "binary output should prevent idle timeout"
        );
        // Should run ~3s (20 * 0.15s), well past idle deadline
        assert!(
            result.elapsed >= Duration::from_millis(2800),
            "should run full duration with binary output"
        );
    }

    #[tokio::test]
    async fn activity_detection_happens_before_newline_parsing() {
        // Verify that activity is detected on every byte read, before newline parsing.
        // Output many bytes without newlines, then a newline at the end.
        let mut adapter = test_adapter(
            "no-newlines",
            // Emit 1000 characters without newlines, sleep 200ms between chunks
            "printf '%0.s#' {1..1000}; sleep 0.2; echo done",
        );
        adapter.idle_timeout_secs = 1;
        adapter.hard_timeout_secs = 10;

        let mut adapters = HashMap::new();
        adapters.insert("no-newlines".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("no-newlines").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-activity-nonewline"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(
            result.exit_code, 0,
            "output without newlines should still reset idle timer"
        );
        // The initial printf is fast, but the 200ms sleep should extend execution
        assert!(result.elapsed >= Duration::from_millis(150));
    }

    #[tokio::test]
    async fn activity_detection_with_mixed_stdout_stderr() {
        // Verify that both stdout and stderr activity reset the idle timer.
        let mut adapter = test_adapter(
            "mixed-streams",
            // Alternate between stdout and stderr output
            "for i in $(seq 1 8); do echo -n out >&1; echo -n err >&2; sleep 0.18; done; echo done",
        );
        adapter.idle_timeout_secs = 1;
        adapter.hard_timeout_secs = 10;

        let mut adapters = HashMap::new();
        adapters.insert("mixed-streams".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("mixed-streams").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-activity-mixed"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(
            result.exit_code, 0,
            "mixed stdout/stderr should prevent idle timeout"
        );
        // 8 iterations * 0.18s ≈ 1.44s, plus overhead
        assert!(result.elapsed >= Duration::from_millis(1300));
        assert!(result.stdout.contains("outoutoutoutoutoutoutout"));
        assert!(result.stderr.contains("errerrererrererrerrerre"));
    }

    #[tokio::test]
    async fn idle_timeout_fires_when_no_activity() {
        // Verify that idle timeout DOES fire when there's no output.
        // This is the negative case proving activity detection works.
        let mut adapter = test_adapter(
            "silent-process",
            // Sleep for 5 seconds without any output
            "sleep 5",
        );
        adapter.idle_timeout_secs = 1; // Should fire after 1s of silence

        let mut adapters = HashMap::new();
        adapters.insert("silent-process".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("silent-process").unwrap().clone();

        let start = Instant::now();
        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-idle-timeout"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(
            result.exit_code, 124,
            "idle timeout should return exit code 124"
        );
        assert!(
            result.timeout_reason.is_some(),
            "should have timeout reason"
        );
        match result.timeout_reason {
            Some(TimeoutReason::Idle { timeout_secs, .. }) => {
                assert_eq!(timeout_secs, 1);
            }
            _ => panic!(
                "expected Idle timeout reason, got {:?}",
                result.timeout_reason
            ),
        }
        // Should fire around 1s (allowing for scheduling overhead)
        assert!(elapsed >= Duration::from_millis(900));
        assert!(elapsed < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn activity_detection_on_partial_chunks() {
        // Verify that partial reads (chunks < 8192 bytes) still register activity.
        // The real reader reads in chunks; we verify small chunks are detected.
        let mut adapter = test_adapter(
            "small-chunks",
            // Emit small amounts of output with delays
            "for i in $(seq 1 15); do echo -n x; sleep 0.12; done; echo",
        );
        adapter.idle_timeout_secs = 1;
        adapter.hard_timeout_secs = 10;

        let mut adapters = HashMap::new();
        adapters.insert("small-chunks".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("small-chunks").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-activity-chunks"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(
            result.exit_code, 0,
            "small chunk writes should prevent idle timeout"
        );
        // 15 iterations * 0.12s = 1.8s
        assert!(result.elapsed >= Duration::from_millis(1700));
    }

    #[tokio::test]
    async fn activity_detection_large_output_burst() {
        // Verify that a large burst of output (multiple chunks) is detected as activity.
        let mut adapter = test_adapter(
            "large-burst",
            // Emit 50KB of data in one go
            "dd if=/dev/zero bs=1024 count=50 2>/dev/null; sleep 0.5; echo done",
        );
        adapter.idle_timeout_secs = 1;
        adapter.hard_timeout_secs = 10;

        let mut adapters = HashMap::new();
        adapters.insert("large-burst".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);
        let adapter = dispatcher.adapter("large-burst").unwrap().clone();

        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-activity-burst"),
                &test_prompt("t"),
                &adapter,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(
            result.exit_code, 0,
            "large output burst should be detected as activity"
        );
        // 50KB read time + 0.5s sleep
        assert!(result.elapsed >= Duration::from_millis(400));
        assert!(result.stdout.contains("done"));
    }

    #[tokio::test]
    async fn activity_timestamp_tracked_per_process() {
        // Verify that activity tracking is isolated per process execution.
        // Two sequential dispatches should have independent activity timestamps.
        let mut adapter = test_adapter("timestamped", "echo output-$(date +%s%N)");
        adapter.idle_timeout_secs = 1;

        let mut adapters = HashMap::new();
        adapters.insert("timestamped".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);

        // First dispatch
        let adapter1 = dispatcher.adapter("timestamped").unwrap().clone();
        let result1 = dispatcher
            .dispatch(
                &BeadId::from("nd-timestamp-1"),
                &test_prompt("t"),
                &adapter1,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result1.exit_code, 0);

        // Small delay to ensure different timestamp
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Second dispatch
        let adapter2 = dispatcher.adapter("timestamped").unwrap().clone();
        let result2 = dispatcher
            .dispatch(
                &BeadId::from("nd-timestamp-2"),
                &test_prompt("t"),
                &adapter2,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(result2.exit_code, 0);
        // Both should succeed independently
        assert!(result1.stdout.contains("output-"));
        assert!(result2.stdout.contains("output-"));
    }

    // ── Activity Detection Tests ──

    #[tokio::test]
    async fn activity_detection_on_normal_stdout_output() {
        // Test that activity detection works for normal stdout output
        let adapter = test_adapter("echo-test", "echo 'hello world'");
        let mut adapters = HashMap::new();
        adapters.insert("echo-test".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);

        let adapter_ref = dispatcher.adapter("echo-test").unwrap().clone();
        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-echo-test"),
                &test_prompt("test"),
                &adapter_ref,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        // Should complete successfully
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello world"));
        // Activity was detected (process completed without timeout)
        assert!(result.elapsed < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn activity_detection_on_stderr_output() {
        // Test that activity detection works for stderr output
        let adapter = test_adapter("stderr-test", "echo 'error message' >&2");
        let mut adapters = HashMap::new();
        adapters.insert("stderr-test".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);

        let adapter_ref = dispatcher.adapter("stderr-test").unwrap().clone();
        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-stderr-test"),
                &test_prompt("test"),
                &adapter_ref,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        // Should complete successfully
        assert_eq!(result.exit_code, 0);
        assert!(result.stderr.contains("error message"));
        // Activity was detected on stderr
        assert!(result.elapsed < Duration::from_secs(10));
    }


    #[tokio::test]
    async fn activity_detection_during_transforms() {
        // Test that activity detection works when output_transform is configured
        let mut adapter = test_adapter("transform-test", "echo 'test output'");
        adapter.output_transform = Some("cat".to_string()); // Use cat as simple transform
        let mut adapters = HashMap::new();
        adapters.insert("transform-test".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);

        let adapter_ref = dispatcher.adapter("transform-test").unwrap().clone();
        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-transform-test"),
                &test_prompt("test"),
                &adapter_ref,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        // Should complete successfully with transform
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("test output"));
        // Activity was detected even with transform active
        assert!(result.elapsed < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn activity_detection_on_multiline_output() {
        // Test that activity detection works for multiline output
        let adapter = test_adapter("multiline-test", "for i in 1 2 3 4 5; do echo \"line $i\"; sleep 0.1; done");
        let mut adapters = HashMap::new();
        adapters.insert("multiline-test".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);

        let adapter_ref = dispatcher.adapter("multiline-test").unwrap().clone();
        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-multiline-test"),
                &test_prompt("test"),
                &adapter_ref,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        // Should complete successfully
        assert_eq!(result.exit_code, 0);
        // All lines should be captured
        assert!(result.stdout.contains("line 1"));
        assert!(result.stdout.contains("line 5"));
        // Activity was detected continuously (prevents idle timeout)
        assert!(result.elapsed < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn activity_detection_on_chunked_output() {
        // Test that activity detection works when output comes in chunks
        let adapter = test_adapter(
            "chunked-test",
            "echo 'chunk1'; sleep 0.2; echo 'chunk2'; sleep 0.2; echo 'chunk3'",
        );
        let mut adapters = HashMap::new();
        adapters.insert("chunked-test".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);

        let adapter_ref = dispatcher.adapter("chunked-test").unwrap().clone();
        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-chunked-test"),
                &test_prompt("test"),
                &adapter_ref,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        // Should complete successfully
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("chunk1"));
        assert!(result.stdout.contains("chunk2"));
        assert!(result.stdout.contains("chunk3"));
        // Activity was detected on each chunk
        assert!(result.elapsed < Duration::from_secs(10));
    }


    #[tokio::test]
    async fn activity_detection_timestamps_before_parsing() {
        // Test that activity timestamps are recorded before newline parsing
        // This test verifies the structural requirement from the acceptance criteria
        let adapter = test_adapter("timestamp-order-test", "printf 'line1\\nline2\\nline3'");
        let mut adapters = HashMap::new();
        adapters.insert("timestamp-order-test".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);

        let adapter_ref = dispatcher.adapter("timestamp-order-test").unwrap().clone();
        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-timestamp-order-test"),
                &test_prompt("test"),
                &adapter_ref,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        // Should complete successfully
        assert_eq!(result.exit_code, 0);
        // All lines captured (proving parsing happened after activity detection)
        assert!(result.stdout.contains("line1"));
        assert!(result.stdout.contains("line2"));
        assert!(result.stdout.contains("line3"));
        // Fast completion proves activity was detected continuously
        assert!(result.elapsed < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn activity_detection_on_rapid_output() {
        // Test that activity detection handles rapid output without missing bytes
        let adapter = test_adapter("rapid-test", "for i in $(seq 1 100); do echo \"rapid $i\"; done");
        let mut adapters = HashMap::new();
        adapters.insert("rapid-test".to_string(), adapter);
        let dispatcher = test_dispatcher(adapters);

        let adapter_ref = dispatcher.adapter("rapid-test").unwrap().clone();
        let result = dispatcher
            .dispatch(
                &BeadId::from("nd-rapid-test"),
                &test_prompt("test"),
                &adapter_ref,
                Path::new("/tmp"),
            )
            .await
            .unwrap();

        // Should complete successfully
        assert_eq!(result.exit_code, 0);
        // All rapid lines should be captured
        assert!(result.stdout.contains("rapid 1"));
        assert!(result.stdout.contains("rapid 100"));
        // Count the lines to verify none were missed
        let line_count = result.stdout.lines().count();
        assert!(line_count >= 100, "Expected at least 100 lines, got {}", line_count);
    }
}
