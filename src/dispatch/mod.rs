//! Agent dispatch: load adapters, render templates, execute agent processes.
//!
//! The dispatcher is agent-agnostic. Adding a new agent requires only a YAML
//! adapter file. It renders an invoke template, starts a process, waits (with
//! timeout enforcement), and captures the raw result.
//!
//! Depends on: `types`, `config`, `telemetry`, `prompt`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
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
    }
}

/// Returns all built-in adapters.
pub fn builtin_adapters() -> Vec<AgentAdapter> {
    vec![builtin_claude_sonnet(), builtin_generic()]
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
        assert!(adapters.iter().any(|a| a.name == "generic"));
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
}
