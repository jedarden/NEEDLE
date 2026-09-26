//! Fake-CLI coverage for the documented agent adapter support matrix.
//!
//! The README's "Supported Agents" table promises OpenCode, Codex, and Aider
//! (plus a generic YAML template) alongside the Claude adapters. The built-in
//! registry ships all of them, but until now only their static configuration
//! was asserted on — no test ever executed their invoke templates. These
//! tests run each shipped built-in adapter definition through the real
//! [`Dispatcher::dispatch`] spawn path against a fake agent CLI, checking:
//!
//! 1. **Invocation** — the rendered template hands the prompt to the CLI the
//!    way the adapter's input method promises (stdin, argv, or a prompt file),
//!    from the bead's workspace, with `{model}` rendered.
//! 2. **Exit status** — a zero exit is reported as success with captured
//!    stdout; a non-zero exit is reported verbatim.
//! 3. **Failures** — a CLI named by the template but missing from PATH
//!    surfaces as exit 127 with "command not found" on stderr.
//! 4. **Routing** — model→adapter routing rules resolve the README's
//!    non-Claude model families (qwen, gpt, sonnet) to the OpenCode, Codex,
//!    and Aider adapters, the routed adapter dispatches its own CLI with the
//!    prompt delivered by its input method, and unmatched models fall back
//!    to the configured default adapter.
//!
//! The fake CLIs resolve through `PATH`, so tests that prepend a fake bin
//! directory serialize on a shared mutex: concurrent `set_var` from sibling
//! tests in this binary would lose one another's directory.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tempfile::TempDir;
use tokio::sync::Mutex;

use needle::adapter_usage::stream_usage;
use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::claim::{ClaimIdentity, ResolvedStoreContext};
use needle::config::{AgentConfig, Config, RoutingConfig, RoutingRule};
use needle::dispatch::{
    builtin_adapters, AgentAdapter, DispatchContext, Dispatcher, ExecutionResult, TokenExtraction,
    UsageFormat,
};
use needle::prompt::BuiltPrompt;
use needle::telemetry::Telemetry;
use needle::types::{Bead, BeadId, BeadStatus, ClaimResult, ClaimStatus, InputMethod};

/// The worker identity every matrix dispatch claims (and verifies) under.
const MATRIX_WORKER: &str = "matrix-test";
/// Claim revision and epoch the matrix store reports for every bead.
const MATRIX_REVISION: u64 = 11;
const MATRIX_EPOCH: u64 = 4;

/// Serializes PATH mutation among the tests in this module.
static FAKE_PATH_LOCK: Mutex<()> = Mutex::const_new(());

/// A bead store that answers every claim with this module's worker, so the
/// dispatcher's final pre-spawn gate verifies and the spawn proceeds.
///
/// The pre-spawn gate only accepts a carried [`DispatchContext`], so the
/// identity here and in [`matrix_context`] must agree exactly.
struct MatrixClaimStore;

#[async_trait]
impl BeadStore for MatrixClaimStore {
    async fn ready(&self, _filters: &Filters) -> anyhow::Result<Vec<Bead>> {
        Ok(vec![])
    }

    async fn list_all(&self) -> anyhow::Result<Vec<Bead>> {
        Ok(vec![])
    }

    async fn show(&self, _id: &BeadId) -> anyhow::Result<Bead> {
        anyhow::bail!("MatrixClaimStore serves claim_status only")
    }

    async fn claim_status(&self, _id: &BeadId) -> anyhow::Result<ClaimStatus> {
        Ok(ClaimStatus {
            status: BeadStatus::InProgress,
            assignee: Some(MATRIX_WORKER.to_string()),
            revision: Some(MATRIX_REVISION),
            claim_epoch: Some(MATRIX_EPOCH),
        })
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> anyhow::Result<ClaimResult> {
        anyhow::bail!("MatrixClaimStore serves claim_status only")
    }

    async fn claim_auto(&self, _actor: &str) -> anyhow::Result<ClaimResult> {
        anyhow::bail!("MatrixClaimStore serves claim_status only")
    }

    async fn release(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn block(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn create_bead(
        &self,
        _title: &str,
        _body: &str,
        _labels: &[&str],
    ) -> anyhow::Result<BeadId> {
        anyhow::bail!("MatrixClaimStore serves claim_status only")
    }

    async fn add_dependency(
        &self,
        _blocker_id: &BeadId,
        _blocked_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_dependency(
        &self,
        _blocked_id: &BeadId,
        _blocker_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> anyhow::Result<RepairReport> {
        Ok(RepairReport {
            warnings: Vec::new(),
            fixed: Vec::new(),
        })
    }

    async fn doctor_check(&self) -> anyhow::Result<RepairReport> {
        Ok(RepairReport {
            warnings: Vec::new(),
            fixed: Vec::new(),
        })
    }

    async fn full_rebuild(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

/// The claim identity and carried store the pre-spawn gate verifies against.
fn matrix_context(workspace: &Path) -> DispatchContext {
    let store: Arc<dyn BeadStore> = Arc::new(MatrixClaimStore);
    DispatchContext::new(
        ResolvedStoreContext::new(store, workspace.to_path_buf()),
        ClaimIdentity {
            actor: MATRIX_WORKER.to_string(),
            revision: Some(MATRIX_REVISION),
            claim_epoch: Some(MATRIX_EPOCH),
        },
    )
}

/// Marker env var carrying the fake-CLI invocation log path. Injected through
/// the adapter's own `environment` map, which the dispatcher passes to the
/// child process — the templates themselves are never modified.
const LOG_ENV: &str = "NEEDLE_FAKE_AGENT_LOG";

/// Env var a fake CLI reads to decide its exit code (failure-path control).
const EXIT_ENV: &str = "NEEDLE_FAKE_AGENT_EXIT";

/// One fake bin directory plus the invocation log its CLIs append to.
struct FakeCli {
    _bin_dir: TempDir,
    log: PathBuf,
}

impl FakeCli {
    /// Create the bin dir and write a fake script for each agent name.
    ///
    /// Each script appends one record per invocation — `cwd=`, then one
    /// `arg=` line per argv entry, then `file_content=` for any
    /// `--prompt-file <path>` pair — prints a marker line, and exits with
    /// [`EXIT_ENV`] (default 0).
    fn new() -> Self {
        let bin_dir = tempfile::tempdir().expect("create fake bin dir");
        let log = bin_dir.path().join("invocations.log");
        // Shared prologue: record cwd and every argv entry.
        const RECORD_ARGS: &str = "printf 'cwd=%s\\n' \"$PWD\" >> \"$NEEDLE_FAKE_AGENT_LOG\"\n\
             for a in \"$@\"; do printf 'arg=%s\\n' \"$a\" >> \"$NEEDLE_FAKE_AGENT_LOG\"; done\n\
             prev=''\n\
             for a in \"$@\"; do\n\
             \x20 if [ \"$prev\" = '--prompt-file' ]; then\n\
             \x20   printf 'file_content=%s\\n' \"$(cat \"$a\")\" >> \"$NEEDLE_FAKE_AGENT_LOG\"\n\
             \x20 fi\n\
             \x20 prev=\"$a\"\n\
             done\n";
        let agents = [
            (
                "claude",
                "fake-claude-ok",
                concat!(
                    "printf 'cwd=%s\\n' \"$PWD\" >> \"$NEEDLE_FAKE_AGENT_LOG\"\n",
                    "for a in \"$@\"; do printf 'arg=%s\\n' \"$a\" >> \"$NEEDLE_FAKE_AGENT_LOG\"; done\n",
                    "stdin_content=$(cat)\n",
                    "printf 'stdin=%s\\n' \"$stdin_content\" >> \"$NEEDLE_FAKE_AGENT_LOG\"\n",
                    "case \"${NEEDLE_FAKE_AGENT_MODE:-success}\" in\n",
                    "  api-error) printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"api_error_status\":503,\"terminal_reason\":\"api_error\",\"result\":\"API Error: 503\"}' ;;\n",
                    "  success) printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"terminal_reason\":\"end_turn\",\"result\":\"done\"}' ;;\n",
                    "  timeout) sleep 10 ;;\n",
                    "esac\n"
                ),
                "",
            ),
            (
                "claude-print",
                "fake-claude-print-ok",
                concat!(
                    "printf 'cwd=%s\\n' \"$PWD\" >> \"$NEEDLE_FAKE_AGENT_LOG\"\n",
                    "for a in \"$@\"; do printf 'arg=%s\\n' \"$a\" >> \"$NEEDLE_FAKE_AGENT_LOG\"; done\n",
                    "stdin_content=$(cat)\n",
                    "printf 'stdin=%s\\n' \"$stdin_content\" >> \"$NEEDLE_FAKE_AGENT_LOG\"\n",
                    "case \"${NEEDLE_FAKE_AGENT_MODE:-success}\" in\n",
                    "  api-error) printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"api_error_status\":503,\"terminal_reason\":\"api_error\",\"result\":\"API Error: 503\"}' ;;\n",
                    "  success) printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"terminal_reason\":\"end_turn\",\"result\":\"done\"}' ;;\n",
                    "  timeout) sleep 10 ;;\n",
                    "esac\n"
                ),
                "",
            ),
            (
                "opencode",
                "fake-opencode-ok",
                "",
                concat!(
                    "printf 'cwd=%s\n' \"$PWD\" >> \"$NEEDLE_FAKE_AGENT_LOG\"\n",
                    "for a in \"$@\"; do printf 'arg=%s\n' \"$a\" >> \"$NEEDLE_FAKE_AGENT_LOG\"; done\n",
                    "stdin_content=$(cat)\n",
                    "printf 'stdin=%s\n' \"$stdin_content\" >> \"$NEEDLE_FAKE_AGENT_LOG\"\n",
                ),
            ),
            ("codex", "fake-codex-ok", "", RECORD_ARGS),
            (
                "aider",
                "fake-aider-ok",
                // The token summary the shipped usage parser consumes.
                "echo 'Tokens: 12.5k sent, 8.1k cache write, 2.1k cache hit, 4.3k received.'",
                RECORD_ARGS,
            ),
            (
                "my-agent",
                "fake-generic-ok",
                "",
                concat!(
                    "printf 'cwd=%s\\n' \"$PWD\" >> \"$NEEDLE_FAKE_AGENT_LOG\"\n",
                    // The generic template (and the Stdin input method) hand
                    // the prompt to the CLI on standard input.
                    "stdin_content=$(cat)\n",
                    "printf 'stdin=%s\\n' \"$stdin_content\" >> \"$NEEDLE_FAKE_AGENT_LOG\"\n",
                ),
            ),
        ];
        for (name, marker, output, record) in agents {
            let script = format!(
                "#!/usr/bin/env bash\n{record}{output}\nif [ \"${{NEEDLE_FAKE_AGENT_MODE:-success}}\" = timeout ]; then sleep 10; fi\necho '{marker}'\nexit \"${{NEEDLE_FAKE_AGENT_EXIT:-0}}\"\n"
            );
            let path = bin_dir.path().join(name);
            fs::write(&path, script).expect("write fake CLI script");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .expect("make fake CLI executable");
        }
        FakeCli {
            _bin_dir: bin_dir,
            log,
        }
    }
}

/// The documented Claude print adapter is normally installed as a user YAML
/// file. Keep this test fixture portable: its command shape is the documented
/// contract, while the executable resolves through the fake PATH below rather
/// than a host-specific absolute path.
fn claude_print_adapter() -> AgentAdapter {
    AgentAdapter {
        name: "claude-print".to_string(),
        description: Some("Claude Code print mode".to_string()),
        agent_cli: "claude-print".to_string(),
        version_command: Some("claude-print --version".to_string()),
        input_method: InputMethod::Stdin,
        invoke_template: concat!(
            "cd {workspace} && claude-print --model {model}",
            " --max-turns 30 --output-format stream-json",
            " --dangerously-skip-permissions < {prompt_file}",
        )
        .to_string(),
        environment: HashMap::new(),
        timeout_secs: 3600,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: Some("anthropic".to_string()),
        model: Some("claude-sonnet-5".to_string()),
        token_extraction: TokenExtraction::None,
        usage_format: Some(UsageFormat::ClaudeStreamJson),
        output_transform: None,
        harness: Some("needle".to_string()),
        harness_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

fn documented_adapter(name: &str) -> AgentAdapter {
    if name == "claude-print" {
        return claude_print_adapter();
    }

    let mut adapter = builtin_adapters()
        .into_iter()
        .find(|adapter| adapter.name == name)
        .unwrap_or_else(|| panic!("documented adapter {name} missing"));
    // The contract tests below assert the dispatcher and adapter command
    // boundary, not the separate transform binary. Keeping the child raw also
    // makes the matrix independent of which Cargo targets the test command
    // happened to build.
    adapter.output_transform = None;
    adapter
}

/// Prepend `dir` to `PATH`, restoring the previous value on drop.
struct FakePathGuard {
    previous: Option<std::ffi::OsString>,
}

impl FakePathGuard {
    fn prepend(dir: &Path) -> Self {
        let previous = std::env::var_os("PATH");
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(
            previous.as_deref().unwrap_or_default(),
        ));
        std::env::set_var("PATH", std::env::join_paths(&paths).expect("join PATH"));
        FakePathGuard { previous }
    }
}

impl Drop for FakePathGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
    }
}

fn test_prompt(bead_id: &str) -> BuiltPrompt {
    BuiltPrompt {
        content: format!("matrix smoke prompt for {bead_id}"),
        hash: "matrix-hash".to_string(),
        token_estimate: 42,
        template_name: "test".to_string(),
        template_version: "1.0".to_string(),
    }
}

/// Dispatch the shipped built-in adapter `name` against the fake CLIs.
///
/// Clones the built-in definition untouched except for the fake-CLI log path
/// (and any extra environment), so the template under test is exactly the one
/// shipped in [`builtin_adapters`].
async fn dispatch_builtin(
    name: &str,
    fake: &FakeCli,
    extra_env: &[(&str, String)],
    bead_id: &str,
    workspace: &Path,
) -> anyhow::Result<ExecutionResult> {
    let adapter: AgentAdapter = builtin_adapters()
        .into_iter()
        .find(|a| a.name == name)
        .unwrap_or_else(|| panic!("builtin adapter {name} missing from the registry"))
        .clone();
    dispatch_adapter(adapter, name, fake, extra_env, bead_id, workspace).await
}

/// Dispatch a fixture adapter through the same production spawn path.
async fn dispatch_adapter(
    mut adapter: AgentAdapter,
    name: &str,
    fake: &FakeCli,
    extra_env: &[(&str, String)],
    bead_id: &str,
    workspace: &Path,
) -> anyhow::Result<ExecutionResult> {
    adapter
        .environment
        .insert(LOG_ENV.to_string(), fake.log.display().to_string());
    for (key, value) in extra_env {
        adapter.environment.insert(key.to_string(), value.clone());
    }

    let mut adapters = HashMap::new();
    adapters.insert(name.to_string(), adapter);

    let dispatcher =
        Dispatcher::with_adapters(adapters, Telemetry::new(MATRIX_WORKER.to_string()), 3600)
            .with_worker_id(MATRIX_WORKER.to_string());

    let adapter_ref = dispatcher.adapter(name).expect("adapter wired");
    dispatcher
        .dispatch_with_context(
            &BeadId::from(bead_id),
            &test_prompt(bead_id),
            adapter_ref,
            workspace,
            &matrix_context(workspace),
        )
        .await
}

/// Read trace metadata emitted by the dispatcher for a workspace-backed store.
fn trace_metadata(workspace: &Path, bead_id: &str) -> serde_json::Value {
    let path = workspace
        .join(".beads")
        .join("traces")
        .join(bead_id)
        .join("metadata.json");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("trace metadata missing at {}: {e}", path.display()));
    serde_json::from_str(&text).expect("trace metadata must be valid JSON")
}

fn trace_workspace() -> TempDir {
    let workspace = unique_workspace();
    fs::create_dir(workspace.path().join(".beads")).expect("create trace store marker");
    workspace
}

/// Read the fake CLI's invocation log as flat `key=value` lines.
fn read_log(fake: &FakeCli) -> Vec<String> {
    fs::read_to_string(&fake.log)
        .unwrap_or_else(|e| panic!("fake CLI never ran (no log at {}): {e}", fake.log.display()))
        .lines()
        .map(str::to_owned)
        .collect()
}

fn log_values<'a>(lines: &'a [String], key: &str) -> Vec<&'a str> {
    let prefix = format!("{key}=");
    lines
        .iter()
        .filter_map(|l| l.strip_prefix(prefix.as_str()))
        .collect()
}

/// The argv entry immediately following `flag` in the logged invocation.
fn value_after_flag<'a>(args: &'a [&'a str], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| *a == flag)
        .and_then(|i| args.get(i + 1).copied())
}

fn unique_workspace() -> TempDir {
    tempfile::tempdir().expect("create workspace dir")
}

// ──────────────────────────────────────────────────────────────────────────────
// Invocation: each documented input method delivers the prompt as promised
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn opencode_builtin_receives_prompt_via_stdin() {
    let _path_lock = FAKE_PATH_LOCK.lock().await;
    let fake = FakeCli::new();
    let _path = FakePathGuard::prepend(fake._bin_dir.path());
    let workspace = unique_workspace();

    let result = dispatch_builtin(
        "opencode",
        &fake,
        &[],
        "needle-matrix-opencode",
        workspace.path(),
    )
    .await
    .expect("opencode dispatch should succeed");

    assert_eq!(result.exit_code, 0, "fake opencode exits 0");
    assert!(result.timeout_reason.is_none());
    assert!(
        result.stdout.contains("fake-opencode-ok"),
        "stdout captured"
    );

    let lines = read_log(&fake);
    assert_eq!(
        log_values(&lines, "cwd").first().copied(),
        Some(workspace.path().to_str().expect("utf-8 workspace")),
        "template must run the CLI from the bead workspace"
    );

    let args = log_values(&lines, "arg");
    assert_eq!(
        args.first().copied(),
        Some("run"),
        "opencode run subcommand"
    );
    assert!(args.contains(&"--format"), "JSON output format flag");
    assert!(args.contains(&"json"), "JSON output format");
    assert!(args.contains(&"--auto"), "headless auto-approve flag");
    assert!(
        !args.contains(&"--prompt-file"),
        "OpenCode has no prompt-file flag"
    );
    assert!(
        !args.contains(&"--non-interactive"),
        "OpenCode has no non-interactive flag"
    );
    let logged_content = log_values(&lines, "stdin");
    assert_eq!(
        logged_content.first().copied(),
        Some("matrix smoke prompt for needle-matrix-opencode"),
        "stdin must carry the built prompt"
    );
    assert!(
        !args.iter().any(|arg| arg.contains("matrix smoke prompt")),
        "stdin input must not leak the prompt through argv"
    );
}

#[tokio::test]
async fn claude_code_builtin_receives_prompt_via_stdin() {
    let _path_lock = FAKE_PATH_LOCK.lock().await;
    let fake = FakeCli::new();
    let _path = FakePathGuard::prepend(fake._bin_dir.path());
    let workspace = unique_workspace();

    // The built-in Claude adapter is exercised with its output transform
    // disabled here. This contract is about the shipped invoke template and
    // prompt transport; the transform has its own schema conformance tests.
    let mut adapter = builtin_adapters()
        .into_iter()
        .find(|a| a.name == "claude")
        .expect("claude builtin");
    adapter.output_transform = None;
    let result = dispatch_adapter(
        adapter,
        "claude",
        &fake,
        &[],
        "needle-matrix-claude",
        workspace.path(),
    )
    .await
    .expect("Claude dispatch should succeed");

    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("\"type\":\"result\""));

    let lines = read_log(&fake);
    assert_eq!(
        log_values(&lines, "cwd").first().copied(),
        Some(workspace.path().to_str().expect("utf-8 workspace"))
    );
    let args = log_values(&lines, "arg");
    for expected in [
        "-p",
        "--model",
        "claude-sonnet-4-6",
        "--output-format",
        "stream-json",
        "--dangerously-skip-permissions",
        "--verbose",
    ] {
        assert!(
            args.contains(&expected),
            "Claude template must render {expected}"
        );
    }
    assert_eq!(
        log_values(&lines, "stdin").first().copied(),
        Some("matrix smoke prompt for needle-matrix-claude")
    );
    assert!(
        !args.iter().any(|arg| arg.contains("matrix smoke prompt")),
        "stdin input must not leak the prompt through argv"
    );
}

#[tokio::test]
async fn claude_print_contract_receives_prompt_via_stdin() {
    let _path_lock = FAKE_PATH_LOCK.lock().await;
    let fake = FakeCli::new();
    let _path = FakePathGuard::prepend(fake._bin_dir.path());
    let workspace = unique_workspace();

    let adapter = claude_print_adapter();
    assert_eq!(adapter.agent_cli, "claude-print");
    assert!(matches!(adapter.input_method, InputMethod::Stdin));
    assert!(adapter.invoke_template.contains("claude-print --model"));
    assert!(adapter
        .invoke_template
        .contains("--output-format stream-json"));

    let result = dispatch_adapter(
        adapter,
        "claude-print",
        &fake,
        &[],
        "needle-matrix-claude-print",
        workspace.path(),
    )
    .await
    .expect("Claude print dispatch should succeed");

    assert_eq!(result.exit_code, 0);
    let lines = read_log(&fake);
    assert_eq!(
        log_values(&lines, "cwd").first().copied(),
        Some(workspace.path().to_str().expect("utf-8 workspace"))
    );
    let args = log_values(&lines, "arg");
    for expected in [
        "--model",
        "claude-sonnet-5",
        "--max-turns",
        "30",
        "--output-format",
        "stream-json",
        "--dangerously-skip-permissions",
    ] {
        assert!(
            args.contains(&expected),
            "Claude print template must render {expected}"
        );
    }
    assert_eq!(
        log_values(&lines, "stdin").first().copied(),
        Some("matrix smoke prompt for needle-matrix-claude-print")
    );
}

#[tokio::test]
async fn codex_builtin_receives_prompt_as_argument() {
    let _path_lock = FAKE_PATH_LOCK.lock().await;
    let fake = FakeCli::new();
    let _path = FakePathGuard::prepend(fake._bin_dir.path());
    let workspace = unique_workspace();

    let result = dispatch_builtin("codex", &fake, &[], "needle-matrix-codex", workspace.path())
        .await
        .expect("codex dispatch should succeed");

    assert_eq!(result.exit_code, 0, "fake codex exits 0");
    assert!(result.stdout.contains("fake-codex-ok"), "stdout captured");

    let lines = read_log(&fake);
    assert_eq!(
        log_values(&lines, "cwd").first().copied(),
        Some(workspace.path().to_str().expect("utf-8 workspace"))
    );

    let args = log_values(&lines, "arg");
    assert_eq!(args.first().copied(), Some("exec"), "codex exec subcommand");
    for expected in [
        "--model",
        "gpt-5.6-terra",
        "--sandbox",
        "workspace-write",
        "--json",
    ] {
        assert!(
            args.contains(&expected),
            "codex template must render {expected}"
        );
    }
    assert_eq!(
        args.last().copied(),
        Some("matrix smoke prompt for needle-matrix-codex"),
        "prompt must arrive as the final argv entry via $(cat prompt_file)"
    );
    assert!(
        !args.iter().any(|a| a.ends_with(".md")),
        "args input method must not leak the prompt file path"
    );
}

#[tokio::test]
async fn aider_builtin_receives_prompt_via_message_flag() {
    let _path_lock = FAKE_PATH_LOCK.lock().await;
    let fake = FakeCli::new();
    let _path = FakePathGuard::prepend(fake._bin_dir.path());
    let workspace = unique_workspace();

    let result = dispatch_builtin("aider", &fake, &[], "needle-matrix-aider", workspace.path())
        .await
        .expect("aider dispatch should succeed");

    assert_eq!(result.exit_code, 0, "fake aider exits 0");
    assert!(result.stdout.contains("fake-aider-ok"), "stdout captured");

    let lines = read_log(&fake);
    assert_eq!(
        log_values(&lines, "cwd").first().copied(),
        Some(workspace.path().to_str().expect("utf-8 workspace"))
    );

    let args = log_values(&lines, "arg");
    for expected in ["--model", "claude-sonnet-4-6", "--yes-always", "--message"] {
        assert!(
            args.contains(&expected),
            "aider template must render {expected}"
        );
    }
    assert_eq!(
        value_after_flag(&args, "--message"),
        Some("matrix smoke prompt for needle-matrix-aider"),
        "--message must carry the rendered prompt"
    );

    // The shipped summary parser must parse the token line the real aider
    // prints (and this fake reproduces) from the captured stdout.
    let adapter = builtin_adapters()
        .into_iter()
        .find(|a| a.name == "aider")
        .expect("aider builtin");
    assert_eq!(adapter.usage_format, Some(UsageFormat::AiderSummary));
    let usage = stream_usage(adapter.effective_usage_format(), &result.stdout)
        .expect("aider summary should carry usage");
    assert_eq!(usage.input, 12_500);
    assert_eq!(usage.output, 4_300);
    assert_eq!(usage.cache_write, 8_100);
    assert_eq!(usage.cache_read, 2_100);
}

#[tokio::test]
async fn generic_builtin_delivers_prompt_via_stdin() {
    let _path_lock = FAKE_PATH_LOCK.lock().await;
    let fake = FakeCli::new();
    let _path = FakePathGuard::prepend(fake._bin_dir.path());
    let workspace = unique_workspace();

    // The generic template pipes {prompt_file} into the CLI's stdin.
    let result = dispatch_builtin(
        "generic",
        &fake,
        &[],
        "needle-matrix-generic",
        workspace.path(),
    )
    .await
    .expect("generic dispatch should succeed");

    assert_eq!(result.exit_code, 0, "fake generic agent exits 0");
    assert!(result.stdout.contains("fake-generic-ok"), "stdout captured");

    let lines = read_log(&fake);
    assert_eq!(
        log_values(&lines, "stdin").first().copied(),
        Some("matrix smoke prompt for needle-matrix-generic"),
        "the prompt must arrive on the CLI's standard input"
    );
    let args = log_values(&lines, "arg");
    assert!(
        !args.iter().any(|a| a.contains("matrix smoke prompt")),
        "stdin adapters must not pass the prompt through argv"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Exit status and failures
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn builtin_adapter_nonzero_exit_is_reported_verbatim() {
    let _path_lock = FAKE_PATH_LOCK.lock().await;
    let fake = FakeCli::new();
    let _path = FakePathGuard::prepend(fake._bin_dir.path());

    for name in ["claude", "claude-print", "opencode", "codex", "aider"] {
        let workspace = unique_workspace();
        let result = dispatch_adapter(
            documented_adapter(name),
            name,
            &fake,
            &[(EXIT_ENV, "3".to_string())],
            &format!("needle-matrix-{name}-fail"),
            workspace.path(),
        )
        .await
        .expect("dispatch itself completes when an agent fails");

        assert_eq!(result.exit_code, 3, "{name} exit code must pass through");
        assert!(
            result.timeout_reason.is_none(),
            "a plain {name} failure is not a timeout"
        );
    }
}

#[tokio::test]
async fn documented_adapters_enforce_the_configured_timeout() {
    let _path_lock = FAKE_PATH_LOCK.lock().await;
    let fake = FakeCli::new();
    let _path = FakePathGuard::prepend(fake._bin_dir.path());

    for name in ["claude", "claude-print", "opencode", "codex", "aider"] {
        let workspace = unique_workspace();
        let mut adapter = documented_adapter(name);
        // Use hard-only mode so the timeout reason is deterministic. Legacy
        // mode arms both idle and hard deadlines and either one may win the
        // same instant when the fake is silent.
        adapter.timeout_secs = 0;
        adapter.hard_timeout_secs = 1;
        let result = dispatch_adapter(
            adapter,
            name,
            &fake,
            &[("NEEDLE_FAKE_AGENT_MODE", "timeout".to_string())],
            &format!("needle-matrix-{name}-timeout"),
            workspace.path(),
        )
        .await
        .expect("timed-out agent is still a completed dispatch");

        assert_eq!(result.exit_code, 124, "{name} timeout exit code");
        assert_eq!(
            result.timeout_reason,
            Some(needle::dispatch::TimeoutReason::Hard { timeout_secs: 1 }),
            "{name} timeout reason"
        );
    }
}

#[tokio::test]
async fn documented_adapters_classify_exit_codes_and_structured_results() {
    let _path_lock = FAKE_PATH_LOCK.lock().await;
    let fake = FakeCli::new();
    let _path = FakePathGuard::prepend(fake._bin_dir.path());

    for name in ["claude", "claude-print", "opencode", "codex", "aider"] {
        let workspace = trace_workspace();
        let bead_id = format!("needle-matrix-{name}-success");
        let result = dispatch_adapter(
            documented_adapter(name),
            name,
            &fake,
            &[],
            &bead_id,
            workspace.path(),
        )
        .await
        .expect("successful adapter dispatch");

        assert_eq!(result.exit_code, 0, "{name} success exit code");
        let metadata = trace_metadata(workspace.path(), &bead_id);
        assert_eq!(
            metadata["outcome"], "success",
            "{name} result classification"
        );
    }

    // Claude stream-json adapters can report an API failure in a result
    // envelope while exiting zero. The envelope must override the process
    // status for both Claude Code and the documented Claude print adapter.
    for name in ["claude", "claude-print"] {
        let workspace = trace_workspace();
        let bead_id = format!("needle-matrix-{name}-api-error");
        let result = dispatch_adapter(
            documented_adapter(name),
            name,
            &fake,
            &[("NEEDLE_FAKE_AGENT_MODE", "api-error".to_string())],
            &bead_id,
            workspace.path(),
        )
        .await
        .expect("structured API error is still a process result");

        assert_eq!(result.exit_code, 0, "{name} API error fixture exits zero");
        let metadata = trace_metadata(workspace.path(), &bead_id);
        assert_eq!(metadata["trace_format"], "claude_json");
        assert_eq!(
            metadata["outcome"], "failure",
            "{name} envelope classification"
        );
        assert_eq!(metadata["terminal_reason"], "api_error");
        assert_eq!(metadata["api_error_status"], 503);
    }
}

#[tokio::test]
async fn missing_agent_cli_reports_command_not_found() {
    // No PATH mutation: the template's CLI is renamed to a binary that cannot
    // exist, so this test never races sibling tests reading the real PATH.
    let fake = FakeCli::new();
    let workspace = unique_workspace();

    let mut adapter: AgentAdapter = builtin_adapters()
        .into_iter()
        .find(|a| a.name == "opencode")
        .expect("opencode builtin")
        .clone();
    adapter
        .environment
        .insert(LOG_ENV.to_string(), fake.log.display().to_string());
    adapter.agent_cli = "needle-matrix-missing-cli".to_string();
    adapter.invoke_template = adapter
        .invoke_template
        .replace("opencode", "needle-matrix-missing-cli");

    let mut adapters = HashMap::new();
    adapters.insert("opencode".to_string(), adapter);

    let dispatcher =
        Dispatcher::with_adapters(adapters, Telemetry::new(MATRIX_WORKER.to_string()), 3600)
            .with_worker_id(MATRIX_WORKER.to_string());

    let adapter_ref = dispatcher.adapter("opencode").expect("adapter wired");
    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("needle-matrix-missing-cli"),
            &test_prompt("needle-matrix-missing-cli"),
            adapter_ref,
            workspace.path(),
            &matrix_context(workspace.path()),
        )
        .await
        .expect("missing CLI is a process exit, not a dispatch error");

    assert_eq!(
        result.exit_code, 127,
        "shell reports a missing command as 127"
    );
    assert!(
        result.stderr.contains("command not found"),
        "stderr must name the failure: {:?}",
        result.stderr
    );
    assert!(
        !fake.log.exists(),
        "the fake CLI must not have been invoked"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Dispatch routing: README-named models reach the OpenCode/Codex/Aider adapters
// ──────────────────────────────────────────────────────────────────────────────

/// The model→adapter routing a mixed fleet from the README's parallel-workers
/// diagram implies: qwen models on OpenCode, gpt models on Codex CLI, sonnet
/// models on Aider, and everything unmatched on the default Claude adapter.
fn fleet_routing() -> RoutingConfig {
    RoutingConfig {
        rules: vec![
            RoutingRule {
                match_model: "qwen.*".to_string(),
                adapter: "opencode".to_string(),
            },
            RoutingRule {
                match_model: "gpt-.*".to_string(),
                adapter: "codex".to_string(),
            },
            RoutingRule {
                match_model: ".*sonnet.*".to_string(),
                adapter: "aider".to_string(),
            },
        ],
        default_adapter: Some("claude".to_string()),
        strict: false,
    }
}

/// A config carrying [`fleet_routing`] over the README's recommended default.
fn fleet_config() -> Config {
    Config {
        agent: AgentConfig {
            default: "claude".to_string(),
            args: vec![],
            timeout: 3600,
            adapters_dir: PathBuf::from("/nonexistent"),
            routing: Some(fleet_routing()),
            evidence_routing: Default::default(),
        },
        ..Default::default()
    }
}

/// A dispatcher wiring the shipped built-in adapters the README names, with
/// the fake-CLI log destination injected through each adapter's own
/// environment map (the same mechanism the invocation tests above use).
fn fleet_dispatcher(fake: &FakeCli) -> Dispatcher {
    let mut adapters = HashMap::new();
    for name in ["claude", "opencode", "codex", "aider"] {
        let mut adapter = builtin_adapters()
            .into_iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("builtin adapter {name} missing from the registry"))
            .clone();
        adapter.output_transform = None;
        adapter
            .environment
            .insert(LOG_ENV.to_string(), fake.log.display().to_string());
        adapters.insert(name.to_string(), adapter);
    }
    Dispatcher::with_adapters(adapters, Telemetry::new(MATRIX_WORKER.to_string()), 3600)
        .with_worker_id(MATRIX_WORKER.to_string())
}

#[tokio::test]
async fn readme_models_route_to_and_dispatch_the_named_adapters() {
    let _path_lock = FAKE_PATH_LOCK.lock().await;
    let fake = FakeCli::new();
    let _path = FakePathGuard::prepend(fake._bin_dir.path());
    let dispatcher = fleet_dispatcher(&fake);
    let config = fleet_config();

    // (model, expected adapter, the flag only that adapter's CLI receives).
    // The model families mirror the README's parallel-worker names:
    // qwen→OpenCode, gpt→Codex, sonnet→Aider. The sonnet case also pins that
    // a matching rule beats `default_adapter` ("claude"), the first-match
    // contract the Claude routing tests pin from the other side.
    let cases: [(&str, &str, &str); 3] = [
        ("qwen3.6-35b", "opencode", "--format"),
        ("gpt-5.6-terra", "codex", "exec"),
        ("claude-sonnet-4-6", "aider", "--message"),
    ];

    for (model, expected_adapter, distinguishing_flag) in cases {
        let routed = dispatcher.resolve_adapter_name(model, &config);
        assert_eq!(
            routed, expected_adapter,
            "model {model} must route to the {expected_adapter} adapter"
        );

        let bead_id = format!("needle-matrix-route-{expected_adapter}");
        let workspace = unique_workspace();
        let adapter_ref = dispatcher
            .adapter(&routed)
            .expect("routed adapter must be wired in the registry");
        let result = dispatcher
            .dispatch_with_context(
                &BeadId::from(bead_id.as_str()),
                &test_prompt(&bead_id),
                adapter_ref,
                workspace.path(),
                &matrix_context(workspace.path()),
            )
            .await
            .expect("routed dispatch should complete");

        assert_eq!(
            result.exit_code, 0,
            "{expected_adapter} routed dispatch exits 0"
        );
        assert!(
            result
                .stdout
                .contains(&format!("fake-{expected_adapter}-ok")),
            "routing must land on the {expected_adapter} CLI, not another adapter's"
        );

        let expected_prompt = format!("matrix smoke prompt for {bead_id}");
        let lines = read_log(&fake);
        let args = log_values(&lines, "arg");
        assert!(
            args.contains(&distinguishing_flag),
            "{expected_adapter} invocation must carry {distinguishing_flag}"
        );
        match expected_adapter {
            // OpenCode's stdin input method receives the prompt through its
            // stdin stream, just like the Claude adapter.
            "opencode" => assert!(
                log_values(&lines, "stdin").contains(&expected_prompt.as_str()),
                "opencode must receive the prompt through stdin"
            ),
            // Codex's args template and Aider's --message render the prompt
            // itself into argv.
            _ => assert!(
                args.contains(&expected_prompt.as_str()),
                "{expected_adapter} must receive the rendered prompt in argv"
            ),
        }
    }
}

#[test]
fn unrouted_models_fall_back_to_the_configured_default_adapter() {
    // Adapters are wired for registry realism, but nothing spawns and PATH is
    // untouched, so no PATH lock is needed.
    let fake = FakeCli::new();
    let dispatcher = fleet_dispatcher(&fake);
    let config = fleet_config();

    for model in ["llama-3-70b", "mistral-large", "deepseek-coder"] {
        assert_eq!(
            dispatcher.resolve_adapter_name(model, &config),
            "claude",
            "unrouted model {model} must land on the configured default adapter"
        );
    }
}
