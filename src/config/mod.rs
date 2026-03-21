//! Hierarchical configuration loading and validation.
//!
//! Resolution order (later layers override earlier):
//! 1. Built-in defaults
//! 2. Global config file (`~/.config/needle/config.yaml`)  [Phase 1]
//! 3. Workspace config file (`.needle.yaml`)               [Phase 2]
//! 4. Environment variables (`NEEDLE_*`)                    [Phase 2]
//! 5. CLI arguments (highest precedence)
//!
//! Config is loaded once at boot and never reloaded.
//!
//! Leaf module — depends only on `types`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
}

impl Default for MendConfig {
    fn default() -> Self {
        MendConfig {
            stuck_threshold_secs: Self::default_stuck_threshold_secs(),
        }
    }
}

impl MendConfig {
    fn default_stuck_threshold_secs() -> u64 {
        300
    }
}

/// Knot strand configuration (exhaustion alerting).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnotConfig {
    /// Alert destination (e.g., webhook URL).
    #[serde(default)]
    pub alert_destination: Option<String>,
}

/// Strand waterfall configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StrandsConfig {
    #[serde(default)]
    pub pluck: PluckConfig,
    #[serde(default)]
    pub mend: MendConfig,
    #[serde(default)]
    pub knot: KnotConfig,
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

/// Telemetry configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub file_sink: FileSinkConfig,
}

/// Prompt construction configuration.
///
/// Loaded from the `prompt` section of workspace config (`.needle.yaml`).
/// Phase 1 uses these fields directly; Phase 3 adds per-strand template overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptConfig {
    /// Paths to context files read from the workspace and included in prompts.
    #[serde(default)]
    pub context_files: Vec<PathBuf>,

    /// Free-form instructions appended to every prompt.
    #[serde(default)]
    pub instructions: Option<String>,
}

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

    /// Apply CLI overrides (highest precedence) to a loaded config.
    pub fn apply_overrides(config: &mut Config, overrides: CliOverrides) {
        if let Some(ws) = overrides.workspace {
            config.workspace.default = ws;
        }
        if let Some(agent) = overrides.agent_binary {
            config.agent.default = agent;
        }
        if let Some(n) = overrides.max_workers {
            config.worker.max_workers = n;
        }
        // worker_name is handled at the Worker level, not stored in Config
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

        errors
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
}
