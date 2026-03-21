//! Hierarchical configuration loading and validation.
//!
//! Resolution order (later layers override earlier):
//! 1. Built-in defaults
//! 2. Global config file (`~/.config/needle/config.yaml`)
//! 3. Workspace config file (`.needle.yaml`)
//! 4. Environment variables (`NEEDLE_*`)
//! 5. CLI arguments (highest precedence)
//!
//! Config is loaded once at boot and never reloaded.
//!
//! Leaf module — depends only on `types`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cost::{BudgetConfig, PricingConfig};
use crate::types::{IdentifierScheme, IdleAction};

// ──────────────────────────────────────────────────────────────────────────────
// Sub-structs
// ──────────────────────────────────────────────────────────────────────────────

/// Agent (AI model CLI) configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Name or path of the default agent adapter (e.g., `claude`).
    #[serde(default = "AgentConfig::default_agent")]
    pub default: String,

    /// Extra arguments to pass before the prompt.
    #[serde(default)]
    pub args: Vec<String>,

    /// Agent process timeout in seconds (0 = unlimited).
    #[serde(default = "AgentConfig::default_timeout")]
    pub timeout: u64,

    /// Directory containing adapter TOML files.
    #[serde(default = "AgentConfig::default_adapters_dir")]
    pub adapters_dir: PathBuf,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            default: Self::default_agent(),
            args: Vec::new(),
            timeout: Self::default_timeout(),
            adapters_dir: Self::default_adapters_dir(),
        }
    }
}

impl AgentConfig {
    fn default_agent() -> String {
        "claude".to_string()
    }
    fn default_timeout() -> u64 {
        3600
    }
    fn default_adapters_dir() -> PathBuf {
        dirs_or_home(".config/needle/adapters")
    }
}

/// Worker fleet configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConfig {
    /// Maximum number of concurrent workers.
    #[serde(default = "WorkerConfig::default_max_workers")]
    pub max_workers: u32,

    /// Stagger delay (seconds) between worker launches.
    #[serde(default = "WorkerConfig::default_launch_stagger_seconds")]
    pub launch_stagger_seconds: u64,

    /// Seconds to wait between queue polls when idle.
    #[serde(default = "WorkerConfig::default_idle_timeout")]
    pub idle_timeout: u64,

    /// What to do when the queue is empty.
    #[serde(default)]
    pub idle_action: IdleAction,

    /// Maximum claim retries before skipping a bead.
    #[serde(default = "WorkerConfig::default_max_claim_retries")]
    pub max_claim_retries: u32,

    /// How workers generate their unique names.
    #[serde(default)]
    pub identifier_scheme: IdentifierScheme,

    /// Warn when CPU load (0.0–1.0) exceeds this threshold.
    #[serde(default = "WorkerConfig::default_cpu_load_warn")]
    pub cpu_load_warn: f64,

    /// Warn when available memory falls below this threshold (MB).
    #[serde(default = "WorkerConfig::default_memory_free_warn_mb")]
    pub memory_free_warn_mb: u64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        WorkerConfig {
            max_workers: Self::default_max_workers(),
            launch_stagger_seconds: Self::default_launch_stagger_seconds(),
            idle_timeout: Self::default_idle_timeout(),
            idle_action: IdleAction::default(),
            max_claim_retries: Self::default_max_claim_retries(),
            identifier_scheme: IdentifierScheme::default(),
            cpu_load_warn: Self::default_cpu_load_warn(),
            memory_free_warn_mb: Self::default_memory_free_warn_mb(),
        }
    }
}

impl WorkerConfig {
    fn default_max_workers() -> u32 {
        4
    }
    fn default_launch_stagger_seconds() -> u64 {
        2
    }
    fn default_idle_timeout() -> u64 {
        60
    }
    fn default_max_claim_retries() -> u32 {
        3
    }
    fn default_cpu_load_warn() -> f64 {
        0.8
    }
    fn default_memory_free_warn_mb() -> u64 {
        512
    }
}

/// Workspace path configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Default workspace directory (used when not specified on CLI).
    #[serde(default = "WorkspaceConfig::default_workspace")]
    pub default: PathBuf,

    /// NEEDLE home directory (heartbeat files, log output).
    #[serde(default = "WorkspaceConfig::default_home")]
    pub home: PathBuf,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        WorkspaceConfig {
            default: Self::default_workspace(),
            home: Self::default_home(),
        }
    }
}

impl WorkspaceConfig {
    fn default_workspace() -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
    fn default_home() -> PathBuf {
        dirs_or_home(".needle")
    }
}

/// Pluck strand configuration (primary bead selection).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluckConfig {
    /// Labels to exclude from selection.
    #[serde(default)]
    pub exclude_labels: Vec<String>,
}

/// Mend strand configuration (stuck/failed bead recovery).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MendConfig {
    /// Beads stuck in_progress longer than this (seconds) are candidates.
    #[serde(default = "MendConfig::default_stuck_threshold_secs")]
    pub stuck_threshold_secs: u64,

    /// Lock files older than this (seconds) are considered orphaned.
    #[serde(default = "MendConfig::default_lock_ttl_secs")]
    pub lock_ttl_secs: u64,

    /// Run `br doctor` after every N beads processed (0 = disabled).
    #[serde(default = "MendConfig::default_db_check_interval")]
    pub db_check_interval: u64,
}

impl Default for MendConfig {
    fn default() -> Self {
        MendConfig {
            stuck_threshold_secs: Self::default_stuck_threshold_secs(),
            lock_ttl_secs: Self::default_lock_ttl_secs(),
            db_check_interval: Self::default_db_check_interval(),
        }
    }
}

impl MendConfig {
    fn default_stuck_threshold_secs() -> u64 {
        300
    }
    fn default_lock_ttl_secs() -> u64 {
        600
    }
    fn default_db_check_interval() -> u64 {
        50
    }
}

/// Explore strand configuration (multi-workspace discovery).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreConfig {
    /// Whether the Explore strand is enabled.
    #[serde(default = "ExploreConfig::default_enabled")]
    pub enabled: bool,

    /// Explicit workspace paths to search for beads.
    /// No filesystem scanning — only these paths are checked.
    #[serde(default)]
    pub workspaces: Vec<PathBuf>,
}

impl Default for ExploreConfig {
    fn default() -> Self {
        ExploreConfig {
            enabled: Self::default_enabled(),
            workspaces: Vec::new(),
        }
    }
}

impl ExploreConfig {
    fn default_enabled() -> bool {
        true
    }
}

/// Knot strand configuration (exhaustion alerting).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnotConfig {
    /// Alert destination (e.g., webhook URL).
    #[serde(default)]
    pub alert_destination: Option<String>,

    /// Minimum minutes between alert beads for the same workspace.
    #[serde(default = "KnotConfig::default_alert_cooldown_minutes")]
    pub alert_cooldown_minutes: u64,

    /// Number of consecutive exhaustion cycles before creating an alert bead.
    #[serde(default = "KnotConfig::default_exhaustion_threshold")]
    pub exhaustion_threshold: u64,
}

impl Default for KnotConfig {
    fn default() -> Self {
        KnotConfig {
            alert_destination: None,
            alert_cooldown_minutes: Self::default_alert_cooldown_minutes(),
            exhaustion_threshold: Self::default_exhaustion_threshold(),
        }
    }
}

impl KnotConfig {
    fn default_alert_cooldown_minutes() -> u64 {
        60
    }
    fn default_exhaustion_threshold() -> u64 {
        3
    }
}

/// Mitosis configuration (bead splitting on failure).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MitosisConfig {
    /// Whether mitosis is enabled for this workspace.
    #[serde(default = "MitosisConfig::default_enabled")]
    pub enabled: bool,

    /// Only evaluate on first failure, not retries.
    #[serde(default = "MitosisConfig::default_first_failure_only")]
    pub first_failure_only: bool,
}

impl Default for MitosisConfig {
    fn default() -> Self {
        MitosisConfig {
            enabled: Self::default_enabled(),
            first_failure_only: Self::default_first_failure_only(),
        }
    }
}

impl MitosisConfig {
    fn default_enabled() -> bool {
        true
    }
    fn default_first_failure_only() -> bool {
        true
    }
}

/// Unravel strand configuration (alternative proposals for human-blocked beads).
///
/// Unravel proposes automated alternatives for beads labeled "human".
/// Child beads are created as alternatives; the original is never modified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnravelConfig {
    /// Whether the Unravel strand is enabled (opt-in, default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Maximum human-labeled beads to analyze per run (default: 5).
    #[serde(default = "UnravelConfig::default_max_beads_per_run")]
    pub max_beads_per_run: u32,

    /// Maximum alternative children per original bead (default: 3).
    #[serde(default = "UnravelConfig::default_max_alternatives_per_bead")]
    pub max_alternatives_per_bead: u32,

    /// Minimum hours between re-analysis of the same bead (default: 168 = 7 days).
    #[serde(default = "UnravelConfig::default_cooldown_hours")]
    pub cooldown_hours: u64,

    /// Custom prompt template for the alternative-proposal agent invocation.
    ///
    /// Template variables: `{id}`, `{title}`, `{body}`, `{labels}`.
    /// When `None`, the built-in template is used.
    #[serde(default)]
    pub prompt_template: Option<String>,
}

impl Default for UnravelConfig {
    fn default() -> Self {
        UnravelConfig {
            enabled: false,
            max_beads_per_run: Self::default_max_beads_per_run(),
            max_alternatives_per_bead: Self::default_max_alternatives_per_bead(),
            cooldown_hours: Self::default_cooldown_hours(),
            prompt_template: None,
        }
    }
}

impl UnravelConfig {
    fn default_max_beads_per_run() -> u32 {
        5
    }
    fn default_max_alternatives_per_bead() -> u32 {
        3
    }
    fn default_cooldown_hours() -> u64 {
        168
    }
}

/// Weave strand configuration (gap analysis and bead creation).
///
/// Weave analyzes workspace documentation for gaps and creates beads to
/// address them. Heavily guardrailed to prevent infinite work creation loops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeaveConfig {
    /// Whether the Weave strand is enabled (opt-in, default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Maximum beads to create per weave run (default: 5).
    #[serde(default = "WeaveConfig::default_max_beads_per_run")]
    pub max_beads_per_run: u32,

    /// Minimum hours between weave runs per workspace (default: 24).
    #[serde(default = "WeaveConfig::default_cooldown_hours")]
    pub cooldown_hours: u64,

    /// Workspaces where weave is forbidden (default: []).
    #[serde(default)]
    pub exclude_workspaces: Vec<PathBuf>,

    /// Glob patterns for documentation files to analyze.
    #[serde(default = "WeaveConfig::default_doc_patterns")]
    pub doc_patterns: Vec<String>,
}

impl Default for WeaveConfig {
    fn default() -> Self {
        WeaveConfig {
            enabled: false,
            max_beads_per_run: Self::default_max_beads_per_run(),
            cooldown_hours: Self::default_cooldown_hours(),
            exclude_workspaces: Vec::new(),
            doc_patterns: Self::default_doc_patterns(),
        }
    }
}

impl WeaveConfig {
    fn default_max_beads_per_run() -> u32 {
        5
    }
    fn default_cooldown_hours() -> u64 {
        24
    }
    fn default_doc_patterns() -> Vec<String> {
        vec![
            "README*".to_string(),
            "AGENTS.md".to_string(),
            "docs/**/*".to_string(),
        ]
    }
}

/// Strand waterfall configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StrandsConfig {
    #[serde(default)]
    pub pluck: PluckConfig,
    #[serde(default)]
    pub mend: MendConfig,
    #[serde(default)]
    pub explore: ExploreConfig,
    #[serde(default)]
    pub knot: KnotConfig,
    #[serde(default)]
    pub mitosis: MitosisConfig,
    #[serde(default)]
    pub weave: WeaveConfig,
    #[serde(default)]
    pub unravel: UnravelConfig,
}

/// File sink configuration for telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSinkConfig {
    /// Enable or disable the file sink.
    #[serde(default = "FileSinkConfig::default_enabled")]
    pub enabled: bool,

    /// Directory for log files (defaults to `workspace.home/logs`).
    #[serde(default)]
    pub log_dir: Option<PathBuf>,
}

impl Default for FileSinkConfig {
    fn default() -> Self {
        FileSinkConfig {
            enabled: Self::default_enabled(),
            log_dir: None,
        }
    }
}

impl FileSinkConfig {
    fn default_enabled() -> bool {
        true
    }
}

/// Stdout sink verbosity level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StdoutFormat {
    /// One-line summary: time, worker, event type only.
    Minimal,
    /// Default: time, worker, event type, bead ID, brief details.
    #[default]
    Normal,
    /// Full details including data payload.
    Verbose,
}

/// Stdout sink color mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    /// Auto-detect from terminal (isatty).
    #[default]
    Auto,
    /// Always emit ANSI color codes.
    Always,
    /// Never emit color codes.
    Never,
}

/// Stdout sink configuration for human-readable telemetry output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StdoutSinkConfig {
    /// Enable or disable the stdout sink.
    #[serde(default)]
    pub enabled: bool,

    /// Verbosity: minimal, normal, verbose.
    #[serde(default)]
    pub format: StdoutFormat,

    /// Color mode: auto, always, never.
    #[serde(default)]
    pub color: ColorMode,
}

impl Default for StdoutSinkConfig {
    fn default() -> Self {
        StdoutSinkConfig {
            enabled: false,
            format: StdoutFormat::Normal,
            color: ColorMode::Auto,
        }
    }
}

/// A single hook definition: an event filter glob and a shell command.
///
/// Events whose `event_type` matches the glob are piped as JSON to the
/// command's stdin. Hooks are fire-and-forget — failures are logged but
/// never block the worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    /// Glob pattern matched against `event_type` (e.g. `"outcome.*"`).
    pub event_filter: String,

    /// Shell command to execute. The event JSON is written to stdin.
    pub command: String,
}

/// Telemetry configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub file_sink: FileSinkConfig,
    #[serde(default)]
    pub stdout_sink: StdoutSinkConfig,
    /// Optional hook sinks — dispatch matching events to external commands.
    #[serde(default)]
    pub hooks: Vec<HookConfig>,
}

/// Health monitoring configuration (heartbeat, peer detection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    /// How often to emit a heartbeat file (seconds).
    #[serde(default = "HealthConfig::default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,

    /// Time after which a heartbeat is considered stale (seconds).
    #[serde(default = "HealthConfig::default_heartbeat_ttl_secs")]
    pub heartbeat_ttl_secs: u64,
}

impl Default for HealthConfig {
    fn default() -> Self {
        HealthConfig {
            heartbeat_interval_secs: Self::default_heartbeat_interval_secs(),
            heartbeat_ttl_secs: Self::default_heartbeat_ttl_secs(),
        }
    }
}

impl HealthConfig {
    fn default_heartbeat_interval_secs() -> u64 {
        30
    }
    fn default_heartbeat_ttl_secs() -> u64 {
        300
    }
}

/// Per-provider concurrency and rate limits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderLimits {
    /// Maximum concurrent workers dispatching to this provider.
    #[serde(default)]
    pub max_concurrent: Option<u32>,
    /// Maximum requests per minute (token bucket capacity).
    #[serde(default)]
    pub requests_per_minute: Option<u32>,
}

/// Per-model concurrency limits (overrides provider-level).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelLimits {
    /// Maximum concurrent workers dispatching to this model.
    #[serde(default)]
    pub max_concurrent: Option<u32>,
}

/// Provider/model concurrency and rate limiting configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LimitsConfig {
    /// Per-provider limits keyed by provider name (e.g., `anthropic`, `openai`).
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderLimits>,
    /// Per-model limits keyed by model name (e.g., `claude-opus`).
    #[serde(default)]
    pub models: BTreeMap<String, ModelLimits>,
}

/// Prompt construction configuration.
///
/// Loaded from the `prompt` section of workspace config (`.needle.yaml`).
/// Templates can be overridden per-workspace or globally.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptConfig {
    /// Paths to context files read from the workspace and included in prompts.
    #[serde(default)]
    pub context_files: Vec<PathBuf>,

    /// Free-form instructions appended to every prompt.
    #[serde(default)]
    pub instructions: Option<String>,

    /// Named template overrides. Keys are template names (e.g., `"pluck"`,
    /// `"mitosis"`, `"weave"`, `"unravel"`, `"pulse"`). Only the templates
    /// specified here are overridden; others use built-in defaults.
    #[serde(default)]
    pub templates: std::collections::BTreeMap<String, String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Config Source Tracking
// ──────────────────────────────────────────────────────────────────────────────

/// Where a configuration value originated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Built-in default value.
    Default,
    /// Global config file (`~/.config/needle/config.yaml`).
    GlobalFile(PathBuf),
    /// Workspace config file (`.needle.yaml`).
    WorkspaceFile(PathBuf),
    /// Environment variable override.
    EnvVar(String),
    /// CLI argument override.
    CliOverride,
}

impl std::fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigSource::Default => write!(f, "built-in default"),
            ConfigSource::GlobalFile(p) => write!(f, "{}", p.display()),
            ConfigSource::WorkspaceFile(p) => write!(f, "{}", p.display()),
            ConfigSource::EnvVar(name) => write!(f, "{} env var", name),
            ConfigSource::CliOverride => write!(f, "CLI argument"),
        }
    }
}

/// Map of config field paths to their source.
///
/// Used by `needle config --dump --show-source` to annotate each value.
pub type SourceMap = BTreeMap<String, ConfigSource>;

// ──────────────────────────────────────────────────────────────────────────────
// Workspace Overrides
// ──────────────────────────────────────────────────────────────────────────────

/// Subset of configuration that can be overridden per-workspace via `.needle.yaml`.
///
/// Only these sections are allowed at the workspace level:
/// - `agent.default`, `agent.timeout`
/// - `strands` (weave, pulse, unravel)
/// - `prompt.*`
///
/// Non-overridable sections (worker, limits, health, telemetry) are detected
/// and produce warnings.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkspaceOverrides {
    #[serde(default)]
    pub agent: Option<WorkspaceAgentOverrides>,
    #[serde(default)]
    pub strands: Option<WorkspaceStrandsOverrides>,
    #[serde(default)]
    pub prompt: Option<PromptConfig>,
    /// Verification commands run after agent success, before accepting closure.
    #[serde(default)]
    pub verification: Option<Vec<String>>,
}

/// Agent fields overridable at the workspace level.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceAgentOverrides {
    pub default: Option<String>,
    pub timeout: Option<u64>,
}

/// Strand fields overridable at the workspace level.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkspaceStrandsOverrides {
    #[serde(default)]
    pub weave: Option<serde_yaml::Value>,
    #[serde(default)]
    pub pulse: Option<serde_yaml::Value>,
    #[serde(default)]
    pub unravel: Option<serde_yaml::Value>,
}

/// Non-overridable top-level keys in workspace config.
const NON_OVERRIDABLE_KEYS: &[&str] = &["worker", "limits", "health", "telemetry", "workspace"];

// ──────────────────────────────────────────────────────────────────────────────
// Top-level Config
// ──────────────────────────────────────────────────────────────────────────────

/// Fully resolved NEEDLE configuration.
///
/// Loaded once at boot, immutable during a session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub worker: WorkerConfig,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub strands: StrandsConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    pub prompt: PromptConfig,
    #[serde(default)]
    pub health: HealthConfig,
    /// Provider/model concurrency and rate limits.
    #[serde(default)]
    pub limits: LimitsConfig,
    /// Per-model token pricing (USD per million tokens).
    #[serde(default = "crate::cost::default_pricing")]
    pub pricing: PricingConfig,
    /// Daily budget thresholds for cost enforcement.
    #[serde(default)]
    pub budget: BudgetConfig,
    /// Verification commands run after agent success, before accepting closure.
    #[serde(default)]
    pub verification: Vec<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Config validation
// ──────────────────────────────────────────────────────────────────────────────

/// A single config validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// Dot-separated field path (e.g., `agent.default`).
    pub field: String,
    /// Human-readable explanation.
    pub message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// CLI overrides
// ──────────────────────────────────────────────────────────────────────────────

/// CLI-level overrides applied after all file-based config loading.
#[derive(Debug, Default)]
pub struct CliOverrides {
    pub workspace: Option<PathBuf>,
    pub worker_name: Option<String>,
    pub agent_binary: Option<String>,
    pub max_workers: Option<u32>,
}

// ──────────────────────────────────────────────────────────────────────────────
// ConfigLoader
// ──────────────────────────────────────────────────────────────────────────────

/// Loads and validates NEEDLE configuration.
pub struct ConfigLoader;

impl ConfigLoader {
    /// Load global config from `~/.config/needle/config.yaml`.
    ///
    /// If the file does not exist, returns the default config (not an error).
    pub fn load_global() -> Result<Config> {
        let path = dirs_or_home(".config/needle/config.yaml");
        Self::load_from_path(&path)
    }

    /// Load config from a specific path.
    ///
    /// If the file does not exist, returns the default config.
    pub fn load_from_path(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        let config: Config = serde_yaml::from_str(&text)
            .with_context(|| format!("invalid YAML in config file: {}", path.display()))?;
        Ok(config)
    }

    /// Load workspace overrides from `.needle.yaml` in the given workspace root.
    ///
    /// Returns `None` if the file does not exist. Emits warnings for
    /// non-overridable keys found in the workspace config.
    pub fn load_workspace(workspace_root: &Path) -> Result<Option<WorkspaceOverrides>> {
        let path = workspace_root.join(".needle.yaml");
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read workspace config: {}", path.display()))?;

        // Check for non-overridable keys and warn.
        Self::warn_non_overridable_keys(&text, &path)?;

        let overrides: WorkspaceOverrides = serde_yaml::from_str(&text)
            .with_context(|| format!("invalid YAML in workspace config: {}", path.display()))?;
        Ok(Some(overrides))
    }

    /// Warn about non-overridable top-level keys in workspace config YAML.
    fn warn_non_overridable_keys(yaml_text: &str, path: &Path) -> Result<()> {
        let value: serde_yaml::Value = serde_yaml::from_str(yaml_text)
            .with_context(|| format!("invalid YAML in workspace config: {}", path.display()))?;

        if let serde_yaml::Value::Mapping(map) = value {
            for key in map.keys() {
                if let serde_yaml::Value::String(key_str) = key {
                    if NON_OVERRIDABLE_KEYS.contains(&key_str.as_str()) {
                        tracing::warn!(
                            key = %key_str,
                            path = %path.display(),
                            "workspace config contains non-overridable setting '{}' — ignored",
                            key_str,
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Apply workspace overrides to a config.
    ///
    /// Only overridable fields are applied. Records sources in the source map.
    pub fn apply_workspace(
        config: &mut Config,
        overrides: &WorkspaceOverrides,
        ws_path: &Path,
        sources: &mut SourceMap,
    ) {
        let source = ConfigSource::WorkspaceFile(ws_path.join(".needle.yaml"));

        if let Some(ref agent) = overrides.agent {
            if let Some(ref default) = agent.default {
                config.agent.default = default.clone();
                sources.insert("agent.default".to_string(), source.clone());
            }
            if let Some(timeout) = agent.timeout {
                config.agent.timeout = timeout;
                sources.insert("agent.timeout".to_string(), source.clone());
            }
        }

        if let Some(ref strands) = overrides.strands {
            if strands.weave.is_some() {
                sources.insert("strands.weave".to_string(), source.clone());
            }
            if strands.pulse.is_some() {
                sources.insert("strands.pulse".to_string(), source.clone());
            }
            if strands.unravel.is_some() {
                sources.insert("strands.unravel".to_string(), source.clone());
            }
        }

        if let Some(ref prompt) = overrides.prompt {
            config.prompt = prompt.clone();
            sources.insert("prompt.context_files".to_string(), source.clone());
            sources.insert("prompt.instructions".to_string(), source.clone());
        }

        if let Some(ref verification) = overrides.verification {
            config.verification = verification.clone();
            sources.insert("verification".to_string(), source);
        }
    }

    /// Apply environment variable overrides (`NEEDLE_*` prefix, `__` separator).
    ///
    /// Example: `NEEDLE_AGENT__DEFAULT=opus` sets `agent.default` to `"opus"`.
    pub fn apply_env_overrides(config: &mut Config, sources: &mut SourceMap) {
        for (key, value) in std::env::vars() {
            if let Some(suffix) = key.strip_prefix("NEEDLE_") {
                let config_path = suffix.to_lowercase().replace("__", ".");
                let source = ConfigSource::EnvVar(key.clone());

                match config_path.as_str() {
                    "agent.default" => {
                        config.agent.default = value;
                        sources.insert(config_path, source);
                    }
                    "agent.timeout" => {
                        if let Ok(v) = value.parse::<u64>() {
                            config.agent.timeout = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for agent.timeout — expected integer"
                            );
                        }
                    }
                    "worker.max_workers" => {
                        if let Ok(v) = value.parse::<u32>() {
                            config.worker.max_workers = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for worker.max_workers — expected integer"
                            );
                        }
                    }
                    "worker.idle_timeout" => {
                        if let Ok(v) = value.parse::<u64>() {
                            config.worker.idle_timeout = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for worker.idle_timeout — expected integer"
                            );
                        }
                    }
                    "worker.launch_stagger_seconds" => {
                        if let Ok(v) = value.parse::<u64>() {
                            config.worker.launch_stagger_seconds = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for worker.launch_stagger_seconds — expected integer"
                            );
                        }
                    }
                    "health.heartbeat_interval_secs" => {
                        if let Ok(v) = value.parse::<u64>() {
                            config.health.heartbeat_interval_secs = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for health.heartbeat_interval_secs — expected integer"
                            );
                        }
                    }
                    "health.heartbeat_ttl_secs" => {
                        if let Ok(v) = value.parse::<u64>() {
                            config.health.heartbeat_ttl_secs = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for health.heartbeat_ttl_secs — expected integer"
                            );
                        }
                    }
                    _ => {
                        tracing::debug!(
                            env_var = %key,
                            config_path = %config_path,
                            "unrecognized NEEDLE_ environment variable — ignored"
                        );
                    }
                }
            }
        }
    }

    /// Apply CLI overrides (highest precedence) to a loaded config.
    pub fn apply_overrides(config: &mut Config, overrides: CliOverrides) {
        Self::apply_cli_overrides(config, overrides, &mut SourceMap::new());
    }

    /// Apply CLI overrides with source tracking.
    pub fn apply_cli_overrides(
        config: &mut Config,
        overrides: CliOverrides,
        sources: &mut SourceMap,
    ) {
        if let Some(ws) = overrides.workspace {
            config.workspace.default = ws;
            sources.insert("workspace.default".to_string(), ConfigSource::CliOverride);
        }
        if let Some(agent) = overrides.agent_binary {
            config.agent.default = agent;
            sources.insert("agent.default".to_string(), ConfigSource::CliOverride);
        }
        if let Some(n) = overrides.max_workers {
            config.worker.max_workers = n;
            sources.insert("worker.max_workers".to_string(), ConfigSource::CliOverride);
        }
        // worker_name is handled at the Worker level, not stored in Config
    }

    /// Load the fully resolved configuration using the complete hierarchy:
    ///
    /// defaults → global file → workspace `.needle.yaml` → env vars → CLI args
    ///
    /// Returns the resolved config and a source map showing where each value
    /// came from. The source map only contains entries for values that were
    /// overridden from their defaults.
    pub fn load_resolved(workspace_root: &Path, cli: CliOverrides) -> Result<(Config, SourceMap)> {
        let mut sources = SourceMap::new();

        // Layer 1 + 2: defaults + global config.
        let global_path = dirs_or_home(".config/needle/config.yaml");
        let mut config = Self::load_from_path(&global_path)?;

        // Track which fields came from global config (if file existed).
        if global_path.exists() {
            let source = ConfigSource::GlobalFile(global_path);
            // Mark all top-level sections as from global.
            for key in &[
                "agent.default",
                "agent.timeout",
                "worker.max_workers",
                "worker.idle_timeout",
                "health.heartbeat_interval_secs",
                "health.heartbeat_ttl_secs",
            ] {
                sources.insert((*key).to_string(), source.clone());
            }
        }

        // Layer 3: workspace config.
        if let Some(ws_overrides) = Self::load_workspace(workspace_root)? {
            Self::apply_workspace(&mut config, &ws_overrides, workspace_root, &mut sources);
        }

        // Layer 4: environment variables.
        Self::apply_env_overrides(&mut config, &mut sources);

        // Layer 5: CLI arguments.
        Self::apply_cli_overrides(&mut config, cli, &mut sources);

        Ok((config, sources))
    }

    /// Validate a resolved config.
    ///
    /// Returns a list of errors (empty = valid).
    pub fn validate(config: &Config) -> Vec<ConfigError> {
        let mut errors = Vec::new();

        if config.agent.default.is_empty() {
            errors.push(ConfigError {
                field: "agent.default".to_string(),
                message: "must not be empty".to_string(),
            });
        }

        if config.worker.max_workers == 0 {
            errors.push(ConfigError {
                field: "worker.max_workers".to_string(),
                message: "must be at least 1".to_string(),
            });
        }

        if config.worker.max_workers > 50 {
            errors.push(ConfigError {
                field: "worker.max_workers".to_string(),
                message: format!(
                    "{} exceeds practical fleet limit of 50",
                    config.worker.max_workers
                ),
            });
        }

        if config.worker.cpu_load_warn <= 0.0 || config.worker.cpu_load_warn > 1.0 {
            errors.push(ConfigError {
                field: "worker.cpu_load_warn".to_string(),
                message: "must be in range (0.0, 1.0]".to_string(),
            });
        }

        if config.health.heartbeat_ttl_secs < 3 * config.health.heartbeat_interval_secs {
            errors.push(ConfigError {
                field: "health.heartbeat_ttl_secs".to_string(),
                message: format!(
                    "should be >= 3 * heartbeat_interval_secs ({}); detection may be unreliable",
                    3 * config.health.heartbeat_interval_secs
                ),
            });
        }

        errors
    }

    /// Format config values with source annotations for `--dump --show-source`.
    pub fn dump_with_sources(config: &Config, sources: &SourceMap) -> Vec<String> {
        let fields: Vec<(&str, String)> = vec![
            ("agent.default", config.agent.default.clone()),
            ("agent.timeout", config.agent.timeout.to_string()),
            ("worker.max_workers", config.worker.max_workers.to_string()),
            (
                "worker.idle_timeout",
                config.worker.idle_timeout.to_string(),
            ),
            (
                "worker.launch_stagger_seconds",
                config.worker.launch_stagger_seconds.to_string(),
            ),
            (
                "health.heartbeat_interval_secs",
                config.health.heartbeat_interval_secs.to_string(),
            ),
            (
                "health.heartbeat_ttl_secs",
                config.health.heartbeat_ttl_secs.to_string(),
            ),
            (
                "prompt.context_files",
                format!("{:?}", config.prompt.context_files),
            ),
            (
                "prompt.instructions",
                config
                    .prompt
                    .instructions
                    .as_deref()
                    .unwrap_or("")
                    .to_string(),
            ),
        ];

        fields
            .into_iter()
            .map(|(key, value)| {
                let source = sources.get(key).cloned().unwrap_or(ConfigSource::Default);
                format!("{}: {} (from: {})", key, value, source)
            })
            .collect()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Resolve a path relative to the user's home directory.
fn dirs_or_home(relative: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(relative)
    } else {
        PathBuf::from("/tmp").join(relative)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = Config::default();
        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.is_empty(),
            "default config has validation errors: {:?}",
            errors
        );
    }

    #[test]
    fn missing_agent_binary_fails_validation() {
        let mut config = Config::default();
        config.agent.default = String::new();
        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.iter().any(|e| e.field == "agent.default"),
            "expected agent.default error, got: {:?}",
            errors
        );
    }

    #[test]
    fn zero_max_workers_fails_validation() {
        let mut config = Config::default();
        config.worker.max_workers = 0;
        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.iter().any(|e| e.field == "worker.max_workers"),
            "expected worker.max_workers error, got: {:?}",
            errors
        );
    }

    #[test]
    fn cli_overrides_apply() {
        let mut config = Config::default();
        let overrides = CliOverrides {
            workspace: Some(PathBuf::from("/tmp/test-workspace")),
            agent_binary: Some("gpt4".to_string()),
            max_workers: Some(8),
            ..Default::default()
        };
        ConfigLoader::apply_overrides(&mut config, overrides);
        assert_eq!(
            config.workspace.default,
            PathBuf::from("/tmp/test-workspace")
        );
        assert_eq!(config.agent.default, "gpt4");
        assert_eq!(config.worker.max_workers, 8);
    }

    #[test]
    fn missing_file_returns_default() {
        let config = ConfigLoader::load_from_path(Path::new("/nonexistent/config.yaml")).unwrap();
        let errors = ConfigLoader::validate(&config);
        assert!(errors.is_empty(), "default config should be valid");
    }

    #[test]
    fn yaml_roundtrip() {
        let config = Config::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        let decoded: Config = serde_yaml::from_str(&yaml).unwrap();
        // Spot-check a few values
        assert_eq!(config.agent.default, decoded.agent.default);
        assert_eq!(config.worker.max_workers, decoded.worker.max_workers);
    }

    // ── Workspace config tests ──

    #[test]
    fn workspace_config_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let result = ConfigLoader::load_workspace(dir.path()).unwrap();
        assert!(result.is_none(), "missing .needle.yaml should return None");
    }

    #[test]
    fn workspace_config_overrides_agent_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            "agent:\n  default: opus\n  timeout: 1200\n",
        )
        .unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert_eq!(config.agent.default, "opus");
        assert_eq!(config.agent.timeout, 1200);
        assert!(
            matches!(
                sources.get("agent.default"),
                Some(ConfigSource::WorkspaceFile(_))
            ),
            "agent.default source should be WorkspaceFile"
        );
        assert!(
            matches!(
                sources.get("agent.timeout"),
                Some(ConfigSource::WorkspaceFile(_))
            ),
            "agent.timeout source should be WorkspaceFile"
        );
    }

    #[test]
    fn workspace_config_overrides_prompt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            "prompt:\n  context_files:\n    - AGENTS.md\n  instructions: test instructions\n",
        )
        .unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert_eq!(
            config.prompt.context_files,
            vec![PathBuf::from("AGENTS.md")]
        );
        assert_eq!(
            config.prompt.instructions.as_deref(),
            Some("test instructions")
        );
    }

    #[test]
    fn workspace_config_global_used_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        // No .needle.yaml — global config should remain unchanged.
        let mut config = Config::default();
        config.agent.default = "global-agent".to_string();

        let ws_overrides = ConfigLoader::load_workspace(dir.path()).unwrap();
        assert!(ws_overrides.is_none());
        // Config remains as-is.
        assert_eq!(config.agent.default, "global-agent");
    }

    #[test]
    fn workspace_config_partial_agent_override() {
        let dir = tempfile::tempdir().unwrap();
        // Only override timeout, not default.
        std::fs::write(dir.path().join(".needle.yaml"), "agent:\n  timeout: 999\n").unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let original_agent = config.agent.default.clone();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert_eq!(
            config.agent.default, original_agent,
            "default should not change"
        );
        assert_eq!(config.agent.timeout, 999);
        assert!(
            !sources.contains_key("agent.default"),
            "source should not be set for unchanged field"
        );
    }

    #[test]
    fn non_overridable_keys_are_detected() {
        // This tests the detection logic directly — warnings are emitted via tracing.
        let yaml = "worker:\n  max_workers: 99\nagent:\n  default: opus\ntelemetry:\n  file_sink:\n    enabled: false\n";
        let path = Path::new("/test/.needle.yaml");
        // Should not return error — non-overridable keys produce warnings, not errors.
        let result = ConfigLoader::warn_non_overridable_keys(yaml, path);
        assert!(result.is_ok());
    }

    #[test]
    fn workspace_overrides_deserialize_ignores_non_overridable() {
        let yaml = "agent:\n  default: opus\nworker:\n  max_workers: 99\n";
        let overrides: WorkspaceOverrides = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            overrides.agent.as_ref().unwrap().default.as_deref(),
            Some("opus")
        );
        // WorkspaceOverrides doesn't have a worker field, so it's silently ignored.
    }

    // ── Environment variable tests ──

    #[test]
    fn env_override_agent_default() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        // Temporarily set env var for this test.
        let key = "NEEDLE_AGENT__DEFAULT";
        std::env::set_var(key, "env-opus");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(config.agent.default, "env-opus");
        assert!(
            matches!(sources.get("agent.default"), Some(ConfigSource::EnvVar(k)) if k == key),
            "source should be EnvVar"
        );
    }

    #[test]
    fn env_override_worker_max_workers() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_WORKER__MAX_WORKERS";
        std::env::set_var(key, "12");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(config.worker.max_workers, 12);
    }

    #[test]
    fn env_override_invalid_integer_ignored() {
        let mut config = Config::default();
        let original = config.agent.timeout;
        let mut sources = SourceMap::new();

        let key = "NEEDLE_AGENT__TIMEOUT";
        std::env::set_var(key, "not_a_number");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(
            config.agent.timeout, original,
            "invalid env var should be ignored"
        );
        assert!(!sources.contains_key("agent.timeout"));
    }

    #[test]
    fn env_override_beats_workspace_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            "agent:\n  default: workspace-agent\n",
        )
        .unwrap();

        let mut config = Config::default();
        let mut sources = SourceMap::new();

        // Apply workspace first.
        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);
        assert_eq!(config.agent.default, "workspace-agent");

        // Then env var overrides workspace.
        let key = "NEEDLE_AGENT__DEFAULT";
        std::env::set_var(key, "env-agent");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(config.agent.default, "env-agent");
        assert!(matches!(
            sources.get("agent.default"),
            Some(ConfigSource::EnvVar(_))
        ));
    }

    #[test]
    fn cli_overrides_beat_env_vars() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        // Apply env var.
        let key = "NEEDLE_AGENT__DEFAULT";
        std::env::set_var(key, "env-agent");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        // Then CLI overrides.
        let cli = CliOverrides {
            agent_binary: Some("cli-agent".to_string()),
            ..Default::default()
        };
        ConfigLoader::apply_cli_overrides(&mut config, cli, &mut sources);

        assert_eq!(config.agent.default, "cli-agent");
        assert!(matches!(
            sources.get("agent.default"),
            Some(ConfigSource::CliOverride)
        ));
    }

    // ── Source tracking tests ──

    #[test]
    fn source_map_tracks_cli_overrides() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let cli = CliOverrides {
            workspace: Some(PathBuf::from("/tmp/ws")),
            agent_binary: Some("test-agent".to_string()),
            max_workers: Some(2),
            ..Default::default()
        };
        ConfigLoader::apply_cli_overrides(&mut config, cli, &mut sources);

        assert_eq!(
            sources.get("workspace.default"),
            Some(&ConfigSource::CliOverride)
        );
        assert_eq!(
            sources.get("agent.default"),
            Some(&ConfigSource::CliOverride)
        );
        assert_eq!(
            sources.get("worker.max_workers"),
            Some(&ConfigSource::CliOverride)
        );
    }

    #[test]
    fn dump_with_sources_formats_correctly() {
        let config = Config::default();
        let mut sources = SourceMap::new();
        sources.insert(
            "agent.default".to_string(),
            ConfigSource::GlobalFile(PathBuf::from("/home/test/.config/needle/config.yaml")),
        );

        let lines = ConfigLoader::dump_with_sources(&config, &sources);
        let agent_line = lines
            .iter()
            .find(|l| l.starts_with("agent.default"))
            .unwrap();
        assert!(
            agent_line.contains("from: /home/test/.config/needle/config.yaml"),
            "should show global file source: {}",
            agent_line,
        );

        let timeout_line = lines
            .iter()
            .find(|l| l.starts_with("agent.timeout"))
            .unwrap();
        assert!(
            timeout_line.contains("from: built-in default"),
            "untracked field should show default: {}",
            timeout_line,
        );
    }

    #[test]
    fn config_source_display() {
        assert_eq!(format!("{}", ConfigSource::Default), "built-in default");
        assert_eq!(
            format!("{}", ConfigSource::GlobalFile(PathBuf::from("/a/b.yaml"))),
            "/a/b.yaml"
        );
        assert_eq!(
            format!(
                "{}",
                ConfigSource::WorkspaceFile(PathBuf::from("/ws/.needle.yaml"))
            ),
            "/ws/.needle.yaml"
        );
        assert_eq!(
            format!("{}", ConfigSource::EnvVar("NEEDLE_X".to_string())),
            "NEEDLE_X env var"
        );
        assert_eq!(format!("{}", ConfigSource::CliOverride), "CLI argument");
    }
}
