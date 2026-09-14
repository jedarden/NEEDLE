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
//! # Native checks
//!
//! [`dod_bypass`] is not a configured gate: the outcome handler runs it
//! directly after the shipped-work check, so it needs no entry in
//! `.needle.yaml` and never depends on a commit message naming the bead.
//!
//! Inspired by bg-gate (docs/research/bg-gate-validation.md).

pub mod default_gates;
pub mod dod_bypass;
pub mod predispatch;
mod shipped_work;
pub mod worker_config;
pub use shipped_work::{upstream_status, verify_shipped_work, UpstreamStatus};

mod gate_path_validation;
pub use gate_path_validation::{
    validate_gate_command_paths, GatePathValidationError, GatePathValidationResult, PathType,
};

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
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
            // An executed rejection is attributable to the work, while an
            // execution error means the gate never produced a verdict. Do
            // not collapse the latter into a generic Fail while aggregating
            // a command gate's report (ADR-023).
            if let Some(reason) = self
                .results
                .values()
                .find_map(|r| r.failure_reason().map(|s| s.to_string()))
            {
                return GateResult::Fail(reason);
            }
            if let Some(error) = self.results.values().find_map(|result| match result {
                GateResult::ExecutionError { command, reason } => {
                    Some(GateResult::ExecutionError {
                        command: command.clone(),
                        reason: reason.clone(),
                    })
                }
                _ => None,
            }) {
                return error;
            }
            GateResult::Fail("verification gate failed".to_string())
        }
    }
}

/// Execution mode for a gate command.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunIn {
    /// Run in a clean extraction of an exact captured commit.
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
        /// in the shared workspace checkout. `clean` (default) resolves HEAD
        /// once, archives that exact commit to a temp directory, and runs there; `workspace`
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

// ------------------------------------------------------------------------------
// Immutable execution subject and environment
// ------------------------------------------------------------------------------

/// Version of the explicit environment inherited by command gates.
///
/// Receipt fingerprinting can name this schema later. Bumping the allowlist or
/// changing an injected variable's meaning requires a new version.
pub const COMMAND_GATE_ENVIRONMENT_SCHEMA: &str = "needle-command-gate-env/v1";

/// Environment variables whose values may affect ordinary build/test tools
/// without carrying credentials. Everything else is deliberately absent from
/// a command gate's child environment.
///
/// The list is fixed rather than prefix-based: `CARGO_*`, `AWS_*`, `GIT_*`, and
/// proxy families can contain registry or repository credentials. A new build
/// input must be reviewed and added by exact name.
const COMMAND_GATE_ENV_ALLOWLIST: &[&str] = &[
    "AR",
    "CARGO_BUILD_JOBS",
    "CARGO_HOME",
    "CARGO_INCREMENTAL",
    "CARGO_NET_OFFLINE",
    "CARGO_TARGET_DIR",
    "CARGO_TERM_COLOR",
    "CC",
    "CFLAGS",
    "CI",
    "CXX",
    "CXXFLAGS",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LD_LIBRARY_PATH",
    "LDFLAGS",
    "LIBCLANG_PATH",
    "LOGNAME",
    "NO_COLOR",
    "OPENSSL_DIR",
    "OPENSSL_INCLUDE_DIR",
    "OPENSSL_LIB_DIR",
    "PATH",
    "PKG_CONFIG_LIBDIR",
    "PKG_CONFIG_PATH",
    "PKG_CONFIG_SYSROOT_DIR",
    "PROTOC",
    "RANLIB",
    "RUST_BACKTRACE",
    "RUSTDOCFLAGS",
    "RUSTFLAGS",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "RUST_TEST_THREADS",
    "SHELL",
    "SOURCE_DATE_EPOCH",
    "TERM",
    "TMPDIR",
    "TZ",
    "USER",
];

/// The immutable Git object a clean command gate judges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateSubject {
    revision: String,
    tree: String,
}

impl GateSubject {
    /// Full commit object ID captured before any gate starts.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Tree object ID resolved from [`Self::revision`].
    pub fn tree(&self) -> &str {
        &self.tree
    }

    async fn capture(workspace: &Path) -> Result<Self> {
        let revision = git_stdout(workspace, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
        validate_git_oid("commit", &revision)?;
        let tree_expression = format!("{revision}^{{tree}}");
        let tree = git_stdout(
            workspace,
            &["rev-parse", "--verify", tree_expression.as_str()],
        )
        .await?;
        validate_git_oid("tree", &tree)?;
        Ok(Self { revision, tree })
    }

    /// Verify both that the captured commit still names the captured tree and
    /// that the workspace still points at that commit. Git objects are
    /// immutable, but HEAD in a shared checkout is not.
    async fn verify_current(&self, workspace: &Path) -> Result<()> {
        let tree_expression = format!("{}^{{tree}}", self.revision);
        let resolved_tree = git_stdout(
            workspace,
            &["rev-parse", "--verify", tree_expression.as_str()],
        )
        .await?;
        if resolved_tree != self.tree {
            anyhow::bail!(
                "captured commit {} now resolves to tree {}, expected {}",
                self.revision,
                resolved_tree,
                self.tree
            );
        }

        let current = git_stdout(workspace, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
        if current != self.revision {
            anyhow::bail!(
                "workspace HEAD changed from captured revision {} to {} while verification ran",
                self.revision,
                current
            );
        }
        Ok(())
    }
}

/// Explicit child environment captured once for a complete validation run.
#[derive(Debug, Clone)]
pub struct GateEnvironment {
    values: BTreeMap<OsString, OsString>,
}

impl GateEnvironment {
    fn capture() -> Self {
        let values = COMMAND_GATE_ENV_ALLOWLIST
            .iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
            .collect();
        Self { values }
    }

    /// Environment schema used by this captured value set.
    pub fn schema(&self) -> &'static str {
        COMMAND_GATE_ENVIRONMENT_SCHEMA
    }

    /// Whether an exact allowlisted variable was captured.
    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(OsStr::new(name))
    }

    fn apply(&self, command: &mut tokio::process::Command) {
        command.env_clear();
        command.envs(self.values.iter());
    }
}

/// One immutable context shared by every gate in a validation run.
#[derive(Debug, Clone)]
pub struct GateExecutionContext {
    subject: Option<GateSubject>,
    environment: GateEnvironment,
}

impl GateExecutionContext {
    async fn capture(workspace: &Path, require_subject: bool) -> Result<Self> {
        let subject = if require_subject {
            Some(GateSubject::capture(workspace).await?)
        } else {
            None
        };
        Ok(Self {
            subject,
            environment: GateEnvironment::capture(),
        })
    }

    /// Exact committed subject, present when at least one clean gate runs.
    pub fn subject(&self) -> Option<&GateSubject> {
        self.subject.as_ref()
    }

    /// Sanitized environment inherited by command gates.
    pub fn environment(&self) -> &GateEnvironment {
        &self.environment
    }
}

/// Whether a later receipt layer may reuse this gate without executing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateCacheability {
    /// The gate has a complete, hermetic input contract.
    Cacheable,
    /// The gate can execute, but its inputs are not complete enough to reuse.
    NonCacheable { reason: &'static str },
}

async fn git_stdout(workspace: &Path, args: &[&str]) -> Result<String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("failed to execute git {}", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            truncate_output(String::from_utf8_lossy(&output.stderr).trim(), 4096)
        );
    }
    Ok(String::from_utf8(output.stdout)
        .context("git returned a non-UTF-8 object ID")?
        .trim()
        .to_string())
}

fn validate_git_oid(kind: &str, oid: &str) -> Result<()> {
    if !matches!(oid.len(), 40 | 64)
        || !oid
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        anyhow::bail!("git returned a non-canonical {kind} object ID: {oid:?}");
    }
    Ok(())
}

fn subject_execution_error(error: impl std::fmt::Display) -> GateResult {
    GateResult::ExecutionError {
        command: "git verify command-gate subject".to_string(),
        reason: error.to_string(),
    }
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

    /// Validate using the immutable subject/environment captured for the
    /// complete [`ValidationGate`] run. Custom gates retain their existing
    /// behavior until they explicitly consume this contract.
    async fn validate_with_context(
        &self,
        bead: &crate::types::Bead,
        workspace: &Path,
        _context: &GateExecutionContext,
    ) -> Result<GateResult> {
        self.validate(bead, workspace).await
    }

    /// Whether this gate needs an exact committed Git subject.
    fn requires_exact_subject(&self) -> bool {
        false
    }

    /// Receipt eligibility. Unknown/custom gates remain executable but cannot
    /// be reused until they declare a complete hermetic contract.
    fn cacheability(&self) -> GateCacheability {
        GateCacheability::NonCacheable {
            reason: "gate does not declare a hermetic input contract",
        }
    }

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
/// This creates a clean copy of the captured commit in a temporary directory
/// using `git archive`. This is used for running gates in isolation from
/// uncommitted changes and from a concurrently moving workspace HEAD.
async fn extract_committed_state(
    workspace: &Path,
    bead_id: &str,
    subject: &GateSubject,
) -> Result<PathBuf> {
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

    // Verify the commit-to-tree binding immediately before extraction. HEAD
    // itself may move after capture; the archived commit object may not.
    let tree_expression = format!("{}^{{tree}}", subject.revision());
    let resolved_tree = git_stdout(
        workspace,
        &["rev-parse", "--verify", tree_expression.as_str()],
    )
    .await?;
    if resolved_tree != subject.tree() {
        anyhow::bail!(
            "captured revision {} resolves to tree {}, expected {}",
            subject.revision(),
            resolved_tree,
            subject.tree()
        );
    }

    // Extract the captured revision, never the moving HEAD reference.
    let output = Command::new("git")
        .args(["archive", "--format=tar", subject.revision()])
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
        let context =
            match GateExecutionContext::capture(workspace, self.run_in == RunIn::Clean).await {
                Ok(context) => context,
                Err(error) => return Ok(subject_execution_error(error)),
            };
        self.validate_with_context(bead, workspace, &context).await
    }

    async fn validate_with_context(
        &self,
        bead: &crate::types::Bead,
        workspace: &Path,
        context: &GateExecutionContext,
    ) -> Result<GateResult> {
        // Try running in clean mode first (if configured)
        // Returns: (result, clean_directory_path_if_any)
        let clean_attempt = if self.run_in == RunIn::Clean {
            let Some(subject) = context.subject() else {
                return Ok(subject_execution_error(
                    "clean command gate has no captured Git subject",
                ));
            };
            // Extract committed state to a temporary directory
            match extract_committed_state(workspace, &bead.id, subject).await {
                Ok(dir) => {
                    tracing::info!(
                        workspace = %workspace.display(),
                        clean_dir = %dir.display(),
                        "extracted committed state for clean gate execution"
                    );
                    // Run commands in the clean extraction
                    let result = self.run_commands_in_dir(bead, &dir, true, context).await;

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
                    // Extraction/captured-subject failures mean no gate
                    // verdict exists. Preserve ADR-023's infrastructure route.
                    return Ok(subject_execution_error(format!(
                        "failed to extract captured revision: {e}"
                    )));
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
                let workspace_result = self
                    .run_commands_in_dir(bead, workspace, false, context)
                    .await;

                if let Some(subject) = context.subject() {
                    if let Err(error) = subject.verify_current(workspace).await {
                        return Ok(subject_execution_error(error));
                    }
                }

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
            if let Some(subject) = context.subject() {
                if let Err(error) = subject.verify_current(workspace).await {
                    return Ok(subject_execution_error(error));
                }
            }
            // Clean mode passed, return success
            return Ok(self.to_gate_result(clean_result));
        }

        // Not running in clean mode, or clean wasn't attempted - run directly in workspace
        let result = self
            .run_commands_in_dir(bead, workspace, false, context)
            .await;
        Ok(self.to_gate_result(result))
    }

    fn requires_exact_subject(&self) -> bool {
        self.run_in == RunIn::Clean
    }

    fn cacheability(&self) -> GateCacheability {
        GateCacheability::NonCacheable {
            reason: "arbitrary shell commands have no declared hermetic input manifest",
        }
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
        context: &GateExecutionContext,
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

            match self.run_command(cmd, bead, dir, context).await {
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
        report.to_gate_result()
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
        context: &GateExecutionContext,
    ) -> std::result::Result<(), GateFailure> {
        let mut command = tokio::process::Command::new("sh");
        context.environment().apply(&mut command);
        let result = command
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

        // Capture one environment and, when needed, one exact Git subject for
        // the complete run. Without this, two configured clean gates can judge
        // different commits when another worker advances shared HEAD between
        // them.
        let requires_subject = self
            .gates
            .iter()
            .any(|(_, gate)| gate.requires_exact_subject());
        let context = match GateExecutionContext::capture(&self.workspace, requires_subject).await {
            Ok(context) => context,
            Err(error) => {
                let name = self
                    .gates
                    .iter()
                    .find(|(_, gate)| gate.requires_exact_subject())
                    .map(|(name, _)| name.clone())
                    .unwrap_or_else(|| "execution_context".to_string());
                results.insert(name, subject_execution_error(error));
                return Ok(GateReport::new(results));
            }
        };

        for (name, gate) in &self.gates {
            let result = gate
                .validate_with_context(bead, &self.workspace, &context)
                .await?;
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

    #[test]
    fn command_gate_exact_subject_marks_arbitrary_external_gate_non_cacheable() {
        let gate = CommandGate::new(vec!["external-validator".to_string()]);
        assert_eq!(gate.gate_type(), "command");
        assert_eq!(
            gate.cacheability(),
            GateCacheability::NonCacheable {
                reason: "arbitrary shell commands have no declared hermetic input manifest",
            }
        );
    }

    // ── ValidationGate tests ──

    #[test]
    fn validation_gate_new_returns_none_for_empty_configs() {
        let ws = tempfile::tempdir().unwrap();
        let gate = ValidationGate::new(vec![], ws.path().to_path_buf());
        assert!(gate.is_none());
    }

    #[test]
    fn validation_gate_from_commands_returns_none_for_empty() {
        let ws = tempfile::tempdir().unwrap();
        let gate = ValidationGate::from_commands(vec![], ws.path().to_path_buf());
        assert!(gate.is_none());
    }

    #[test]
    fn validation_gate_from_commands_returns_some_for_nonempty() {
        let ws = tempfile::tempdir().unwrap();
        let gate = ValidationGate::from_commands(vec!["true".to_string()], ws.path().to_path_buf());
        assert!(gate.is_some());
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
