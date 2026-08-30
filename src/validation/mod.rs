//! Validation gates: pre-closure verification.
//!
//! After an agent exits successfully (code 0), validation gates run to verify
//! the work before accepting bead closure. If any gate fails, the bead is
//! released instead of having its closure accepted.
//!
//! # Pluggable Gate System
//!
//! Gates implement the [`Gate`] trait and are registered in the [`GateRegistry`].
//! Built-in gate types:
//! - `command`: Runs shell commands in the workspace directory
//!
//! Custom gates can be registered at runtime by calling [`GateRegistry::register`].
//!
//! Inspired by bg-gate (docs/research/bg-gate-validation.md).

pub mod predispatch;
mod shipped_work;
pub mod worker_config;
pub use shipped_work::verify_shipped_work;

mod gate_path_validation;
pub use gate_path_validation::{
    validate_gate_command_paths, GatePathValidationError, GatePathValidationResult, PathType,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{ConfigTier, ReloadTier};

// ──────────────────────────────────────────────────────────────────────────────
// Types
// ──────────────────────────────────────────────────────────────────────────────

/// Result of a single gate validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateResult {
    /// The gate passed validation.
    Pass,
    /// The gate failed validation with a reason.
    Fail(String),
    /// The gate could not run (execution error: ENOENT/EACCES/missing directory/timeout).
    ExecutionError {
        /// The command that could not run.
        command: String,
        /// Human-readable error reason (e.g., "ENOENT", "EACCES", "directory not found").
        reason: String,
    },
}

impl GateResult {
    /// Returns true if the gate passed.
    pub fn passed(&self) -> bool {
        matches!(self, GateResult::Pass)
    }

    /// Returns the failure reason if this is a `Fail` result.
    pub fn failure_reason(&self) -> Option<&str> {
        match self {
            GateResult::Pass => None,
            GateResult::Fail(reason) => Some(reason),
            GateResult::ExecutionError { .. } => None,
        }
    }

    /// Returns true if this is an execution error (gate could not run).
    pub fn is_execution_error(&self) -> bool {
        matches!(self, GateResult::ExecutionError { .. })
    }
}

/// Aggregated result of running multiple validation gates.
#[derive(Debug)]
pub struct GateReport {
    /// Whether all gates passed.
    pub all_passed: bool,
    /// Individual gate results keyed by gate name.
    pub results: HashMap<String, GateResult>,
}

impl GateReport {
    /// Create a new report from individual gate results.
    pub fn new(results: HashMap<String, GateResult>) -> Self {
        let all_passed = results.values().all(|r| r.passed());
        GateReport {
            all_passed,
            results,
        }
    }

    /// Create a report where all gates passed.
    pub fn all_pass() -> Self {
        GateReport {
            all_passed: true,
            results: HashMap::new(),
        }
    }

    /// Create a report with a single gate failure.
    pub fn single_failure(gate_name: impl Into<String>, reason: impl Into<String>) -> Self {
        let mut results = HashMap::new();
        results.insert(gate_name.into(), GateResult::Fail(reason.into()));
        GateReport {
            all_passed: false,
            results,
        }
    }

    /// Returns true if all gates passed.
    pub fn passed(&self) -> bool {
        self.all_passed
    }

    /// Convert to a single GateResult.
    ///
    /// If all gates passed, returns Pass. Otherwise returns Fail with the first failure reason.
    pub fn to_gate_result(&self) -> GateResult {
        if self.all_passed {
            GateResult::Pass
        } else {
            // Get the first failure reason
            let reason = self
                .results
                .values()
                .find_map(|r| r.failure_reason().map(|s| s.to_string()))
                .unwrap_or_else(|| "verification gate failed".to_string());
            GateResult::Fail(reason)
        }
    }
}

/// Execution mode for a gate command.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunIn {
    /// Run in a clean extraction of committed state (git archive HEAD).
    #[default]
    Clean,
    /// Run in the shared workspace checkout (may contain uncommitted changes).
    Workspace,
}

/// Configuration for a single gate from `.needle.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GateConfig {
    /// Run shell commands in the workspace directory.
    Command {
        commands: Vec<String>,
        /// Maximum bytes of stderr captured on failure. `None` means "inherit
        /// `validation.stderr_cap_bytes`" — the caller assembling gate configs
        /// (see `outcome::OutcomeHandler::handle_success`) fills this in from
        /// the resolved `Config` before construction. See GitHub issue
        /// jedarden/NEEDLE#9.
        #[serde(default)]
        stderr_cap_bytes: Option<usize>,
        /// Whether to run commands in a clean extraction of committed state or
        /// in the shared workspace checkout. `clean` (default) extracts HEAD
        /// via `git archive` to a temp directory and runs there; `workspace`
        /// runs directly in the shared checkout. Use `workspace` only for
        /// gates that must see uncommitted state (e.g., testing a build cache).
        /// See ADR-020 for the full rationale.
        #[serde(default)]
        run_in: RunIn,
    },
}

impl ConfigTier for Vec<GateConfig> {
    fn reload_tier(&self) -> ReloadTier {
        // Tier B: Gate configuration requires rebuilding OutcomeHandler
        ReloadTier::Rebuild
    }
}

/// Validation errors that can occur during bead store validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// Invalid dependency kind or type.
    InvalidKind {
        /// The invalid kind value that was encountered.
        kind: String,
    },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::InvalidKind { kind } => {
                write!(f, "invalid kind: '{}'", kind)
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// A single gate command that failed (for backwards compatibility).
#[derive(Debug, Clone)]
pub struct GateFailure {
    /// The command that was run.
    pub command: String,
    /// Process exit code (None if killed by signal).
    pub exit_code: Option<i32>,
    /// Combined stderr output (truncated to a reasonable length).
    pub output: String,
}

/// Aggregated result of running a `ValidationGate`.
#[derive(Debug)]
pub struct ValidationRunResult {
    /// Whether all gate commands passed.
    pub passed: bool,
    /// List of failures (empty when `passed` is true).
    pub failures: Vec<GateFailure>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Gate Trait
// ──────────────────────────────────────────────────────────────────────────────

/// A validation gate that can check bead work before accepting closure.
///
/// Gates are run after an agent exits successfully (code 0). If any gate
/// returns `GateResult::Fail`, the bead is released instead of having its
/// closure accepted.
///
/// `validate` is async so a slow gate is a genuine `.await` yield point:
/// `OutcomeHandler::handle_with_cancellation`'s `tokio::time::timeout` can
/// only preempt a future at such a point (see bf-3saat) — a synchronous,
/// blocking implementation would let the configured
/// `validation.outcome_timeout_seconds` observe a slow gate only after the
/// fact, never actually cut it off.
#[async_trait::async_trait]
pub trait Gate: Send + Sync {
    /// Validate the bead's work in the given workspace.
    ///
    /// Returns `GateResult::Pass` if validation succeeds, or `GateResult::Fail`
    /// with a human-readable reason if it fails.
    async fn validate(&self, bead: &crate::types::Bead, workspace: &Path) -> Result<GateResult>;

    /// Gate type name for telemetry and configuration (e.g., "command", "custom").
    fn gate_type(&self) -> &str;
}

// ──────────────────────────────────────────────────────────────────────────────
// Gate Registry
// ──────────────────────────────────────────────────────────────────────────────

/// Registry for pluggable validation gates.
///
/// Gates are registered by type name. The registry is thread-safe and supports
/// dynamic registration of custom gate types.
pub struct GateRegistry {
    #[allow(clippy::type_complexity)]
    gates: RwLock<HashMap<String, Arc<dyn Fn(&GateConfig) -> Result<Arc<dyn Gate>> + Send + Sync>>>,
}

impl Default for GateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl GateRegistry {
    /// Create a new registry with built-in gate types registered.
    pub fn new() -> Self {
        let registry = GateRegistry {
            gates: RwLock::new(HashMap::new()),
        };
        // Register built-in gate types
        registry.register_builtin_gates();
        registry
    }

    /// Register a built-in gate type constructor.
    fn register_builtin_gates(&self) {
        self.register("command", |config| match config {
            GateConfig::Command {
                commands,
                stderr_cap_bytes,
                run_in,
            } => Ok(Arc::new(CommandGate::with_options(
                commands.clone(),
                stderr_cap_bytes.unwrap_or(DEFAULT_STDERR_CAP_BYTES),
                *run_in,
            ))),
        });
    }

    /// Register a custom gate type constructor.
    ///
    /// The constructor function takes a `GateConfig` and returns a boxed `Gate`.
    /// Custom gates should parse their config from the `GateConfig` enum.
    ///
    /// # Example
    /// ```ignore
    /// registry.register("my_gate", |config| {
    ///     // Parse config and create custom gate
    ///     Ok(Arc::new(MyCustomGate::new(config)?))
    /// });
    /// ```
    pub fn register<F>(&self, gate_type: impl Into<String>, constructor: F)
    where
        F: Fn(&GateConfig) -> Result<Arc<dyn Gate>> + Send + Sync + 'static,
    {
        let gate_type = gate_type.into();
        let mut guards = self.gates.write().unwrap();
        guards.insert(gate_type, Arc::new(constructor));
    }

    /// Create a gate instance from its configuration.
    ///
    /// Returns an error if the gate type is not registered or construction fails.
    pub fn create_gate(&self, config: &GateConfig) -> Result<Arc<dyn Gate>> {
        let gate_type = match config {
            GateConfig::Command { .. } => "command",
        };

        let guards = self.gates.read().unwrap();
        let constructor = guards
            .get(gate_type)
            .ok_or_else(|| anyhow::anyhow!("unknown gate type '{}': not registered", gate_type))?;

        constructor(config)
    }

    /// Get the global gate registry instance.
    pub fn global() -> &'static GateRegistry {
        use std::sync::OnceLock;
        static REGISTRY: OnceLock<GateRegistry> = OnceLock::new();
        REGISTRY.get_or_init(GateRegistry::new)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Built-in Gate Types
// ──────────────────────────────────────────────────────────────────────────────

/// Default stderr capture cap (bytes) — preserves the previous hardcoded value.
const DEFAULT_STDERR_CAP_BYTES: usize = 4096;

/// Runs configured shell commands as validation gates.
pub struct CommandGate {
    commands: Vec<String>,
    /// Maximum bytes of stderr captured on failure. Configurable via
    /// `validation.stderr_cap_bytes` — see GitHub issue jedarden/NEEDLE#9.
    stderr_cap_bytes: usize,
    /// Whether to run in a clean extraction or the shared workspace.
    run_in: RunIn,
}

impl CommandGate {
    /// Create a new command gate with the default stderr cap (4096 bytes) and workspace execution.
    pub fn new(commands: Vec<String>) -> Self {
        Self::with_options(commands, DEFAULT_STDERR_CAP_BYTES, RunIn::Workspace)
    }

    /// Create a new command gate with an explicit stderr capture cap and workspace execution.
    pub fn with_stderr_cap(commands: Vec<String>, stderr_cap_bytes: usize) -> Self {
        Self::with_options(commands, stderr_cap_bytes, RunIn::Workspace)
    }

    /// Create a new command gate with full options.
    pub fn with_options(commands: Vec<String>, stderr_cap_bytes: usize, run_in: RunIn) -> Self {
        CommandGate {
            commands,
            stderr_cap_bytes,
            run_in,
        }
    }
}

/// Extract committed state from a workspace to a temporary directory.
///
/// This creates a clean copy of the workspace's committed state (git HEAD)
/// to a temporary directory using `git archive`. This is used for running
/// gates in isolation from uncommitted changes.
async fn extract_committed_state(workspace: &Path, bead_id: &str) -> Result<PathBuf> {
    use std::process::Command;

    // Name the extraction after the bead. A failing clean gate leaves this
    // directory behind on purpose, and a bare `.tmpXXXXXX` is not something
    // anyone can find later or attribute to a bead.
    let safe_bead_id: String = bead_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let temp_dir = tempfile::Builder::new()
        .prefix(&format!("needle-clean-{safe_bead_id}-"))
        .tempdir()
        .context("failed to create temporary directory for committed state extraction")?;

    let extract_dir = temp_dir.path();

    // Extract git HEAD to the temporary directory using git archive
    let output = Command::new("git")
        .args(["archive", "--format=tar", "HEAD"])
        .current_dir(workspace)
        .output()
        .context("failed to run git archive")?;

    if !output.status.success() {
        anyhow::bail!(
            "git archive failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Extract the tar archive to the temporary directory.
    //
    // stdin MUST be piped: without it the child inherits this process's stdin,
    // `child.stdin` is None, the archive is silently never written, and tar
    // fails on whatever it reads instead — which is how every `run_in: clean`
    // gate failed with a bare "tar extraction failed".
    let mut child = Command::new("tar")
        .args(["-x", "-f", "-"])
        .current_dir(extract_dir)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn tar extraction")?;

    // Write the archive to tar's stdin, then close it so tar sees EOF.
    {
        let mut stdin = child
            .stdin
            .take()
            .context("tar extraction stdin was not piped")?;
        std::io::copy(&mut output.stdout.as_slice(), &mut stdin)
            .context("failed to write archive to tar")?;
    }

    let tar_output = child
        .wait_with_output()
        .context("failed to wait for tar extraction")?;

    if !tar_output.status.success() {
        anyhow::bail!(
            "tar extraction failed: {}",
            String::from_utf8_lossy(&tar_output.stderr).trim()
        );
    }

    // Keep the temp directory alive by leaking it
    // This is safe because the temporary directory will be cleaned up when the process exits
    let leaked_dir = PathBuf::from(extract_dir);
    std::mem::forget(temp_dir);

    Ok(leaked_dir)
}

#[async_trait::async_trait]
impl Gate for CommandGate {
    async fn validate(&self, bead: &crate::types::Bead, workspace: &Path) -> Result<GateResult> {
        // Try running in clean mode first (if configured)
        // Returns: (result, clean_directory_path_if_any)
        let clean_attempt = if self.run_in == RunIn::Clean {
            // Extract committed state to a temporary directory
            match extract_committed_state(workspace, &bead.id).await {
                Ok(dir) => {
                    tracing::info!(
                        workspace = %workspace.display(),
                        clean_dir = %dir.display(),
                        "extracted committed state for clean gate execution"
                    );
                    // Run commands in the clean extraction
                    let result = self.run_commands_in_dir(bead, &dir, true).await;

                    // Clean up extraction on success (preserve on failure for diagnosis)
                    if result.all_passed {
                        tracing::debug!(
                            clean_dir = %dir.display(),
                            "removing clean extraction (all gates passed)"
                        );
                        let _ = tokio::fs::remove_dir_all(&dir).await;
                        // Return result and no directory (it was cleaned up)
                        (Some(result), None)
                    } else {
                        tracing::info!(
                            clean_dir = %dir.display(),
                            "preserving clean extraction for diagnosis (gate failed)"
                        );
                        // Return result and the directory path (preserved for diagnosis)
                        (Some(result), Some(dir))
                    }
                }
                Err(e) => {
                    tracing::error!(
                        workspace = %workspace.display(),
                        error = %e,
                        "failed to extract committed state for clean gate execution"
                    );
                    // Extraction failure counts as a failure in clean mode
                    let failure = GateReport {
                        all_passed: false,
                        results: self.make_failure_map(
                            "extraction",
                            format!("failed to extract committed state: {}", e).to_string(),
                        ),
                    };
                    (Some(failure), None)
                }
            }
        } else {
            (None, None)
        };

        let (clean_result, clean_dir) = clean_attempt;

        // If clean mode failed, try in workspace mode to detect uncommitted dependencies
        if let Some(clean_result) = clean_result {
            if !clean_result.all_passed {
                tracing::info!(
                    bead_id = %bead.id,
                    "clean mode failed, checking if workspace mode passes (uncommitted dependency detection)"
                );

                // Try running in workspace mode
                let workspace_result = self.run_commands_in_dir(bead, workspace, false).await;

                if workspace_result.all_passed {
                    // Clean failed but workspace passed: uncommitted dependency detected
                    tracing::warn!(
                        bead_id = %bead.id,
                        "detected uncommitted dependency: clean failed but workspace passed"
                    );

                    // Get git diff for context
                    let diff_output = match get_git_diff(workspace).await {
                        Ok(diff) => diff,
                        Err(e) => format!("(failed to get diff: {})", e),
                    };

                    // Clean up the preserved clean extraction (we detected uncommitted dependency)
                    if let Some(dir) = clean_dir {
                        tracing::debug!(
                            clean_dir = %dir.display(),
                            "removing clean extraction (uncommitted dependency detected)"
                        );
                        let _ = tokio::fs::remove_dir_all(&dir).await;
                    }

                    return Ok(GateResult::Fail(format!(
                        "passes only with uncommitted files: {}\n\nThis indicates the code depends on uncommitted changes. Either commit the changes or fix the underlying issue.",
                        diff_output
                    )));
                } else {
                    // Both modes failed, return the original clean failure
                    tracing::info!(
                        bead_id = %bead.id,
                        "both clean and workspace modes failed, returning clean failure"
                    );
                    // Clean extraction is preserved for diagnosis (clean_dir is Some)
                    return Ok(self.to_gate_result(clean_result));
                }
            }
            // Clean mode passed, return success
            return Ok(self.to_gate_result(clean_result));
        }

        // Not running in clean mode, or clean wasn't attempted - run directly in workspace
        let result = self.run_commands_in_dir(bead, workspace, false).await;
        Ok(self.to_gate_result(result))
    }

    fn gate_type(&self) -> &str {
        "command"
    }
}

impl CommandGate {
    /// Run all commands in the specified directory and return a report.
    async fn run_commands_in_dir(
        &self,
        bead: &crate::types::Bead,
        dir: &Path,
        is_clean: bool,
    ) -> GateReport {
        let mut results = HashMap::new();

        for cmd in &self.commands {
            tracing::info!(
                command = %cmd,
                workspace = %bead.workspace.display(),
                run_dir = if is_clean { "clean" } else { "workspace" },
                execution_dir = %dir.display(),
                "running command gate"
            );

            match self.run_command(cmd, bead, dir).await {
                Ok(()) => {
                    tracing::info!(command = %cmd, "command gate passed");
                    results.insert(cmd.clone(), GateResult::Pass);
                }
                Err(failure) => {
                    // Detect execution errors: command could not run at all.
                    // These produce failure.exit_code=None and error messages like
                    // "failed to execute command: No such file or directory" (ENOENT)
                    // or "Permission denied" (EACCES).
                    let is_execution_error = failure.exit_code.is_none()
                        && (failure.output.contains("No such file or directory")
                            || failure.output.contains("Permission denied")
                            || failure.output.contains("failed to execute command"));

                    if is_execution_error {
                        tracing::warn!(
                            command = %cmd,
                            error = %failure.output,
                            "gate execution error — command could not run"
                        );
                        // Extract the error reason (e.g., "ENOENT", "EACCES")
                        let reason = if failure.output.contains("No such file") {
                            "ENOENT".to_string()
                        } else if failure.output.contains("Permission denied") {
                            "EACCES".to_string()
                        } else {
                            "execution_failed".to_string()
                        };
                        results.insert(
                            cmd.clone(),
                            GateResult::ExecutionError {
                                command: cmd.clone(),
                                reason,
                            },
                        );
                    } else {
                        tracing::warn!(
                            command = %cmd,
                            exit_code = ?failure.exit_code,
                            "command gate failed"
                        );
                        results.insert(
                            cmd.clone(),
                            GateResult::Fail(format!(
                                "command '{}' failed: {}",
                                cmd,
                                failure.output.trim()
                            )),
                        );
                    }
                    // Stop on first failure/error
                    break;
                }
            }
        }

        GateReport::new(results)
    }

    /// Convert a GateReport to GateResult (for backwards compatibility).
    fn to_gate_result(&self, report: GateReport) -> GateResult {
        if report.all_passed {
            GateResult::Pass
        } else {
            // Get the first failure reason
            let reason = report
                .results
                .values()
                .find_map(|r| r.failure_reason().map(|s| s.to_string()))
                .unwrap_or_else(|| "verification gate failed".to_string());
            GateResult::Fail(reason)
        }
    }

    /// Create a failure map for a single failure.
    fn make_failure_map(&self, command: &str, reason: String) -> HashMap<String, GateResult> {
        let mut results = HashMap::new();
        results.insert(command.to_string(), GateResult::Fail(reason));
        results
    }
}

impl CommandGate {
    /// Run a single command. Returns `Ok(())` on exit 0, `Err(GateFailure)` otherwise.
    ///
    /// The command's environment carries `NEEDLE_BEAD_ID` and `NEEDLE_WORKSPACE`
    /// so a gate can identify the bead it is judging without racily guessing it
    /// from external state (e.g. `br list --json` assignee) — see GitHub issue
    /// jedarden/NEEDLE#7.
    ///
    /// Uses `tokio::process::Command` (not `std::process::Command`) with
    /// `kill_on_drop(true)`: this makes the command a genuine `.await` yield
    /// point, so `OutcomeHandler::handle_with_cancellation`'s
    /// `tokio::time::timeout` can actually abandon and kill a gate command
    /// that outruns `validation.outcome_timeout_seconds`, instead of only
    /// observing after the fact that it ran too long (see bf-3saat — a
    /// blocking `std::process::Command` call has no yield point, so a
    /// wrapping timeout can never preempt it mid-command).
    async fn run_command(
        &self,
        cmd: &str,
        bead: &crate::types::Bead,
        workspace: &Path,
    ) -> std::result::Result<(), GateFailure> {
        let result = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(workspace)
            .env("NEEDLE_BEAD_ID", bead.id.to_string())
            .env("NEEDLE_WORKSPACE", workspace.display().to_string())
            .kill_on_drop(true)
            .output()
            .await;

        match result {
            Ok(output) => {
                if output.status.success() {
                    Ok(())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let truncated = truncate_output(&stderr, self.stderr_cap_bytes);
                    Err(GateFailure {
                        command: cmd.to_string(),
                        exit_code: output.status.code(),
                        output: truncated,
                    })
                }
            }
            Err(e) => Err(GateFailure {
                command: cmd.to_string(),
                exit_code: None,
                output: format!("failed to execute command: {}", e),
            }),
        }
    }
}

/// Truncate output to at most `max_bytes`, adding an ellipsis if truncated.
fn truncate_output(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        let truncated = &s[..max_bytes];
        format!("{}... [truncated]", truncated)
    }
}

/// List the files that differ from HEAD, for uncommitted-dependency reporting.
///
/// `git status --porcelain`, not `git diff --name-only`: the usual cause of a
/// gate that passes in the workspace and fails on a clean extraction is a file
/// that was never added at all, and a diff lists only tracked modifications —
/// so the diagnostic that is supposed to name the missing file came back empty
/// in exactly the case it exists for.
async fn get_git_diff(workspace: &Path) -> Result<String> {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(workspace)
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to get git status")?;

    if !output.status.success() {
        anyhow::bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Porcelain v1: "XY <path>", with renames as "XY <old> -> <new>".
    let files: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.get(3..))
        .map(|path| path.rsplit(" -> ").next().unwrap_or(path).trim())
        .filter(|path| !path.is_empty())
        .collect();
    Ok(files.join("\n"))
}

// ──────────────────────────────────────────────────────────────────────────────
// ValidationGate (Main Entry Point)
// ──────────────────────────────────────────────────────────────────────────────

/// Runs configured verification gates in a workspace directory.
///
/// This is the main entry point for validation. It uses the pluggable gate
/// system to run all configured gates and returns an aggregated report.
pub struct ValidationGate {
    gates: Vec<(String, Arc<dyn Gate>)>,
    workspace: PathBuf,
}

impl ValidationGate {
    /// Create a new validation gate from gate configurations.
    ///
    /// Returns `None` if `gate_configs` is empty (no verification configured).
    pub fn new(gate_configs: Vec<(String, GateConfig)>, workspace: PathBuf) -> Option<Self> {
        if gate_configs.is_empty() {
            return None;
        }

        let registry = GateRegistry::global();
        let mut gates = Vec::new();

        for (name, config) in gate_configs {
            match registry.create_gate(&config) {
                Ok(gate) => gates.push((name, gate)),
                Err(e) => {
                    tracing::warn!(
                        gate_name = %name,
                        error = %e,
                        "failed to create gate — skipping"
                    );
                }
            }
        }

        if gates.is_empty() {
            return None;
        }

        Some(ValidationGate { gates, workspace })
    }

    /// Create from legacy command list (backwards compatibility).
    ///
    /// This method maintains the existing API for code that uses `Vec<String>`
    /// for verification commands. Uses the default stderr cap (4096 bytes);
    /// use [`Self::from_commands_with_stderr_cap`] to configure it.
    pub fn from_commands(commands: Vec<String>, workspace: PathBuf) -> Option<Self> {
        Self::from_commands_with_stderr_cap(commands, workspace, DEFAULT_STDERR_CAP_BYTES)
    }

    /// Create from legacy command list with an explicit stderr capture cap.
    ///
    /// See GitHub issue jedarden/NEEDLE#9 — used by
    /// `OutcomeHandler::handle_success` to apply `validation.stderr_cap_bytes`
    /// to the legacy `verification:` config format.
    pub fn from_commands_with_stderr_cap(
        commands: Vec<String>,
        workspace: PathBuf,
        stderr_cap_bytes: usize,
    ) -> Option<Self> {
        if commands.is_empty() {
            return None;
        }
        let gate = Arc::new(CommandGate::with_stderr_cap(commands, stderr_cap_bytes));
        Some(ValidationGate {
            gates: vec![("command_gate".to_string(), gate as Arc<dyn Gate>)],
            workspace,
        })
    }

    /// Run all gates sequentially. Stops at the first failure.
    pub async fn run(&self, bead: &crate::types::Bead) -> Result<GateReport> {
        let mut results = HashMap::new();

        for (name, gate) in &self.gates {
            let result = gate.validate(bead, &self.workspace).await?;
            results.insert(name.clone(), result);

            // Stop on first failure.
            if !results.values().all(|r| r.passed()) {
                break;
            }
        }

        Ok(GateReport::new(results))
    }

    /// Workspace directory where gates run.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Bead, BeadId, BeadStatus};
    use chrono::Utc;
    use tempfile::TempDir;

    fn test_bead() -> Bead {
        Bead {
            id: BeadId::from("needle-test"),
            title: "Test bead".to_string(),
            body: Some("Test body".to_string()),
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some("worker-01".to_string()),
            labels: vec![],
            workspace: PathBuf::from("/tmp"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ── GateResult tests ──

    #[test]
    fn gate_result_pass_returns_true() {
        assert!(GateResult::Pass.passed());
    }

    #[test]
    fn gate_result_fail_returns_false() {
        assert!(!GateResult::Fail("error".to_string()).passed());
    }

    #[test]
    fn gate_result_fail_has_reason() {
        let result = GateResult::Fail("test error".to_string());
        assert_eq!(result.failure_reason(), Some("test error"));
    }

    #[test]
    fn gate_result_pass_has_no_reason() {
        assert!(GateResult::Pass.failure_reason().is_none());
    }

    // ── GateReport tests ──

    #[test]
    fn gate_report_all_pass() {
        let report = GateReport::all_pass();
        assert!(report.all_passed);
        assert!(report.results.is_empty());
    }

    #[test]
    fn gate_report_single_failure() {
        let report = GateReport::single_failure("test_gate", "failed");
        assert!(!report.all_passed);
        assert_eq!(report.results.len(), 1);
        assert!(!report.results["test_gate"].passed());
    }

    #[test]
    fn gate_report_new_from_results() {
        let mut results = HashMap::new();
        results.insert("gate1".to_string(), GateResult::Pass);
        results.insert("gate2".to_string(), GateResult::Pass);
        let report = GateReport::new(results);
        assert!(report.all_passed);
        assert_eq!(report.results.len(), 2);
    }

    #[test]
    fn gate_report_new_with_failure() {
        let mut results = HashMap::new();
        results.insert("gate1".to_string(), GateResult::Pass);
        results.insert("gate2".to_string(), GateResult::Fail("error".to_string()));
        let report = GateReport::new(results);
        assert!(!report.all_passed);
    }

    // ── GateRegistry tests ──

    #[test]
    fn registry_global_returns_same_instance() {
        let r1 = GateRegistry::global();
        let r2 = GateRegistry::global();
        // Same pointer means same instance
        assert!(std::ptr::eq(r1, r2));
    }

    #[test]
    fn registry_creates_command_gate() {
        let registry = GateRegistry::global();
        let config = GateConfig::Command {
            commands: vec!["true".to_string()],
            stderr_cap_bytes: None,
            run_in: RunIn::Clean,
        };
        let gate = registry.create_gate(&config).unwrap();
        assert_eq!(gate.gate_type(), "command");
    }

    #[test]
    fn registry_fails_unknown_gate_type() {
        // We can't directly test unknown types since the registry uses string matching,
        // but we can verify the error path exists by using an invalid config variant
        // if we had one. For now, this test documents the expected behavior.
    }

    #[test]
    fn registry_register_custom_gate() {
        let registry = GateRegistry::new(); // Fresh registry

        // Register a test gate
        registry.register("test_gate", |_| Ok(Arc::new(TestGate)));

        // Verify we can create it (would need custom config variant)
        // This documents the registration API
    }

    // Test gate for registry testing
    struct TestGate;

    #[async_trait::async_trait]
    impl Gate for TestGate {
        async fn validate(
            &self,
            _bead: &crate::types::Bead,
            _workspace: &Path,
        ) -> Result<GateResult> {
            Ok(GateResult::Pass)
        }

        fn gate_type(&self) -> &str {
            "test"
        }
    }

    // ── CommandGate tests ──

    #[tokio::test]
    async fn command_gate_passes_on_true() {
        let gate = CommandGate::new(vec!["true".to_string()]);
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).await.unwrap();
        assert!(result.passed());
    }

    #[tokio::test]
    async fn command_gate_fails_on_false() {
        let gate = CommandGate::new(vec!["false".to_string()]);
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).await.unwrap();
        assert!(!result.passed());
        assert!(result.failure_reason().unwrap().contains("failed"));
    }

    #[tokio::test]
    async fn command_gate_stops_at_first_failure() {
        let gate = CommandGate::new(vec![
            "true".to_string(),
            "false".to_string(),
            "echo should-not-run".to_string(),
        ]);
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).await.unwrap();
        assert!(!result.passed());
        // Should be the false command that failed
        assert!(result.failure_reason().unwrap().contains("false"));
    }

    #[test]
    fn command_gate_type() {
        let gate = CommandGate::new(vec!["true".to_string()]);
        assert_eq!(gate.gate_type(), "command");
    }

    #[tokio::test]
    async fn command_gate_exposes_bead_id_and_workspace_env() {
        // GitHub issue jedarden/NEEDLE#7: gate commands had no way to identify
        // the bead they were judging. The command below fails (non-zero exit)
        // unless both env vars are present with the expected values, so a
        // passing gate result is proof the env was actually set.
        let gate = CommandGate::new(vec![
            r#"[ "$NEEDLE_BEAD_ID" = "needle-test" ] && [ "$NEEDLE_WORKSPACE" = "/tmp" ]"#
                .to_string(),
        ]);
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).await.unwrap();
        assert!(
            result.passed(),
            "expected gate to see NEEDLE_BEAD_ID=needle-test and NEEDLE_WORKSPACE=/tmp: {:?}",
            result.failure_reason()
        );
    }

    #[tokio::test]
    async fn command_gate_missing_env_fails_the_assertion() {
        // Sanity check for the test above: an unexpected bead ID must fail,
        // proving the assertion isn't vacuously true.
        let gate = CommandGate::new(vec![r#"[ "$NEEDLE_BEAD_ID" = "wrong-id" ]"#.to_string()]);
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).await.unwrap();
        assert!(!result.passed());
    }

    // ── configurable stderr cap tests (GitHub issue jedarden/NEEDLE#9) ──

    /// Shell snippet producing exactly `n` bytes of stderr, then failing.
    /// The command these fixtures gate on.
    ///
    /// Deliberately not `cargo check`: what is under test is that a clean
    /// extraction contains only committed files, and a real compile adds a
    /// whole toolchain run per test — four of them in parallel starved
    /// unrelated timing-sensitive tests in the full suite. `rustc --edition
    /// 2021 --emit=metadata` type-checks the crate root the same way for these
    /// purposes, at a fraction of the cost.
    const CHECK_SOURCES: &str = "rustc --edition 2021 --emit=metadata --crate-type lib \
         --out-dir \"$(mktemp -d)\" \
         \"$([ -f src/main.rs ] && echo src/main.rs || echo src/lib.rs)\"";

    fn fail_with_stderr_bytes(n: usize) -> String {
        format!("head -c {n} /dev/zero | tr '\\0' 'x' 1>&2; exit 1")
    }

    #[tokio::test]
    async fn command_gate_default_cap_truncates_large_stderr() {
        // Baseline: CommandGate::new() (no explicit cap) still truncates at
        // the previous hardcoded default, 4096 bytes.
        let gate = CommandGate::new(vec![fail_with_stderr_bytes(6000)]);
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).await.unwrap();
        assert!(!result.passed());
        assert!(result.failure_reason().unwrap().contains("[truncated]"));
    }

    #[tokio::test]
    async fn command_gate_configured_cap_larger_than_default_avoids_truncation() {
        // A configured cap larger than the old hardcoded 4096 must actually be
        // honored — proof the value isn't still pinned to the old constant.
        let gate = CommandGate::with_stderr_cap(vec![fail_with_stderr_bytes(6000)], 8192);
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).await.unwrap();
        assert!(!result.passed());
        assert!(
            !result.failure_reason().unwrap().contains("[truncated]"),
            "expected full 6000-byte stderr under an 8192 cap, got: {:?}",
            result.failure_reason()
        );
    }

    #[tokio::test]
    async fn command_gate_configured_cap_smaller_than_default_truncates_more() {
        // A cap smaller than the default must also be honored.
        let gate = CommandGate::with_stderr_cap(vec![fail_with_stderr_bytes(200)], 10);
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).await.unwrap();
        assert!(!result.passed());
        assert!(result.failure_reason().unwrap().contains("[truncated]"));
    }

    #[tokio::test]
    async fn gate_config_stderr_cap_bytes_defaults_to_none_and_registry_falls_back_to_4096() {
        // When a "gates:" entry omits stderr_cap_bytes, the registry-created
        // CommandGate must still behave like the pre-#9 hardcoded default when
        // no caller (e.g. OutcomeHandler) has filled in an override.
        let registry = GateRegistry::global();
        let config = GateConfig::Command {
            commands: vec![fail_with_stderr_bytes(6000)],
            stderr_cap_bytes: None,
            // Workspace, not Clean: a Clean gate first extracts committed
            // state, which cannot work in /tmp, and the resulting failure
            // reason is the extraction error rather than the command's
            // stderr — this test is about the cap, not about extraction.
            run_in: RunIn::Workspace,
        };
        let gate = registry.create_gate(&config).unwrap();
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).await.unwrap();
        assert!(!result.passed());
        assert!(result.failure_reason().unwrap().contains("... [truncated]"));
    }

    #[tokio::test]
    async fn command_gate_slow_command_is_killed_when_dropped_mid_flight() {
        // GitHub issue jedarden/NEEDLE#8 follow-up (bf-3saat): the gate command
        // must be a genuine .await yield point so a wrapping tokio::time::timeout
        // can actually cancel and kill it, not just observe it after the fact.
        // Simulate that here directly: race the gate's future against a short
        // timeout and confirm (a) the timeout wins and (b) the child process
        // is actually gone afterward, not left running in the background.
        let marker = tempfile::NamedTempFile::new().unwrap();
        let marker_path = marker.path().to_path_buf();
        std::fs::remove_file(&marker_path).ok();

        // Writes the marker file only after a 3s sleep — if the process is
        // truly killed before then, the marker must never appear.
        let gate = CommandGate::new(vec![format!("sleep 3 && touch {}", marker_path.display())]);
        let bead = test_bead();

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            gate.validate(&bead, Path::new("/tmp")),
        )
        .await;
        assert!(
            result.is_err(),
            "expected the 300ms timeout to fire before the 3s sleep completed"
        );

        // Give the kill signal a moment to actually land, then confirm the
        // command never reached its `touch` — proof the child was killed,
        // not merely abandoned to finish in the background.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            !marker_path.exists(),
            "gate command was not actually killed — it ran to completion in the background"
        );
    }

    // ── ValidationGate tests ──

    #[test]
    fn validation_gate_new_returns_none_for_empty_configs() {
        let gate = ValidationGate::new(vec![], PathBuf::from("/tmp"));
        assert!(gate.is_none());
    }

    #[test]
    fn validation_gate_from_commands_returns_none_for_empty() {
        let gate = ValidationGate::from_commands(vec![], PathBuf::from("/tmp"));
        assert!(gate.is_none());
    }

    #[test]
    fn validation_gate_from_commands_returns_some_for_nonempty() {
        let gate = ValidationGate::from_commands(vec!["true".to_string()], PathBuf::from("/tmp"));
        assert!(gate.is_some());
    }

    #[tokio::test]
    async fn validation_gate_run_passes() {
        let gate =
            ValidationGate::from_commands(vec!["true".to_string()], PathBuf::from("/tmp")).unwrap();
        let bead = test_bead();
        let report = gate.run(&bead).await.unwrap();
        assert!(report.all_passed);
    }

    #[tokio::test]
    async fn validation_gate_run_fails() {
        let gate = ValidationGate::from_commands(vec!["false".to_string()], PathBuf::from("/tmp"))
            .unwrap();
        let bead = test_bead();
        let report = gate.run(&bead).await.unwrap();
        assert!(!report.all_passed);
    }

    #[tokio::test]
    async fn validation_gate_workspace() {
        let workspace = PathBuf::from("/test/workspace");
        let gate =
            ValidationGate::from_commands(vec!["true".to_string()], workspace.clone()).unwrap();
        assert_eq!(gate.workspace(), &workspace);
    }

    // ── truncate_output tests ──

    #[test]
    fn truncate_output_short_string() {
        let s = "hello";
        assert_eq!(truncate_output(s, 100), "hello");
    }

    #[test]
    fn truncate_output_long_string() {
        let s = "a".repeat(200);
        let result = truncate_output(&s, 50);
        assert!(result.len() < 200);
        assert!(result.ends_with("... [truncated]"));
    }

    // ── GateConfig deserialization tests ──

    #[test]
    fn gate_config_command_deserialize() {
        let yaml = r#"
            type: command
            commands:
                - cargo test
                - cargo clippy
        "#;
        let config: GateConfig = serde_yaml::from_str(yaml).unwrap();
        match config {
            GateConfig::Command {
                commands,
                stderr_cap_bytes,
                run_in: _,
            } => {
                assert_eq!(commands, vec!["cargo test", "cargo clippy"]);
                // Not set in YAML — inherits validation.stderr_cap_bytes at the
                // OutcomeHandler call site rather than baking in a default here.
                assert_eq!(stderr_cap_bytes, None);
            }
        }
    }

    #[test]
    fn gate_config_command_deserialize_with_stderr_cap_override() {
        let yaml = r#"
            type: command
            commands:
                - cargo test
            stderr_cap_bytes: 65536
        "#;
        let config: GateConfig = serde_yaml::from_str(yaml).unwrap();
        match config {
            GateConfig::Command {
                stderr_cap_bytes, ..
            } => {
                assert_eq!(stderr_cap_bytes, Some(65536));
            }
        }
    }

    // ───── Uncommitted Dependency Detection Tests ─────

    #[tokio::test]
    async fn uncommitted_dependency_detection_clean_fails_workspace_passes() {
        // Test scenario: A committed file references a symbol that only exists in an untracked file
        // Clean mode should fail (symbol not found), workspace mode should pass (symbol exists in untracked file)
        // The gate should detect this as an uncommitted dependency and return a specific error

        let temp = TempDir::new().unwrap();
        let workspace = temp.path();

        // Initialize a git repo
        git_init(workspace);
        std::fs::write(workspace.join("README.md"), "test repo\\n").unwrap();
        git_add(workspace, ".");
        git_commit(workspace, "initial commit\\n");

        // Create a committed Rust file that references a function from an untracked module
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(
            workspace.join("src/main.rs"),
            r#"
// This references a function that only exists in untracked helper.rs
mod helper;

fn main() {
    helper::uncommitted_function();
}
"#,
        )
        .unwrap();

        // Create an untracked file that defines the function
        std::fs::write(
            workspace.join("src/helper.rs"),
            r#"
pub fn uncommitted_function() {
    println!("This function is only in uncommitted files");
}
"#,
        )
        .unwrap();

        // Commit the main file but leave helper.rs untracked
        git_add(workspace, "src/main.rs");
        git_commit(workspace, "add main.rs");

        // Create a CommandGate that runs `cargo check` (which will fail in clean mode, pass in workspace)
        let gate = CommandGate::with_options(vec![CHECK_SOURCES.to_string()], 65536, RunIn::Clean);

        let bead = crate::types::Bead {
            id: "test-bead-clean-fails".into(),
            title: "Test Bead\\n".to_string(),
            body: None,
            priority: 0u8,
            status: crate::types::BeadStatus::Open,
            assignee: None,
            labels: Vec::new(),
            workspace: workspace.to_path_buf(),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        // Run validation - should detect uncommitted dependency
        let result = gate.validate(&bead, workspace).await.unwrap();

        // Should fail with uncommitted dependency message
        match result {
            GateResult::Fail(reason) => {
                assert!(
                    reason.contains("uncommitted files"),
                    "Error message should mention uncommitted files: {}",
                    reason
                );
                assert!(
                    reason.contains("src/helper.rs"),
                    "Error message should include the untracked file: {}",
                    reason
                );
            }
            GateResult::Pass => {
                panic!("Expected failure due to uncommitted dependency, but got Pass");
            }
            GateResult::ExecutionError { .. } => {
                panic!("Expected Pass or Fail, got ExecutionError")
            }
        }

        // Verify the clean extraction was cleaned up after detecting uncommitted dependency
        // (not preserved for diagnosis since we identified the specific cause).
        //
        // Match on this bead's id only. Every test in this module shares one
        // TMPDIR, and one of them (both_modes_fail) preserves its extraction on
        // purpose, so a scan for the generic "needle-clean" prefix sees a
        // sibling test's directory and fails whichever test happens to run
        // second.
        let clean_dirs = std::fs::read_dir(workspace.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                name_str.contains(bead.id.as_ref())
            });
        assert!(
            !clean_dirs,
            "Clean extraction should be cleaned up after uncommitted dependency detection"
        );
    }

    #[tokio::test]
    async fn uncommitted_dependency_detection_both_modes_pass() {
        // Test scenario: Code works correctly in both clean and workspace modes
        // Clean extraction should be removed on success

        let temp = TempDir::new().unwrap();
        let workspace = temp.path();

        // Initialize a git repo
        git_init(workspace);
        std::fs::write(workspace.join("README.md"), "test repo\\n").unwrap();
        git_add(workspace, ".");
        git_commit(workspace, "initial commit\\n");

        // Create a simple committed file that will pass `cargo check`
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(
            workspace.join("src/lib.rs"),
            r#"
/// A simple function that always works
pub fn simple_function() -> i32 {
    42
}
"#,
        )
        .unwrap();
        git_add(workspace, "src/lib.rs");
        git_commit(workspace, "add lib.rs");

        let gate = CommandGate::with_options(vec![CHECK_SOURCES.to_string()], 65536, RunIn::Clean);

        let bead = crate::types::Bead {
            id: BeadId::from("test-bead-both-pass"),
            title: "Test Bead\\n".to_string(),
            body: None,
            priority: 1,
            status: crate::types::BeadStatus::Open,
            assignee: None,
            labels: Vec::new(),
            workspace: workspace.to_path_buf(),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        // Run validation - should pass in clean mode
        let result = gate.validate(&bead, workspace).await.unwrap();
        assert_eq!(result, GateResult::Pass, "Both modes should pass");

        // Verify clean extraction was removed on success
        let clean_dirs = std::fs::read_dir(workspace.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                name_str.contains(bead.id.as_ref())
            });
        assert!(!clean_dirs, "Clean extraction should be removed on success");
    }

    #[tokio::test]
    async fn uncommitted_dependency_detection_both_modes_fail() {
        // Test scenario: Code has a real error (not related to uncommitted files)
        // Both modes should fail, and clean extraction should be preserved for diagnosis

        let temp = TempDir::new().unwrap();
        let workspace = temp.path();

        // Initialize a git repo
        git_init(workspace);
        std::fs::write(workspace.join("README.md"), "test repo\\n").unwrap();
        git_add(workspace, ".");
        git_commit(workspace, "initial commit\\n");

        // Create a committed file with a real syntax error
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(
            workspace.join("src/lib.rs"),
            r#"
// This has a syntax error that will fail in both modes
pub fn broken_function( -> i32 {
    42
}
"#,
        )
        .unwrap();
        git_add(workspace, "src/lib.rs");
        git_commit(workspace, "add broken lib.rs");

        let gate = CommandGate::with_options(vec![CHECK_SOURCES.to_string()], 65536, RunIn::Clean);

        let bead = crate::types::Bead {
            id: BeadId::from("test-bead-both-fail"),
            title: "Test Bead\\n".to_string(),
            body: None,
            priority: 1,
            status: crate::types::BeadStatus::Open,
            assignee: None,
            labels: Vec::new(),
            workspace: workspace.to_path_buf(),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        // Run validation - should fail in clean mode
        let result = gate.validate(&bead, workspace).await.unwrap();

        match result {
            GateResult::Fail(reason) => {
                assert!(
                    !reason.contains("uncommitted files"),
                    "Real error should not be attributed to uncommitted files: {}",
                    reason
                );
            }
            GateResult::Pass => {
                panic!("Expected failure due to syntax error, but got Pass");
            }
            GateResult::ExecutionError { .. } => {
                panic!("Expected Pass or Fail, got ExecutionError")
            }
        }

        // Give the test a moment to finish preserving the directory
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Verify clean extraction was preserved for diagnosis
        let clean_dirs = std::fs::read_dir(workspace.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                name_str.contains(bead.id.as_ref())
            });
        assert!(
            clean_dirs,
            "Clean extraction should be preserved for diagnosis when both modes fail"
        );

        // Clean up the preserved directory
        if let Ok(entries) = std::fs::read_dir(workspace.parent().unwrap()) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.contains(bead.id.as_ref()) {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }

    #[tokio::test]
    async fn uncommitted_dependency_detection_workspace_mode_bypasses_clean() {
        // Test scenario: When run_in is Workspace, should skip clean extraction entirely

        let temp = TempDir::new().unwrap();
        let workspace = temp.path();

        // Initialize a git repo
        git_init(workspace);
        std::fs::write(workspace.join("README.md"), "test repo\\n").unwrap();
        git_add(workspace, ".");
        git_commit(workspace, "initial commit\\n");

        let gate =
            CommandGate::with_options(vec!["echo test\\n".to_string()], 65536, RunIn::Workspace);

        let bead = crate::types::Bead {
            id: BeadId::from("test-bead-workspace-mode"),
            title: "Test Bead\\n".to_string(),
            body: None,
            priority: 1,
            status: crate::types::BeadStatus::Open,
            assignee: None,
            labels: Vec::new(),
            workspace: workspace.to_path_buf(),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        // Run validation - should run in workspace mode without clean extraction
        let result = gate.validate(&bead, workspace).await.unwrap();
        assert_eq!(result, GateResult::Pass, "Simple echo should pass");

        // Verify no clean extraction was created
        let clean_dirs = std::fs::read_dir(workspace.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                name_str.contains(bead.id.as_ref())
            });
        assert!(
            !clean_dirs,
            "Workspace mode should not create clean extraction"
        );
    }

    #[tokio::test]
    async fn uncommitted_dependency_detection_with_multiple_untracked_files() {
        // Test scenario: Multiple untracked files contribute to the uncommitted dependency
        // The error message should list all untracked files

        let temp = TempDir::new().unwrap();
        let workspace = temp.path();

        // Initialize a git repo
        git_init(workspace);
        std::fs::write(workspace.join("README.md"), "test repo\\n").unwrap();
        git_add(workspace, ".");
        git_commit(workspace, "initial commit\\n");

        // Create a committed Rust file that references multiple untracked modules
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(
            workspace.join("src/main.rs"),
            r#"
mod helper1;
mod helper2;

fn main() {
    helper1::func1();
    helper2::func2();
}
"#,
        )
        .unwrap();

        // Create two untracked files
        std::fs::write(
            workspace.join("src/helper1.rs"),
            r#"
pub fn func1() {
    println!("helper1");
}
"#,
        )
        .unwrap();

        std::fs::write(
            workspace.join("src/helper2.rs"),
            r#"
pub fn func2() {
    println!("helper2");
}
"#,
        )
        .unwrap();

        // Commit only the main file
        git_add(workspace, "src/main.rs");
        git_commit(workspace, "add main.rs");

        let gate = CommandGate::with_options(vec![CHECK_SOURCES.to_string()], 65536, RunIn::Clean);

        let bead = crate::types::Bead {
            id: BeadId::from("test-bead-multiple-untracked"),
            title: "Test Bead\\n".to_string(),
            body: None,
            priority: 1,
            status: crate::types::BeadStatus::Open,
            assignee: None,
            labels: Vec::new(),
            workspace: workspace.to_path_buf(),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        // Run validation - should detect uncommitted dependency
        let result = gate.validate(&bead, workspace).await.unwrap();

        match result {
            GateResult::Fail(reason) => {
                assert!(
                    reason.contains("uncommitted files"),
                    "Error message should mention uncommitted files: {}",
                    reason
                );
                // Should include at least one of the untracked files
                let has_helper1 = reason.contains("helper1.rs");
                let has_helper2 = reason.contains("helper2.rs");
                assert!(
                    has_helper1 || has_helper2,
                    "Error message should include untracked files: {}",
                    reason
                );
            }
            GateResult::Pass => {
                panic!("Expected failure due to uncommitted dependencies, but got Pass");
            }
            GateResult::ExecutionError { .. } => {
                panic!("Expected Pass or Fail, got ExecutionError")
            }
        }
    }

    // ───── Helper functions for test setup ─────

    fn git_init(dir: &Path) {
        std::process::Command::new("git")
            .arg("init")
            .current_dir(dir)
            .output()
            .expect("Failed to init git repo");

        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()
            .expect("Failed to set git email");

        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir)
            .output()
            .expect("Failed to set git name");
    }

    fn git_add(dir: &Path, path: &str) {
        std::process::Command::new("git")
            .args(["add", path])
            .current_dir(dir)
            .output()
            .expect("Failed to run git add");
    }

    fn git_commit(dir: &Path, message: &str) {
        std::process::Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(dir)
            .output()
            .expect("Failed to run git commit");
    }

    // ── ValidationError::InvalidKind tests ──

    #[test]
    fn validation_error_invalid_kind_display_formats_correctly() {
        let error = ValidationError::InvalidKind {
            kind: "test_kind".to_string(),
        };
        let display = format!("{}", error);
        assert_eq!(display, "invalid kind: 'test_kind'");
    }

    #[test]
    fn validation_error_invalid_kind_display_with_numeric_kind() {
        let error = ValidationError::InvalidKind {
            kind: "12345".to_string(),
        };
        let display = format!("{}", error);
        assert_eq!(display, "invalid kind: '12345'");
    }

    #[test]
    fn validation_error_invalid_kind_display_with_special_characters() {
        let error = ValidationError::InvalidKind {
            kind: "kind-with-special-chars_!@#$%".to_string(),
        };
        let display = format!("{}", error);
        assert_eq!(display, "invalid kind: 'kind-with-special-chars_!@#$%'");
    }

    #[test]
    fn validation_error_invalid_kind_display_with_empty_string() {
        let error = ValidationError::InvalidKind {
            kind: "".to_string(),
        };
        let display = format!("{}", error);
        assert_eq!(display, "invalid kind: ''");
    }

    #[test]
    fn validation_error_invalid_kind_display_with_whitespace() {
        let error = ValidationError::InvalidKind {
            kind: "kind with spaces".to_string(),
        };
        let display = format!("{}", error);
        assert_eq!(display, "invalid kind: 'kind with spaces'");
    }

    #[test]
    fn validation_error_invalid_kind_display_with_newlines() {
        let error = ValidationError::InvalidKind {
            kind: "kind\nwith\nnewlines".to_string(),
        };
        let display = format!("{}", error);
        assert_eq!(display, "invalid kind: 'kind\nwith\nnewlines'");
    }

    #[test]
    fn validation_error_invalid_kind_display_with_unicode() {
        let error = ValidationError::InvalidKind {
            kind: "kind-日本語-🎯".to_string(),
        };
        let display = format!("{}", error);
        assert_eq!(display, "invalid kind: 'kind-日本語-🎯'");
    }

    #[test]
    fn validation_error_invalid_kind_implements_error_trait() {
        let error = ValidationError::InvalidKind {
            kind: "test_kind".to_string(),
        };
        // Verify it can be used as an error source
        let error_dyn: &dyn std::error::Error = &error;
        assert_eq!(error_dyn.to_string(), "invalid kind: 'test_kind'");
    }

    #[test]
    fn validation_error_invalid_kind_error_source_is_none() {
        let error = ValidationError::InvalidKind {
            kind: "test_kind".to_string(),
        };
        let error_dyn: &dyn std::error::Error = &error;
        // ValidationError::InvalidKind has no underlying source
        assert!(error_dyn.source().is_none());
    }

    #[test]
    fn validation_error_invalid_kind_equality_same_kind() {
        let error1 = ValidationError::InvalidKind {
            kind: "same_kind".to_string(),
        };
        let error2 = ValidationError::InvalidKind {
            kind: "same_kind".to_string(),
        };
        assert_eq!(error1, error2);
    }

    #[test]
    fn validation_error_invalid_kind_inequality_different_kind() {
        let error1 = ValidationError::InvalidKind {
            kind: "kind_a".to_string(),
        };
        let error2 = ValidationError::InvalidKind {
            kind: "kind_b".to_string(),
        };
        assert_ne!(error1, error2);
    }

    #[test]
    fn validation_error_invalid_kind_clone() {
        let error = ValidationError::InvalidKind {
            kind: "clone_test".to_string(),
        };
        let cloned = error.clone();
        assert_eq!(error, cloned);
        assert_eq!(format!("{}", error), format!("{}", cloned));
    }

    #[test]
    fn validation_error_invalid_kind_debug_format() {
        let error = ValidationError::InvalidKind {
            kind: "debug_test".to_string(),
        };
        let debug = format!("{:?}", error);
        assert!(debug.contains("InvalidKind"));
        assert!(debug.contains("debug_test"));
    }

    #[test]
    fn validation_error_invalid_kind_very_long_string() {
        let long_kind = "a".repeat(10000);
        let error = ValidationError::InvalidKind {
            kind: long_kind.clone(),
        };
        let display = format!("{}", error);
        assert_eq!(display, format!("invalid kind: '{}'", long_kind));
    }

    #[test]
    fn validation_error_invalid_kind_kind_field_accessible() {
        let error = ValidationError::InvalidKind {
            kind: "accessible_kind".to_string(),
        };
        match error {
            ValidationError::InvalidKind { kind } => {
                assert_eq!(kind, "accessible_kind");
            }
        }
    }
}
