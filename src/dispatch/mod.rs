//! Agent dispatch: load adapters, render templates, execute agent processes.
//!
//! The dispatcher is agent-agnostic. Adding a new agent requires only a YAML
//! adapter file. It renders an invoke template, starts a process, waits (with
//! timeout enforcement), and captures the raw result.
//!
//! Depends on: `types`, `config`, `telemetry`, `prompt`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::config::Config;
use crate::prompt::BuiltPrompt;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{BeadId, InputMethod};

// ──────────────────────────────────────────────────────────────────────────────
// ExecutionResult
// ──────────────────────────────────────────────────────────────────────────────

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
    /// AI provider name (informational).
    #[serde(default)]
    pub provider: Option<String>,
    /// Model identifier (substituted as `{model}` in the template).
    #[serde(default)]
    pub model: Option<String>,
    /// How to extract token usage from agent output.
    #[serde(default)]
    pub token_extraction: TokenExtraction,
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
            "cd {workspace} && claude --print --model claude-sonnet-4-6",
            " --max-turns 30 --output-format json --verbose < {prompt_file}",
        )
        .to_string(),
        environment: HashMap::new(),
        timeout_secs: 3600,
        provider: Some("anthropic".to_string()),
        model: Some("claude-sonnet-4-6".to_string()),
        token_extraction: TokenExtraction::JsonField {
            input_path: "result.usage.input_tokens".to_string(),
            output_path: "result.usage.output_tokens".to_string(),
        },
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
            "cd {workspace} && claude --print --model claude-opus-4-6",
            " --max-turns 50 --output-format json --verbose < {prompt_file}",
        )
        .to_string(),
        environment: HashMap::new(),
        timeout_secs: 7200,
        provider: Some("anthropic".to_string()),
        model: Some("claude-opus-4-6".to_string()),
        token_extraction: TokenExtraction::JsonField {
            input_path: "result.usage.input_tokens".to_string(),
            output_path: "result.usage.output_tokens".to_string(),
        },
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
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
    }
}

/// Codex CLI built-in adapter.
fn builtin_codex() -> AgentAdapter {
    AgentAdapter {
        name: "codex".to_string(),
        description: Some("OpenAI Codex CLI with full-auto approval".to_string()),
        agent_cli: "codex".to_string(),
        version_command: Some("codex --version".to_string()),
        input_method: InputMethod::Args {
            flag: "--".to_string(),
        },
        invoke_template: concat!(
            "cd {workspace} && codex --model {model}",
            " --approval-mode full-auto \"$(cat {prompt_file})\"",
        )
        .to_string(),
        environment: HashMap::new(),
        timeout_secs: 3600,
        provider: Some("openai".to_string()),
        model: Some("gpt-4".to_string()),
        token_extraction: TokenExtraction::None,
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
        provider: Some("anthropic".to_string()),
        model: Some("claude-sonnet-4-6".to_string()),
        token_extraction: TokenExtraction::Regex {
            pattern: r"Tokens:\s+([\d,]+)\s+sent,\s+([\d,]+)\s+received".to_string(),
            input_group: 1,
            output_group: 2,
        },
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
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
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
                adapters.insert(adapter.name.clone(), adapter);
            }
        }
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
}

impl Dispatcher {
    /// Create a new dispatcher, loading adapters from config.
    pub fn new(config: &Config, telemetry: Telemetry) -> Result<Self> {
        let adapters = load_adapters(&config.agent.adapters_dir, &builtin_adapters())?;
        Ok(Dispatcher {
            adapters,
            telemetry,
            global_timeout_secs: config.agent.timeout,
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

    /// Execute the agent process for a bead.
    ///
    /// 1. Writes the prompt to a temp file
    /// 2. Renders the invoke template with variables
    /// 3. Sets adapter-specific environment variables
    /// 4. Spawns the process via `bash -c`
    /// 5. Waits with timeout enforcement (kills on timeout, exit 124)
    /// 6. Captures stdout, stderr, exit code
    /// 7. Cleans up the temp file
    pub async fn dispatch(
        &self,
        bead_id: &BeadId,
        prompt: &BuiltPrompt,
        adapter: &AgentAdapter,
        workspace: &Path,
    ) -> Result<ExecutionResult> {
        self.telemetry.emit(EventKind::DispatchStarted {
            bead_id: bead_id.clone(),
            agent: adapter.name.clone(),
            prompt_len: prompt.content.len(),
        })?;

        let result = self
            .execute_agent(bead_id, &prompt.content, adapter, workspace)
            .await;

        // Emit completion telemetry regardless of success/failure.
        match &result {
            Ok(exec) => {
                let _ = self.telemetry.emit(EventKind::DispatchCompleted {
                    bead_id: bead_id.clone(),
                    exit_code: exec.exit_code,
                    duration_ms: exec.elapsed.as_millis() as u64,
                });
            }
            Err(_) => {
                let _ = self.telemetry.emit(EventKind::DispatchCompleted {
                    bead_id: bead_id.clone(),
                    exit_code: -1,
                    duration_ms: 0,
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
    async fn run_process(
        &self,
        bead_id: &BeadId,
        adapter: &AgentAdapter,
        workspace: &Path,
        prompt_file: &Path,
    ) -> Result<ExecutionResult> {
        let model = adapter.model.as_deref().unwrap_or("default");
        let rendered = render_template(
            &adapter.invoke_template,
            workspace,
            prompt_file,
            bead_id,
            model,
        );

        let mut child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(&rendered)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .envs(&adapter.environment)
            .spawn()
            .with_context(|| format!("failed to spawn agent: {}", adapter.name))?;

        let pid = child.id().unwrap_or(0);
        let start = Instant::now();

        // Read stdout/stderr concurrently to avoid pipe buffer deadlock.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let stdout_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stdout_pipe {
                let _ = AsyncReadExt::read_to_end(&mut pipe, &mut buf).await;
            }
            String::from_utf8_lossy(&buf).into_owned()
        });

        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stderr_pipe {
                let _ = AsyncReadExt::read_to_end(&mut pipe, &mut buf).await;
            }
            String::from_utf8_lossy(&buf).into_owned()
        });

        // Wait for exit with optional timeout enforcement.
        let timeout_dur = adapter.effective_timeout(self.global_timeout_secs);

        let exit_code = if timeout_dur.is_zero() {
            let status = child
                .wait()
                .await
                .context("failed to wait for agent process")?;
            status.code().unwrap_or(-1)
        } else {
            match tokio::time::timeout(timeout_dur, child.wait()).await {
                Ok(Ok(status)) => status.code().unwrap_or(-1),
                Ok(Err(e)) => return Err(e).context("failed to wait for agent process"),
                Err(_) => {
                    // Timeout: kill the process and reap it.
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    124
                }
            }
        };

        let elapsed = start.elapsed();
        let stdout = stdout_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();

        Ok(ExecutionResult {
            exit_code,
            stdout,
            stderr,
            elapsed,
            pid,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

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

    // 6. Determine overall status.
    let status = if cli_path.is_none() {
        AgentTestStatus::Error
    } else if !errors.is_empty() {
        AgentTestStatus::Warning
    } else {
        AgentTestStatus::Ready
    };

    Ok(AgentTestResult {
        adapter_name: adapter.name.clone(),
        cli_path,
        version,
        input_method,
        probe_result,
        token_extraction_ok,
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
    let output = ProcessCommand::new(agent_cli)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("failed to probe {agent_cli}"))?;

    Ok(ProbeResult {
        exit_code: output.code().unwrap_or(-1),
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
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
    match &result.probe_result {
        Some(pr) => println!("Probe:   exit {} ({}ms)", pr.exit_code, pr.elapsed_ms),
        None => println!("Probe:   skipped"),
    }
    match result.token_extraction_ok {
        Some(true) => println!("Tokens:  extraction working"),
        Some(false) => println!("Tokens:  extraction FAILED"),
        None => println!("Tokens:  none configured"),
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
            provider: None,
            model: None,
            token_extraction: TokenExtraction::None,
        }
    }

    fn test_prompt(content: &str) -> BuiltPrompt {
        BuiltPrompt {
            content: content.to_string(),
            hash: "testhash".to_string(),
            token_estimate: content.len() as u64 / 4,
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
        assert!(matches!(
            adapter.token_extraction,
            TokenExtraction::JsonField { .. }
        ));
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
        assert!(adapter
            .invoke_template
            .contains("--approval-mode full-auto"));
        assert_eq!(adapter.model, Some("gpt-4".to_string()));
        assert_eq!(adapter.provider, Some("openai".to_string()));
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
        assert!(matches!(
            parsed.token_extraction,
            TokenExtraction::JsonField { .. }
        ));
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
}
