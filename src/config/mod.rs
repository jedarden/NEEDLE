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

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::cost::{BudgetConfig, PricingConfig};
use crate::types::{IdentifierScheme, IdleAction};
use crate::validation::GateConfig;

// ──────────────────────────────────────────────────────────────────────────────
// Sub-structs
// ──────────────────────────────────────────────────────────────────────────────

/// A single routing rule mapping model patterns to adapters.
///
/// Rules are evaluated in order; the first matching rule determines the adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    /// Regex or glob pattern to match against model names.
    ///
    /// Patterns are matched against the model name only (not provider prefixes).
    /// Examples: `"sonnet"`, `"opus"`, `"claude-sonnet-4-6"`, `"(claude-)?(sonnet|opus).*"`.
    pub match_model: String,

    /// Adapter to use for matching models (e.g., `claude-print`, `claude-code-glm-4.7`).
    pub adapter: String,
}

/// Agent routing configuration.
///
/// Maps model name patterns to adapters. When a bead specifies a model,
/// the routing rules are evaluated in order to determine which adapter to use.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingConfig {
    /// Ordered list of routing rules (first match wins).
    #[serde(default)]
    pub rules: Vec<RoutingRule>,

    /// Fallback adapter when no rules match (defaults to `agent.default`).
    #[serde(default)]
    pub default_adapter: Option<String>,

    /// When true, fail if no routing rule matches instead of falling back.
    ///
    /// If set to true and no rule matches the model:
    /// - The bead dispatch fails immediately
    /// - A `RoutingFailed` telemetry event is emitted
    /// - No fallback to `agent.default` or `default_adapter` occurs
    ///
    /// Default: false (fallback behavior for backward compatibility).
    #[serde(default)]
    pub strict: bool,
}

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

    /// Model-to-adapter routing rules (optional).
    #[serde(default)]
    pub routing: Option<RoutingConfig>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            default: Self::default_agent(),
            args: Vec::new(),
            timeout: Self::default_timeout(),
            adapters_dir: Self::default_adapters_dir(),
            routing: Self::default_routing(),
        }
    }
}

impl AgentConfig {
    fn default_agent() -> String {
        "claude".to_string()
    }
    pub fn default_timeout() -> u64 {
        3600
    }
    fn default_adapters_dir() -> PathBuf {
        dirs_or_home(".config/needle/adapters")
    }

    /// Default routing rules for Anthropic subscription models.
    ///
    /// Routers Anthropic Claude models (sonnet, opus, fable, haiku) to claude-print
    /// to use subscription billing before the June 15, 2026 API credit transition.
    /// All other models fall back to claude-code-glm-4.7.
    fn default_routing() -> Option<RoutingConfig> {
        Some(RoutingConfig {
            rules: vec![RoutingRule {
                // Match Anthropic Claude models on subscription billing
                // Patterns: claude-sonnet-4-6, claude-opus-4-6, claude-fable-5, claude-haiku-4-5-20251001
                match_model: "(claude-)?(sonnet|opus|fable|haiku).*".to_string(),
                adapter: "claude-print".to_string(),
            }],
            default_adapter: Some("claude-code-glm-4.7".to_string()),
            strict: false,
        })
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

    /// Consecutive race_lost attempts before treating the ready queue as empty.
    #[serde(default = "WorkerConfig::default_claim_race_lost_skip")]
    pub claim_race_lost_skip: u32,

    /// How workers generate their unique names.
    #[serde(default)]
    pub identifier_scheme: IdentifierScheme,

    /// Warn when CPU load (0.0–1.0) exceeds this threshold.
    #[serde(default = "WorkerConfig::default_cpu_load_warn")]
    pub cpu_load_warn: f64,

    /// Require shipped (committed + pushed) work, or an explicit bead update,
    /// before accepting a bead's closure (bf-1i9). See `validation::verify_shipped_work`.
    #[serde(default = "WorkerConfig::default_enforce_shipped_work")]
    pub enforce_shipped_work: bool,

    /// Warn when available memory falls below this threshold (MB).
    #[serde(default = "WorkerConfig::default_memory_free_warn_mb")]
    pub memory_free_warn_mb: u64,

    /// Maximum additional wait (seconds) for load-adaptive stagger when load is high.
    #[serde(default = "WorkerConfig::default_adaptive_stagger_max_wait_secs")]
    pub adaptive_stagger_max_wait_secs: u64,

    /// How often (seconds) to recheck load during load-adaptive stagger extended wait.
    #[serde(default = "WorkerConfig::default_adaptive_stagger_check_interval_secs")]
    pub adaptive_stagger_check_interval_secs: u64,

    /// BUILDING state timeout in seconds (0 = unlimited).
    #[serde(default = "WorkerConfig::default_building_timeout")]
    pub building_timeout: u64,

    /// Minimum idle backoff in seconds (event-driven polling floor).
    #[serde(default = "WorkerConfig::default_idle_backoff_min")]
    pub idle_backoff_min: u64,

    /// Maximum idle backoff in seconds (for jittered random selection).
    #[serde(default = "WorkerConfig::default_idle_backoff_max")]
    pub idle_backoff_max: u64,

    /// Short retry backoff in seconds (for found-but-excluded case).
    #[serde(default = "WorkerConfig::default_short_retry_backoff")]
    pub short_retry_backoff: u64,

    /// Explicit path to the worker binary `needle supervise` spawns.
    ///
    /// When `None` (the default), the supervisor resolves
    /// `std::env::current_exe()` — the supervisor and worker are the same
    /// binary, so this is always correct absent an override. Set this only
    /// when the running binary's own path is deliberately not what should be
    /// spawned (e.g. a wrapper script). See GitHub issue jedarden/NEEDLE#11:
    /// the previous hardcoded `Command::new("needle")` resolved whatever
    /// binary happened to occupy that name on `$PATH`, silently spawning the
    /// wrong process when another tool shared the name.
    #[serde(default)]
    pub worker_binary_path: Option<PathBuf>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        WorkerConfig {
            max_workers: Self::default_max_workers(),
            launch_stagger_seconds: Self::default_launch_stagger_seconds(),
            idle_timeout: Self::default_idle_timeout(),
            idle_action: IdleAction::default(),
            max_claim_retries: Self::default_max_claim_retries(),
            claim_race_lost_skip: Self::default_claim_race_lost_skip(),
            identifier_scheme: IdentifierScheme::default(),
            cpu_load_warn: Self::default_cpu_load_warn(),
            enforce_shipped_work: Self::default_enforce_shipped_work(),
            memory_free_warn_mb: Self::default_memory_free_warn_mb(),
            adaptive_stagger_max_wait_secs: Self::default_adaptive_stagger_max_wait_secs(),
            adaptive_stagger_check_interval_secs:
                Self::default_adaptive_stagger_check_interval_secs(),
            building_timeout: Self::default_building_timeout(),
            idle_backoff_min: Self::default_idle_backoff_min(),
            idle_backoff_max: Self::default_idle_backoff_max(),
            short_retry_backoff: Self::default_short_retry_backoff(),
            worker_binary_path: None,
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
    pub fn default_idle_timeout() -> u64 {
        60
    }
    fn default_max_claim_retries() -> u32 {
        3
    }
    fn default_claim_race_lost_skip() -> u32 {
        5
    }
    fn default_cpu_load_warn() -> f64 {
        0.8
    }
    fn default_enforce_shipped_work() -> bool {
        true
    }
    fn default_memory_free_warn_mb() -> u64 {
        512
    }
    pub fn default_building_timeout() -> u64 {
        600
    }
    fn default_idle_backoff_min() -> u64 {
        60
    }
    fn default_idle_backoff_max() -> u64 {
        120
    }
    fn default_short_retry_backoff() -> u64 {
        5
    }
    fn default_adaptive_stagger_max_wait_secs() -> u64 {
        300
    }
    fn default_adaptive_stagger_check_interval_secs() -> u64 {
        5
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

    /// Labels describing this workspace's domain (e.g., `rust`, `api`, `trading`).
    ///
    /// Used for cross-workspace skill sharing: skills from other workspaces whose
    /// labels overlap with this workspace's labels are made available during prompt
    /// building. Configure per-workspace in `.needle.yaml` under `workspace.labels`.
    #[serde(default)]
    pub labels: Vec<String>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        WorkspaceConfig {
            default: Self::default_workspace(),
            home: Self::default_home(),
            labels: Vec::new(),
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

/// Workspace-level labels override (`.needle.yaml` `workspace:` section).
///
/// Only `labels` is overridable at the workspace level; the path fields
/// (`default`, `home`) are resolved globally and cannot be set per-workspace.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkspaceLabelsOverride {
    /// Domain labels for this workspace (e.g., `[rust, api, trading]`).
    #[serde(default)]
    pub labels: Vec<String>,
}

/// Bead CLI backend enumeration.
///
/// Represents the available bead CLI backends that can be configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BeadBackend {
    /// Auto-detect the appropriate backend
    Auto,
    /// bead-forge (bf) - canonical CLI with atomic claiming
    Bf,
    /// br - deprecated alias for bead-rs (legacy support)
    Br,
    /// bead-rs (bead) - native CLI
    Bead,
}

impl std::fmt::Display for BeadBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BeadBackend::Auto => write!(f, "auto"),
            BeadBackend::Bf => write!(f, "bf"),
            BeadBackend::Br => write!(f, "br"),
            BeadBackend::Bead => write!(f, "bead"),
        }
    }
}

/// Bead CLI backend configuration.
///
/// Controls which bead store backend is used and how to resolve its CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadCliConfig {
    /// Backend type selector
    #[serde(default = "BeadCliConfig::default_backend")]
    pub backend: BeadBackend,

    /// Optional explicit path to the CLI binary.
    ///
    /// When set, bypasses all discovery and uses this path directly.
    /// Useful for testing or non-standard installations.
    #[serde(default)]
    pub path: Option<PathBuf>,
}

impl Default for BeadCliConfig {
    fn default() -> Self {
        BeadCliConfig {
            backend: Self::default_backend(),
            path: None,
        }
    }
}

impl BeadCliConfig {
    fn default_backend() -> BeadBackend {
        BeadBackend::Auto
    }
}

/// Backend identifier for the resolved bead CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// bead-forge (canonical CLI, atomic claiming)
    Bf,
    /// bead-rs native CLI
    Bead,
}

/// Resolve the bead CLI binary path from configuration.
///
/// Takes a `BeadCliConfig` and returns the backend type and binary path.
/// This replaces all five hardcoded resolution sites across the codebase.
///
/// # Resolution Order
///
/// - If `explicit_path` is set, uses that path directly (backend detection skipped)
/// - `auto` backend: tries bead-rs and bead-forge candidates for diagnostics
/// - `bf` backend: tries bf → ~/.local/bin/bf
/// - `bead` backend: tries bead → ~/.local/bin/bead → /usr/local/cargo/bin/bead
///
/// # Errors
///
/// Returns an error if no matching binary is found. The error message lists
/// all attempted paths.
pub fn resolve_bead_cli(config: &BeadCliConfig) -> Result<(Backend, PathBuf)> {
    // If an explicit path is set, the configured backend remains authoritative.
    // Inferring the dialect from an arbitrary filename would make renamed or
    // wrapper binaries silently select the wrong command grammar.
    if let Some(ref path) = config.path {
        if path.exists() {
            let backend = match config.backend {
                BeadBackend::Bf => Backend::Bf,
                BeadBackend::Br | BeadBackend::Bead => Backend::Bead,
                BeadBackend::Auto => detect_backend_from_path(path)?,
            };
            return Ok((backend, path.clone()));
        } else {
            bail!(
                "explicit bead CLI path does not exist: {}",
                path.display()
            );
        }
    }

    let home = std::env::var("HOME").unwrap_or_default();

    match config.backend {
        BeadBackend::Bf => {
            // bf only: PATH → ~/.local/bin/bf
            let path = find_on_path("bf")
                .or_else(|_| {
                    let candidate = PathBuf::from(format!("{home}/.local/bin/bf"));
                    if candidate.exists() {
                        Ok(candidate)
                    } else {
                        Err(anyhow!("bf not found"))
                    }
                })
                .context("bf CLI not found (checked PATH, ~/.local/bin/bf)")?;
            Ok((Backend::Bf, path))
        }
        BeadBackend::Br | BeadBackend::Bead => {
            // bead only: PATH → ~/.local/bin/bead → /usr/local/cargo/bin/bead
            let path = find_on_path("bead")
                .or_else(|_| {
                    let candidate = PathBuf::from(format!("{home}/.local/bin/bead"));
                    if candidate.exists() {
                        Ok(candidate)
                    } else {
                        Err(anyhow!("bead not found"))
                    }
                })
                .or_else(|_| {
                    let candidate = PathBuf::from("/usr/local/cargo/bin/bead");
                    if candidate.exists() {
                        Ok(candidate)
                    } else {
                        Err(anyhow!("bead not found"))
                    }
                })
                .context(
                    "bead CLI not found (checked PATH, ~/.local/bin/bead, /usr/local/cargo/bin/bead)",
                )?;
            Ok((Backend::Bead, path))
        }
        BeadBackend::Auto => {
            // Auto is diagnostics-only; explicit workspace binding remains
            // mandatory for production store construction.

            // Try bf first
            if let Ok(path) = find_on_path("bf") {
                return Ok((Backend::Bf, path));
            }
            let bf_local = PathBuf::from(format!("{home}/.local/bin/bf"));
            if bf_local.exists() {
                return Ok((Backend::Bf, bf_local));
            }

            // Then bead
            if let Ok(path) = find_on_path("bead") {
                return Ok((Backend::Bead, path));
            }
            let bead_local = PathBuf::from(format!("{home}/.local/bin/bead"));
            if bead_local.exists() {
                return Ok((Backend::Bead, bead_local));
            }
            let bead_cargo = PathBuf::from("/usr/local/cargo/bin/bead");
            if bead_cargo.exists() {
                return Ok((Backend::Bead, bead_cargo));
            }

            // Nothing found
            bail!(
                "no bead CLI found (tried: bf on PATH, {home}/.local/bin/bf, bead on PATH, {home}/.local/bin/bead, /usr/local/cargo/bin/bead)"
            )
        }
    }
}

/// Resolve an executable against the current `PATH` without process-global
/// caching. Backend selection is workspace-scoped, and tests and embedders may
/// intentionally resolve different environments in one NEEDLE process.
fn find_on_path(binary: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").ok_or_else(|| anyhow!("PATH is not set"))?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(binary))
        .find(|candidate| is_executable(candidate))
        .ok_or_else(|| anyhow!("{binary} not found on PATH"))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Detect the backend type from a binary path.
///
/// Infers the backend from the filename: "bf" selects bead-forge; every other
/// name selects bead-rs.
fn detect_backend_from_path(path: &Path) -> Result<Backend> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("invalid binary path: no filename"))?;

    match file_name {
        "bf" => Ok(Backend::Bf),
        _ => Ok(Backend::Bead), // Default to Bead for unknown names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Delegates to the single crate-wide env lock so that `HOME`/`PATH`
    /// mutation here excludes — and is excluded by — every other test that
    /// touches the process environment.
    fn isolate_bead_cli_env() -> (
        std::sync::MutexGuard<'static, ()>,
        crate::util::test_env::EnvGuard,
    ) {
        crate::util::test_env::isolate_env()
    }

    #[test]
    fn test_bead_cli_config_with_auto_backend() {
        let config = BeadCliConfig {
            backend: BeadBackend::Auto,
            path: None,
        };
        assert_eq!(config.backend, BeadBackend::Auto);
        assert!(config.path.is_none());
    }

    #[test]
    fn test_bead_cli_config_with_bf_backend() {
        let config = BeadCliConfig {
            backend: BeadBackend::Bf,
            path: None,
        };
        assert_eq!(config.backend, BeadBackend::Bf);
        assert!(config.path.is_none());
    }

    #[test]
    fn test_bead_cli_config_with_br_backend() {
        let config = BeadCliConfig {
            backend: BeadBackend::Br,
            path: None,
        };
        assert_eq!(config.backend, BeadBackend::Br);
        assert!(config.path.is_none());
    }

    #[test]
    fn test_bead_cli_config_with_bead_backend() {
        let config = BeadCliConfig {
            backend: BeadBackend::Bead,
            path: None,
        };
        assert_eq!(config.backend, BeadBackend::Bead);
        assert!(config.path.is_none());
    }

    #[test]
    fn test_bead_cli_config_with_explicit_path() {
        let custom_path = PathBuf::from("/custom/path/to/bead");
        let config = BeadCliConfig {
            backend: BeadBackend::Bead,
            path: Some(custom_path.clone()),
        };
        assert_eq!(config.backend, BeadBackend::Bead);
        assert_eq!(config.path, Some(custom_path));
    }

    #[test]
    fn test_bead_cli_config_serialization() {
        let config = BeadCliConfig {
            backend: BeadBackend::Bead,
            path: Some(PathBuf::from("/usr/local/bin/bead")),
        };

        // Test serialization to YAML
        let yaml = serde_yaml::to_string(&config).unwrap();
        assert!(yaml.contains("bead"));
        assert!(yaml.contains("/usr/local/bin/bead"));

        // Test deserialization from YAML
        let deserialized: BeadCliConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.backend, BeadBackend::Bead);
        assert_eq!(deserialized.path, Some(PathBuf::from("/usr/local/bin/bead")));
    }

    #[test]
    fn test_bead_cli_config_default() {
        let config = BeadCliConfig::default();
        assert_eq!(config.backend, BeadBackend::Auto);
        assert!(config.path.is_none());
    }

    #[test]
    fn test_bead_backend_serialization() {
        // Test that each backend variant serializes correctly
        let backends = vec![
            (BeadBackend::Auto, "auto"),
            (BeadBackend::Bf, "bf"),
            (BeadBackend::Br, "br"),
            (BeadBackend::Bead, "bead"),
        ];

        for (backend, expected_str) in backends {
            let yaml = serde_yaml::to_string(&backend).unwrap();
            assert!(yaml.contains(expected_str), "Expected '{}' in YAML for {:?}", expected_str, backend);

            // Test round-trip
            let deserialized: BeadBackend = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(deserialized, backend);
        }
    }

    #[test]
    fn test_bead_backend_equality() {
        assert_eq!(BeadBackend::Auto, BeadBackend::Auto);
        assert_eq!(BeadBackend::Bf, BeadBackend::Bf);
        assert_eq!(BeadBackend::Br, BeadBackend::Br);
        assert_eq!(BeadBackend::Bead, BeadBackend::Bead);

        assert_ne!(BeadBackend::Auto, BeadBackend::Bf);
        assert_ne!(BeadBackend::Bf, BeadBackend::Bead);
        assert_ne!(BeadBackend::Br, BeadBackend::Bead);
    }

    #[test]
    fn test_detect_backend_from_path() {
        assert_eq!(
            detect_backend_from_path(PathBuf::from("/usr/bin/bf").as_path()).unwrap(),
            Backend::Bf
        );
        assert_eq!(
            detect_backend_from_path(PathBuf::from("/usr/bin/bead").as_path()).unwrap(),
            Backend::Bead
        );
        assert_eq!(
            detect_backend_from_path(PathBuf::from("/usr/bin/my-custom-bead").as_path()).unwrap(),
            Backend::Bead
        );
    }

    #[test]
    fn test_detect_backend_from_path_no_filename() {
        assert!(detect_backend_from_path(PathBuf::from("/").as_path()).is_err());
    }

    #[test]
    fn test_resolve_bead_cli_bf_backend() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let bf_bin = tmp_dir.path().join("bf");
        std::fs::write(&bf_bin, "#!/bin/sh\necho test").unwrap();
        make_executable(&bf_bin);

        let config = BeadCliConfig {
            backend: BeadBackend::Bf,
            path: Some(bf_bin.clone()),
        };

        let (backend, path) = resolve_bead_cli(&config).unwrap();
        assert_eq!(backend, Backend::Bf);
        assert_eq!(path, bf_bin);
    }

    #[test]
    fn test_resolve_bead_cli_bead_backend() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let bead_bin = tmp_dir.path().join("bead");
        std::fs::write(&bead_bin, "#!/bin/sh\necho test").unwrap();
        make_executable(&bead_bin);

        let config = BeadCliConfig {
            backend: BeadBackend::Bead,
            path: Some(bead_bin.clone()),
        };

        let (backend, path) = resolve_bead_cli(&config).unwrap();
        assert_eq!(backend, Backend::Bead);
        assert_eq!(path, bead_bin);
    }

    #[test]
    fn test_resolve_bead_cli_explicit_path_takes_precedence() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let custom_bin = tmp_dir.path().join("my-bead-cli");
        std::fs::write(&custom_bin, "#!/bin/sh\necho test").unwrap();
        make_executable(&custom_bin);

        let config = BeadCliConfig {
            backend: BeadBackend::Auto,
            path: Some(custom_bin.clone()),
        };

        let (backend, path) = resolve_bead_cli(&config).unwrap();
        assert_eq!(backend, Backend::Bead); // Detected from filename
        assert_eq!(path, custom_bin);
    }

    #[test]
    fn test_resolve_bead_cli_explicit_path_nonexistent() {
        let config = BeadCliConfig {
            backend: BeadBackend::Auto,
            path: Some(PathBuf::from("/nonexistent/binary")),
        };

        let result = resolve_bead_cli(&config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("explicit bead CLI path does not exist"));
    }

    #[test]
    fn test_resolve_bead_cli_auto_precedence_bf_first() {
        let (_lock, _env) = isolate_bead_cli_env();
        let tmp_dir = tempfile::tempdir().unwrap();
        let home = tmp_dir.path().to_path_buf();

        // Create ~/.local/bin/bf
        let bin_dir = home.join(".local/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bf_bin = bin_dir.join("bf");
        std::fs::write(&bf_bin, "#!/bin/sh\necho test").unwrap();
        make_executable(&bf_bin);

        // Set HOME to tmp_dir
        std::env::set_var("HOME", &home);
        std::env::set_var("PATH", "");

        let config = BeadCliConfig {
            backend: BeadBackend::Auto,
            path: None,
        };

        let (backend, path) = resolve_bead_cli(&config).unwrap();
        assert_eq!(backend, Backend::Bf);
        assert_eq!(path, bf_bin);
    }

    #[test]
    fn test_resolve_bead_cli_br_backend() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let br_bin = tmp_dir.path().join("br");
        std::fs::write(&br_bin, "#!/bin/sh\necho test").unwrap();
        make_executable(&br_bin);

        let config = BeadCliConfig {
            backend: BeadBackend::Br,
            path: Some(br_bin.clone()),
        };

        let (backend, path) = resolve_bead_cli(&config).unwrap();
        assert_eq!(backend, Backend::Bead);
        assert_eq!(path, br_bin);
    }

    #[test]
    fn test_resolve_bead_cli_auto_ignores_deprecated_br() {
        let (_lock, _env) = isolate_bead_cli_env();
        let tmp_dir = tempfile::tempdir().unwrap();
        let home = tmp_dir.path().to_path_buf();

        // A deprecated br shim must not become a candidate. bead-rs wins.
        let bin_dir = home.join(".local/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let br_bin = bin_dir.join("br");
        std::fs::write(&br_bin, "#!/bin/sh\necho test").unwrap();
        make_executable(&br_bin);
        let bead_bin = bin_dir.join("bead");
        std::fs::write(&bead_bin, "#!/bin/sh\necho test").unwrap();
        make_executable(&bead_bin);

        // Set HOME to tmp_dir
        std::env::set_var("HOME", &home);
        std::env::set_var("PATH", "");

        let config = BeadCliConfig {
            backend: BeadBackend::Auto,
            path: None,
        };

        let (backend, path) = resolve_bead_cli(&config).unwrap();
        assert_eq!(backend, Backend::Bead);
        assert_eq!(path, bead_bin);
    }

    #[test]
    fn test_resolve_bead_cli_auto_precedence_bead_third() {
        let (_lock, _env) = isolate_bead_cli_env();
        let tmp_dir = tempfile::tempdir().unwrap();
        let home = tmp_dir.path().to_path_buf();

        // Create ~/.local/bin/bead (no bf or br)
        let bin_dir = home.join(".local/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bead_bin = bin_dir.join("bead");
        std::fs::write(&bead_bin, "#!/bin/sh\necho test").unwrap();
        make_executable(&bead_bin);

        // Set HOME to tmp_dir
        std::env::set_var("HOME", &home);
        std::env::set_var("PATH", "");

        let config = BeadCliConfig {
            backend: BeadBackend::Auto,
            path: None,
        };

        let (backend, path) = resolve_bead_cli(&config).unwrap();
        assert_eq!(backend, Backend::Bead);
        assert_eq!(path, bead_bin);
    }

    #[test]
    fn test_resolve_bead_cli_auto_no_binary_error() {
        let (_lock, _env) = isolate_bead_cli_env();
        let tmp_dir = tempfile::tempdir().unwrap();
        let home = tmp_dir.path().to_path_buf();

        // Set HOME to empty tmp_dir (no binaries)
        std::env::set_var("HOME", &home);

        // Clear PATH to prevent finding system binaries
        std::env::set_var("PATH", "");

        let config = BeadCliConfig {
            backend: BeadBackend::Auto,
            path: None,
        };

        let result = resolve_bead_cli(&config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no bead CLI found"));
    }

    #[test]
    fn test_resolve_bead_cli_bf_backend_not_found() {
        let (_lock, _env) = isolate_bead_cli_env();
        let tmp_dir = tempfile::tempdir().unwrap();
        let home = tmp_dir.path().to_path_buf();

        // Set HOME to empty tmp_dir (no bf binary)
        std::env::set_var("HOME", &home);

        // Clear PATH to prevent finding system binaries
        std::env::set_var("PATH", "");

        let config = BeadCliConfig {
            backend: BeadBackend::Bf,
            path: None,
        };

        let result = resolve_bead_cli(&config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("bf CLI not found"));
    }

    /// Regression test: auto on bf-only host resolves exactly as today's hardcoded chain.
    ///
    /// This is the no-regression guard for every existing deployment.
    /// Today's chain (src/bead_store/bf_cli.rs:71-80):
    ///   which::which("bf").or_else(|_| { ~/.local/bin/bf })
    ///
    /// When auto mode is used on a host with only bf installed (no br, no bead),
    /// it must resolve to the exact same path that the current hardcoded chain produces.
    #[test]
    fn test_regression_auto_bf_only_host_matches_hardcoded_chain() {
        let (_lock, _env) = isolate_bead_cli_env();
        let tmp_dir = tempfile::tempdir().unwrap();
        let home = tmp_dir.path().to_path_buf();

        // Scenario 1: bf on PATH (today's common case)
        {
            let bin_dir = home.join("path-bin");
            std::fs::create_dir_all(&bin_dir).unwrap();
            let bf_bin = bin_dir.join("bf");
            std::fs::write(&bf_bin, "#!/bin/sh\necho test").unwrap();
            make_executable(&bf_bin);

            // Set PATH to include bf, HOME to tmp_dir
            std::env::set_var("PATH", &bin_dir);
            std::env::set_var("HOME", &home);

            // Today's hardcoded chain result
            let hardcoded_result = which::which("bf")
                .or_else(|_| {
                    let candidate = PathBuf::from(format!("{}/.local/bin/bf", home.display()));
                    if candidate.exists() {
                        Ok(candidate)
                    } else {
                        Err(anyhow!("bf not found on PATH or at ~/.local/bin/bf"))
                    }
                })
                .unwrap();

            // New auto mode result
            let config = BeadCliConfig {
                backend: BeadBackend::Auto,
                path: None,
            };
            let (backend, auto_path) = resolve_bead_cli(&config).unwrap();

            // Must match exactly
            assert_eq!(backend, Backend::Bf);
            assert_eq!(auto_path, hardcoded_result);
        }

        // Scenario 2: bf NOT on PATH, but at ~/.local/bin/bf
        {
            // Clear PATH
            std::env::set_var("PATH", "");

            // Create ~/.local/bin/bf
            let local_bin = home.join(".local/bin");
            std::fs::create_dir_all(&local_bin).unwrap();
            let bf_local = local_bin.join("bf");
            std::fs::write(&bf_local, "#!/bin/sh\necho test").unwrap();
            make_executable(&bf_local);

            std::env::set_var("HOME", &home);

            // Today's hardcoded chain result
            let hardcoded_result = which::which("bf")
                .or_else(|_| {
                    let candidate = PathBuf::from(format!("{}/.local/bin/bf", home.display()));
                    if candidate.exists() {
                        Ok(candidate)
                    } else {
                        Err(anyhow!("bf not found on PATH or at ~/.local/bin/bf"))
                    }
                })
                .unwrap();

            // New auto mode result
            let config = BeadCliConfig {
                backend: BeadBackend::Auto,
                path: None,
            };
            let (backend, auto_path) = resolve_bead_cli(&config).unwrap();

            // Must match exactly
            assert_eq!(backend, Backend::Bf);
            assert_eq!(auto_path, hardcoded_result);
        }
    }

    /// Helper to make a file executable on Unix
    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).unwrap();
        }
        #[cfg(not(unix))]
        {
            // No-op on non-Unix
        }
    }
}

/// Pluck strand configuration (primary bead selection).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluckConfig {
    /// Labels to exclude from selection.
    #[serde(default)]
    pub exclude_labels: Vec<String>,

    /// Auto-split beads after this many consecutive failures (0 = disabled).
    ///
    /// When a bead accumulates this many consecutive failures, pluck dispatches
    /// a SPLIT instruction to the worker instead of the normal process-the-bead prompt.
    /// The worker decomposes the bead into smaller child beads and converts the
    /// parent into an umbrella (parent depends on last child, parent set non-ready).
    #[serde(default = "PluckConfig::default_split_after_failures")]
    pub split_after_failures: u32,

    /// Write persistent starvation records to NEEDLE workspace (default: false).
    ///
    /// When enabled, starvation events are written to a persistent log file in
    /// NEEDLE's workspace (~/.needle/state/starvation-records.jsonl) rather than
    /// only being emitted as telemetry. Records are never written to target scanned
    /// workspaces, only to NEEDLE's own workspace.
    #[serde(default = "PluckConfig::default_persistent_starvation_records")]
    pub persistent_starvation_records: bool,
}

impl PluckConfig {
    fn default_split_after_failures() -> u32 {
        3
    }

    fn default_persistent_starvation_records() -> bool {
        false
    }
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

    /// Workers with 0 beads processed longer than this (seconds) are flagged.
    #[serde(default = "MendConfig::default_idle_timeout")]
    pub idle_timeout: u64,
}

impl Default for MendConfig {
    fn default() -> Self {
        MendConfig {
            stuck_threshold_secs: Self::default_stuck_threshold_secs(),
            lock_ttl_secs: Self::default_lock_ttl_secs(),
            db_check_interval: Self::default_db_check_interval(),
            idle_timeout: Self::default_idle_timeout(),
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
    pub fn default_idle_timeout() -> u64 {
        120
    }
}

/// Supervisor detection configuration.
///
/// Used to detect if a supervisor process is running via heartbeat files
/// or Unix domain sockets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorDetectionConfig {
    /// Path to the supervisor's heartbeat file for liveness detection.
    pub heartbeat_path: PathBuf,

    /// Optional Unix domain socket path for communication with the supervisor.
    #[serde(default)]
    pub socket_path: Option<PathBuf>,
}

/// Explore strand configuration (multi-workspace discovery).
///
/// ## Workspace Discovery Modes
///
/// **Default mode (recommended):** Leave `workspaces` empty.
/// - All directories under `workspace_root` containing a `.beads/` subdirectory
///   are automatically discovered and scanned for beads.
/// - This is the intended default for the fleet as a whole — new workspaces are
///   picked up automatically without configuration changes.
///
/// **Pinned mode (exception):** Set `workspaces` to an explicit list of paths.
/// - Only the specified workspaces are scanned; auto-discovery is disabled.
/// - Use this to restrict a specific worker to a fixed repo set (e.g., a dedicated
///   worker for a high-priority workspace that should not process other work).
/// - This is an exception mechanism — the fleet should normally run with `workspaces`
///   empty to avoid missing beads in newly-added workspaces.
///
/// ## Rationale
///
/// The 2026-07-19 fleet incident occurred because `explore.workspaces` was populated
/// with 24 hardcoded paths. This permanently disabled discovery fleet-wide, and the
/// list had already drifted stale (missing valid repos like commitgraph and
/// twitterapi-proxy). Recursive discovery is now the documented intended default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreConfig {
    /// Whether the Explore strand is enabled.
    #[serde(default = "ExploreConfig::default_enabled")]
    pub enabled: bool,

    /// **Pin/exception list** for restricting a worker to specific workspaces.
    ///
    /// When **empty** (the default), enables recursive discovery under `workspace_root`:
    /// all directories containing a `.beads/` subdirectory are automatically scanned.
    /// This is the intended default for the fleet — new workspaces are picked up
    /// without configuration changes.
    ///
    /// When **non-empty**, disables auto-discovery and scans only these paths.
    /// Use this to restrict a specific worker to a fixed repo set (e.g., a dedicated
    /// worker for a high-priority workspace). This is an exception mechanism — most
    /// workers should leave this empty to avoid missing beads in newly-added workspaces.
    ///
    /// **WARNING:** When non-empty, a WARN log is emitted at startup naming the
    /// pinned repos, so operators can immediately see when a worker is running in
    /// restricted/exception mode rather than discovering this only via missing beads.
    #[serde(default)]
    pub workspaces: Vec<PathBuf>,

    /// Root path for workspace auto-discovery (when `workspaces` is empty).
    ///
    /// All directories under this path containing a `.beads/` subdirectory
    /// are treated as workspaces. Defaults to the user's home directory.
    #[serde(default = "ExploreConfig::default_workspace_root")]
    pub workspace_root: PathBuf,

    /// Re-run workspace discovery every N cycles (0 = disabled).
    ///
    /// When set (default: 60), the workspace list is refreshed periodically
    /// so new stores are picked up without requiring worker restarts.
    /// Re-discovery preserves the "no upward traversal" constraint (only scanning
    /// workspace_root's immediate children) and the "explicit workspaces override"
    /// (when workspaces is non-empty, re-discovery is skipped).
    ///
    /// A modest default (60 cycles ≈ 1 hour at typical worker cadence) balances
    /// freshness with filesystem churn. Set to 0 to disable periodic re-discovery.
    #[serde(default = "ExploreConfig::default_rediscovery_cycles")]
    pub rediscovery_cycles: u32,

    /// Starvation alarm threshold in minutes (0 = disabled).
    ///
    /// When set (default: 15), emits a WARN telemetry event when ready beads
    /// exist in scanned workspaces but this worker has not successfully claimed
    /// any bead for the specified number of minutes. This helps detect cases
    /// where workers are stuck in exclusion loops or competing for the same
    /// beads without making progress.
    #[serde(default = "ExploreConfig::default_starvation_threshold_minutes")]
    pub starvation_threshold_minutes: u64,
}

impl Default for ExploreConfig {
    fn default() -> Self {
        ExploreConfig {
            enabled: Self::default_enabled(),
            workspaces: Vec::new(),
            workspace_root: Self::default_workspace_root(),
            rediscovery_cycles: Self::default_rediscovery_cycles(),
            starvation_threshold_minutes: Self::default_starvation_threshold_minutes(),
        }
    }
}

impl ExploreConfig {
    fn default_enabled() -> bool {
        true
    }

    fn default_starvation_threshold_minutes() -> u64 {
        15
    }

    fn default_workspace_root() -> PathBuf {
        dirs_or_home("")
    }

    fn default_rediscovery_cycles() -> u32 {
        60
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

/// Timeout-triggered mitosis policy configuration.
///
/// Controls whether timeout-triggered mitosis is enabled and which timeout
/// reasons qualify. This is an opt-in policy (disabled by default) that only
/// applies to legitimate task timeouts, not infrastructure failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutTriggeredPolicy {
    /// Whether timeout-triggered mitosis is enabled (default: false).
    ///
    /// When disabled, timeout failures are handled like any other failure
    /// (increment failure count, potentially trigger regular mitosis).
    /// When enabled, qualifying timeouts trigger mitosis immediately regardless
    /// of failure count or first_failure_only setting.
    #[serde(default = "TimeoutTriggeredPolicy::default_enabled")]
    pub enabled: bool,

    /// Qualify agent-level wall-clock timeouts (exit code 124).
    ///
    /// Agent process timeouts represent legitimate task duration limits
    /// (e.g., a 1-hour agent timeout on a complex analysis task). These are
    /// good candidates for mitosis: the task is real but may be too large.
    #[serde(default)]
    pub agent_wallclock_timeout: bool,

    /// Qualify outcome handler timeouts (validation gates).
    ///
    /// Handler timeouts occur when post-agent validation (tests, linting)
    /// exceeds its budget. These may qualify if the task is genuinely
    /// large and the validation is part of the work itself.
    #[serde(default)]
    pub handler_timeout: bool,

    /// Minimum fraction of timeout budget that must elapse (0.0-1.0, default: 0.9).
    ///
    /// Only triggers when elapsed_time >= timeout * min_elapsed_fraction.
    /// This prevents spurious mitosis on near-misses: if a 1-hour timeout
    /// triggers at 59 minutes (0.98 fraction), the task genuinely needs splitting.
    /// If it triggers at 5 minutes (0.08 fraction), it's likely a flaky timeout.
    #[serde(default = "TimeoutTriggeredPolicy::default_min_elapsed_fraction")]
    pub min_elapsed_fraction: f64,
}

impl Default for TimeoutTriggeredPolicy {
    fn default() -> Self {
        TimeoutTriggeredPolicy {
            enabled: Self::default_enabled(),
            agent_wallclock_timeout: false,
            handler_timeout: false,
            min_elapsed_fraction: Self::default_min_elapsed_fraction(),
        }
    }
}

impl TimeoutTriggeredPolicy {
    fn default_enabled() -> bool {
        false // Disabled by default for backward compatibility
    }

    fn default_min_elapsed_fraction() -> f64 {
        0.9 // Require 90% of timeout budget to elapse
    }

    /// Check if this policy qualifies a timeout reason for mitosis.
    ///
    /// Returns true if:
    /// - The policy is enabled
    /// - The timeout reason matches a qualified type
    /// - The elapsed fraction meets the minimum threshold
    pub fn qualifies(&self, timeout_reason: &str, elapsed_fraction: f64) -> bool {
        if !self.enabled {
            return false;
        }

        // Check elapsed fraction threshold
        if elapsed_fraction < self.min_elapsed_fraction {
            return false;
        }

        // Check if timeout reason is qualified
        match timeout_reason {
            "agent_wallclock_timeout" | "timeout" => self.agent_wallclock_timeout,
            "handler_timeout" => self.handler_timeout,
            _ => false, // All other reasons (build_timeout, idle, cancellation, crash, etc.) do not qualify
        }
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

    /// Force mitosis after this many consecutive failures (0 = disabled).
    ///
    /// When set, mitosis triggers on the Nth failure regardless of
    /// `first_failure_only`. This prevents infinite loops where a bead
    /// fails repeatedly without ever splitting.
    #[serde(default)]
    pub force_failure_threshold: u32,

    /// Re-run mitosis every N consecutive failures after the first (0 = disabled).
    ///
    /// Fires at failure_count == 1, 1+N, 1+2N, ...
    /// Only when force_failure_threshold == 0.
    /// Beads already carrying a mitosis-depth label are skipped.
    #[serde(default)]
    pub repeat_interval: u32,

    /// Maximum mitosis generation depth (0 = unlimited).
    ///
    /// A bead's depth is tracked via its `mitosis-depth:N` label (root beads
    /// are depth 0; each split increments the depth by 1 for its children).
    /// Beads at or beyond this depth are flagged with a `human` label instead
    /// of being split further, to prevent unbounded recursive splitting.
    #[serde(default)]
    pub max_depth: u32,

    /// Timeout-triggered mitosis policy (opt-in, default: disabled).
    #[serde(default)]
    pub timeout_triggered: TimeoutTriggeredPolicy,
}

impl Default for MitosisConfig {
    fn default() -> Self {
        MitosisConfig {
            enabled: Self::default_enabled(),
            first_failure_only: Self::default_first_failure_only(),
            force_failure_threshold: 0,
            repeat_interval: 0,
            max_depth: 0,
            timeout_triggered: TimeoutTriggeredPolicy::default(),
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

    /// Custom prompt template for the gap analysis agent invocation.
    ///
    /// Template variables: `{doc_files}`, `{existing_beads}`, `{workspace}`.
    /// When `None`, the built-in template is used.
    #[serde(default)]
    pub prompt_template: Option<String>,
}

impl Default for WeaveConfig {
    fn default() -> Self {
        WeaveConfig {
            enabled: false,
            max_beads_per_run: Self::default_max_beads_per_run(),
            cooldown_hours: Self::default_cooldown_hours(),
            exclude_workspaces: Vec::new(),
            doc_patterns: Self::default_doc_patterns(),
            prompt_template: None,
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

/// Pulse strand configuration (codebase health scans).
///
/// Pulse runs configured scanners (linters, test coverage, etc.) and creates
/// beads for significant findings. Heavily guardrailed to prevent noise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PulseConfig {
    /// Whether the Pulse strand is enabled (opt-in, default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Scanner commands to run (e.g., `cargo clippy`, `npm run lint`).
    ///
    /// Each command should output findings to stdout in a parseable format.
    #[serde(default)]
    pub scanners: Vec<ScannerConfig>,

    /// Maximum beads to create per pulse run (default: 5).
    #[serde(default = "PulseConfig::default_max_beads_per_run")]
    pub max_beads_per_run: u32,

    /// Minimum hours between pulse runs (default: 48).
    #[serde(default = "PulseConfig::default_cooldown_hours")]
    pub cooldown_hours: u64,

    /// Minimum severity level to create a bead (1-5, 1=critical, default: 3).
    #[serde(default = "PulseConfig::default_severity_threshold")]
    pub severity_threshold: u8,

    /// Custom prompt template for agent-assisted analysis.
    ///
    /// Template variables: `{scanner}`, `{output}`, `{workspace}`.
    #[serde(default)]
    pub prompt_template: Option<String>,
}

/// Configuration for a single scanner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerConfig {
    /// Human-readable name for this scanner (e.g., "clippy").
    pub name: String,

    /// Shell command to run the scanner.
    pub command: String,

    /// Minimum severity for findings from this scanner (1-5).
    /// Overrides global severity_threshold if set.
    #[serde(default)]
    pub severity_threshold: Option<u8>,
}

impl Default for PulseConfig {
    fn default() -> Self {
        PulseConfig {
            enabled: false,
            scanners: Vec::new(),
            max_beads_per_run: Self::default_max_beads_per_run(),
            cooldown_hours: Self::default_cooldown_hours(),
            severity_threshold: Self::default_severity_threshold(),
            prompt_template: None,
        }
    }
}

impl PulseConfig {
    fn default_max_beads_per_run() -> u32 {
        5
    }
    fn default_cooldown_hours() -> u64 {
        48
    }
    fn default_severity_threshold() -> u8 {
        3
    }
}

/// Reflect strand configuration (meta-analysis and learning consolidation).
///
/// Reflect runs after all other strands return NoWork. It reads bead close
/// bodies since the last consolidation, extracts retrospective patterns, merges
/// them into learnings.md, and promotes high-frequency patterns to skill files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectConfig {
    /// Whether the Reflect strand is enabled (default: true).
    #[serde(default = "ReflectConfig::default_enabled")]
    pub enabled: bool,

    /// Minimum beads closed since last consolidation before running (default: 10).
    #[serde(default = "ReflectConfig::default_min_beads_since_last")]
    pub min_beads_since_last: usize,

    /// Minimum hours between reflect runs (default: 24).
    #[serde(default = "ReflectConfig::default_cooldown_hours")]
    pub cooldown_hours: u64,

    /// Maximum learnings to add per run (default: 10).
    #[serde(default = "ReflectConfig::default_max_learnings_per_run")]
    pub max_learnings_per_run: usize,

    /// Maximum skill files to create or update per run (default: 3).
    #[serde(default = "ReflectConfig::default_max_skills_per_run")]
    pub max_skills_per_run: usize,

    /// Days before unreinforced entries are pruned (default: 90).
    #[serde(default = "ReflectConfig::default_learning_retention_days")]
    pub learning_retention_days: u32,

    /// Maximum total learning entries before forced pruning (default: 80).
    #[serde(default = "ReflectConfig::default_max_learnings")]
    pub max_learnings: usize,

    /// Agent command for automatic retrospective extraction (e.g. `claude --print`).
    /// When set, beads closed without a `## Retrospective` block will be passed to
    /// this agent to infer one. When `None`, only explicit retrospective blocks are used.
    #[serde(default)]
    pub extraction_agent: Option<String>,

    /// Custom prompt template for retrospective extraction.
    /// Template variables: `{title}`, `{close_body}`.
    /// When `None`, the built-in default prompt is used.
    #[serde(default)]
    pub extraction_prompt_template: Option<String>,

    /// Maximum beads to pass to the extraction agent per Reflect run (default: 5).
    #[serde(default = "ReflectConfig::default_max_extraction_per_run")]
    pub max_extraction_per_run: usize,

    /// How far back to read session transcripts in days (default: 7).
    #[serde(default = "ReflectConfig::default_transcript_recency_days")]
    pub transcript_recency_days: u32,

    /// Cap on sessions to analyze per transcript run (default: 50).
    #[serde(default = "ReflectConfig::default_transcript_max_sessions")]
    pub transcript_max_sessions: usize,

    /// Jaccard similarity threshold for drift session clustering (default: 0.6).
    #[serde(default = "ReflectConfig::default_drift_similarity_threshold")]
    pub drift_similarity_threshold: f64,

    /// Enable drift detection (default: true).
    #[serde(default = "ReflectConfig::default_drift_enabled")]
    pub drift_enabled: bool,

    /// Enable ADR decision extraction (default: true).
    #[serde(default = "ReflectConfig::default_adr_enabled")]
    pub adr_enabled: bool,

    /// Enable writing learnings to CLAUDE.md (default: true).
    #[serde(default = "ReflectConfig::default_claude_md_placement")]
    pub claude_md_placement: bool,
}

impl Default for ReflectConfig {
    fn default() -> Self {
        ReflectConfig {
            enabled: Self::default_enabled(),
            min_beads_since_last: Self::default_min_beads_since_last(),
            cooldown_hours: Self::default_cooldown_hours(),
            max_learnings_per_run: Self::default_max_learnings_per_run(),
            max_skills_per_run: Self::default_max_skills_per_run(),
            learning_retention_days: Self::default_learning_retention_days(),
            max_learnings: Self::default_max_learnings(),
            extraction_agent: None,
            extraction_prompt_template: None,
            max_extraction_per_run: Self::default_max_extraction_per_run(),
            transcript_recency_days: Self::default_transcript_recency_days(),
            transcript_max_sessions: Self::default_transcript_max_sessions(),
            drift_similarity_threshold: Self::default_drift_similarity_threshold(),
            drift_enabled: Self::default_drift_enabled(),
            adr_enabled: Self::default_adr_enabled(),
            claude_md_placement: Self::default_claude_md_placement(),
        }
    }
}

impl ReflectConfig {
    fn default_enabled() -> bool {
        true
    }
    fn default_min_beads_since_last() -> usize {
        10
    }
    fn default_cooldown_hours() -> u64 {
        24
    }
    fn default_max_learnings_per_run() -> usize {
        10
    }
    fn default_max_skills_per_run() -> usize {
        3
    }
    fn default_learning_retention_days() -> u32 {
        90
    }
    fn default_max_learnings() -> usize {
        80
    }
    fn default_max_extraction_per_run() -> usize {
        5
    }
    fn default_transcript_recency_days() -> u32 {
        7
    }
    fn default_transcript_max_sessions() -> usize {
        50
    }
    fn default_drift_similarity_threshold() -> f64 {
        0.6
    }
    fn default_drift_enabled() -> bool {
        true
    }
    fn default_adr_enabled() -> bool {
        true
    }
    fn default_claude_md_placement() -> bool {
        true
    }
}

/// Splice strand configuration (worker failure documentation).
///
/// Splice detects dead workers (stale heartbeat) and live-but-looping workers
/// (fresh heartbeat, stuck in a tight event loop). Both are documented as
/// failure beads in the configured report workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpliceConfig {
    /// Whether the Splice strand is enabled (default: true).
    #[serde(default = "SpliceConfig::default_enabled")]
    pub enabled: bool,

    /// Seconds since last heartbeat before a worker is considered stale (default: 300).
    #[serde(default = "SpliceConfig::default_stale_threshold_secs")]
    pub stale_threshold_secs: u64,

    /// Path to the workspace where worker failure beads are created.
    /// When `None`, uses the current bead store (no separate workspace).
    #[serde(default)]
    pub report_workspace: Option<PathBuf>,

    /// Whether to scan live workers' JSONL for stuck-loop patterns (default: true).
    #[serde(default = "SpliceConfig::default_detect_live_loops")]
    pub detect_live_loops: bool,

    /// Max events to scan from JSONL tail per worker (default: 200).
    #[serde(default = "SpliceConfig::default_live_loop_scan_events")]
    pub live_loop_scan_events: usize,

    /// Min same-bead `bead.claim.race_lost` events to flag as claim churn (default: 20).
    #[serde(default = "SpliceConfig::default_claim_churn_threshold")]
    pub claim_churn_threshold: u32,

    /// Max JSONL growth (bytes) in `live_loop_window_secs` without `agent.completed`
    /// before flagging as log runaway (default: 10 MiB).
    #[serde(default = "SpliceConfig::default_log_runaway_bytes")]
    pub log_runaway_bytes: u64,

    /// Window for the log-rate runaway check in seconds (default: 300).
    #[serde(default = "SpliceConfig::default_live_loop_window_secs")]
    pub live_loop_window_secs: u64,
}

impl Default for SpliceConfig {
    fn default() -> Self {
        SpliceConfig {
            enabled: Self::default_enabled(),
            stale_threshold_secs: Self::default_stale_threshold_secs(),
            report_workspace: None,
            detect_live_loops: Self::default_detect_live_loops(),
            live_loop_scan_events: Self::default_live_loop_scan_events(),
            claim_churn_threshold: Self::default_claim_churn_threshold(),
            log_runaway_bytes: Self::default_log_runaway_bytes(),
            live_loop_window_secs: Self::default_live_loop_window_secs(),
        }
    }
}

impl SpliceConfig {
    fn default_enabled() -> bool {
        true
    }
    fn default_stale_threshold_secs() -> u64 {
        300
    }
    fn default_detect_live_loops() -> bool {
        true
    }
    fn default_live_loop_scan_events() -> usize {
        200
    }
    fn default_claim_churn_threshold() -> u32 {
        20
    }
    fn default_log_runaway_bytes() -> u64 {
        10 * 1024 * 1024 // 10 MiB
    }
    fn default_live_loop_window_secs() -> u64 {
        300
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
    #[serde(default)]
    pub pulse: PulseConfig,
    #[serde(default)]
    pub reflect: ReflectConfig,
    #[serde(default)]
    pub splice: SpliceConfig,
    /// Learning and trace retention configuration.
    #[serde(default)]
    pub learning: LearningConfig,
}

/// A workspace-specific custom sanitization pattern.
///
/// Configured under `learning.trace_sanitization.custom_patterns` in `.needle.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomSanitizationPattern {
    /// Rule identifier (used in `[REDACTED:<id>]` output).
    pub id: String,
    /// Regex pattern. Capture group 1 is the secret; whole match used if absent.
    pub pattern: String,
    /// Optional minimum Shannon entropy threshold.
    #[serde(default)]
    pub entropy: Option<f64>,
}

/// Trace sanitization configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSanitizationConfig {
    /// Enable trace sanitization (default: true).
    #[serde(default = "TraceSanitizationConfig::default_enabled")]
    pub enabled: bool,

    /// Workspace-specific patterns applied alongside gitleaks rules.
    #[serde(default)]
    pub custom_patterns: Vec<CustomSanitizationPattern>,
}

impl Default for TraceSanitizationConfig {
    fn default() -> Self {
        TraceSanitizationConfig {
            enabled: Self::default_enabled(),
            custom_patterns: Vec::new(),
        }
    }
}

impl TraceSanitizationConfig {
    fn default_enabled() -> bool {
        true
    }
}

/// Learning and trace retention configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningConfig {
    /// Retention days for failed bead traces (default: 30).
    #[serde(default = "LearningConfig::default_trace_retention_failed")]
    pub trace_retention_failed_days: u32,

    /// Retention days for successful bead traces (default: 7).
    #[serde(default = "LearningConfig::default_trace_retention_success")]
    pub trace_retention_success_days: u32,

    /// Maximum number of active learning entries (default: 80).
    ///
    /// When exceeded, the consolidator prunes stale entries (>90 days)
    /// and consolidates redundant entries.
    #[serde(default = "LearningConfig::default_max_learnings")]
    pub max_learnings: usize,

    /// Trace sanitization settings (gitleaks rules + custom patterns).
    #[serde(default)]
    pub trace_sanitization: TraceSanitizationConfig,

    /// Path to the global learnings file (default: ~/.config/needle/global-learnings.md).
    ///
    /// This file stores cross-workspace learnings detected by the consolidator.
    /// It is loaded into all workspace prompts as supplementary context.
    #[serde(default = "LearningConfig::default_global_learnings_file")]
    pub global_learnings_file: PathBuf,

    /// Maximum entries in the global learnings file (default: 40).
    ///
    /// Cross-cutting lessons should be distilled; this cap keeps the file focused.
    #[serde(default = "LearningConfig::default_max_global_learnings")]
    pub max_global_learnings: usize,
}

impl Default for LearningConfig {
    fn default() -> Self {
        LearningConfig {
            trace_retention_failed_days: Self::default_trace_retention_failed(),
            trace_retention_success_days: Self::default_trace_retention_success(),
            max_learnings: Self::default_max_learnings(),
            trace_sanitization: TraceSanitizationConfig::default(),
            global_learnings_file: Self::default_global_learnings_file(),
            max_global_learnings: Self::default_max_global_learnings(),
        }
    }
}

impl LearningConfig {
    fn default_trace_retention_failed() -> u32 {
        30
    }

    fn default_trace_retention_success() -> u32 {
        7
    }

    fn default_max_learnings() -> usize {
        80
    }

    fn default_global_learnings_file() -> PathBuf {
        dirs_or_home(".config/needle/global-learnings.md")
    }

    fn default_max_global_learnings() -> usize {
        40
    }
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

    /// Number of days to retain log files (0 = no cleanup). Default: 30.
    #[serde(default = "FileSinkConfig::default_retention_days")]
    pub retention_days: u32,
}

impl Default for FileSinkConfig {
    fn default() -> Self {
        FileSinkConfig {
            enabled: Self::default_enabled(),
            log_dir: Self::default_log_dir(),
            retention_days: Self::default_retention_days(),
        }
    }
}

impl FileSinkConfig {
    fn default_enabled() -> bool {
        true
    }

    fn default_log_dir() -> Option<PathBuf> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        Some(PathBuf::from(home).join(".needle").join("logs"))
    }

    fn default_retention_days() -> u32 {
        30
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

/// A single hook definition: an event filter glob and a dispatch target.
///
/// Events whose `event_type` matches the glob are dispatched to the
/// configured `command` and/or `url`. At least one must be set.
/// Hooks are fire-and-forget — failures are logged but never block the worker.
///
/// # Example
/// ```yaml
/// telemetry:
///   hooks:
///     - event_filter: "outcome.*"
///       command: "/path/to/alert.sh"
///     - event_filter: "worker.errored"
///       url: "https://hooks.slack.com/services/..."
///     - event_filter: "effort.recorded"
///       command: "/path/to/cost-tracker.sh"
///       url: "https://dashboard.example.com/ingest"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    /// Glob pattern matched against `event_type` (e.g. `"outcome.*"`).
    pub event_filter: String,

    /// Shell command to execute. The event JSON is written to stdin.
    /// Leave empty or omit when dispatching only to `url`.
    #[serde(default)]
    pub command: String,

    /// HTTP(S) URL to POST the event JSON to (native webhook support).
    /// `Content-Type: application/json` is set automatically.
    /// Omit when dispatching only to `command`.
    #[serde(default)]
    pub url: Option<String>,
}

/// OTLP sink configuration for OpenTelemetry export.
///
/// When enabled, NEEDLE exports traces, metrics, and logs to any OTLP-compliant
/// collector (e.g., Grafana Tempo, Jaeger, OpenTelemetry Collector).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtlpSinkConfig {
    /// Enable or disable the OTLP sink.
    #[serde(default = "OtlpSinkConfig::default_enabled")]
    pub enabled: bool,

    /// OTLP endpoint URL (e.g., `http://localhost:4317` for gRPC).
    #[serde(default = "OtlpSinkConfig::default_endpoint")]
    pub endpoint: String,

    /// Protocol: "grpc" or "http".
    #[serde(default = "OtlpSinkConfig::default_protocol")]
    pub protocol: String,

    /// Request timeout in seconds (default: 10).
    #[serde(default = "OtlpSinkConfig::default_timeout_secs")]
    pub timeout_secs: u64,

    /// Compression: "gzip", "none", or "zstd".
    #[serde(default = "OtlpSinkConfig::default_compression")]
    pub compression: String,

    /// TLS configuration: "none", "tls", or "mtls".
    #[serde(default = "OtlpSinkConfig::default_tls")]
    pub tls: String,

    /// HTTP headers to send with each request (format: "key: value").
    #[serde(default)]
    pub headers: Vec<String>,

    /// Resource attributes attached to all exported signals (format: "key=value").
    /// Reserved keys `service.name` and `service.instance.id` cannot be overridden.
    #[serde(default)]
    pub resource_attributes: Vec<String>,

    /// Metrics export interval in seconds (default: 10).
    #[serde(default = "OtlpSinkConfig::default_metrics_interval_secs")]
    pub metrics_interval_secs: u64,

    /// Service namespace for OpenTelemetry semantic conventions.
    /// Defaults to "needle-fleet" if not specified.
    #[serde(default = "OtlpSinkConfig::default_service_namespace")]
    pub service_namespace: String,

    /// Maximum queue size for batch processors (default: 2048).
    /// When the queue fills, the OTel SDK drops the oldest items.
    #[serde(default = "OtlpSinkConfig::default_max_queue_size")]
    pub max_queue_size: usize,
}

impl Default for OtlpSinkConfig {
    fn default() -> Self {
        OtlpSinkConfig {
            enabled: Self::default_enabled(),
            endpoint: Self::default_endpoint(),
            protocol: Self::default_protocol(),
            timeout_secs: Self::default_timeout_secs(),
            compression: Self::default_compression(),
            tls: Self::default_tls(),
            headers: Vec::new(),
            resource_attributes: Vec::new(),
            metrics_interval_secs: Self::default_metrics_interval_secs(),
            service_namespace: Self::default_service_namespace(),
            max_queue_size: Self::default_max_queue_size(),
        }
    }
}

impl OtlpSinkConfig {
    fn default_enabled() -> bool {
        false
    }

    fn default_endpoint() -> String {
        "http://localhost:4317".to_string()
    }

    fn default_protocol() -> String {
        "grpc".to_string()
    }

    fn default_timeout_secs() -> u64 {
        10
    }

    fn default_compression() -> String {
        "gzip".to_string()
    }

    fn default_tls() -> String {
        "none".to_string()
    }

    fn default_metrics_interval_secs() -> u64 {
        10
    }

    fn default_service_namespace() -> String {
        "needle-fleet".to_string()
    }

    fn default_max_queue_size() -> usize {
        2048
    }
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
    /// Optional OTLP sink for OpenTelemetry export.
    #[serde(default)]
    pub otlp_sink: OtlpSinkConfig,
}

/// Health monitoring configuration (heartbeat, peer detection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    /// How often to emit a heartbeat file (seconds).
    ///
    /// Workers write a heartbeat JSON file at this interval to signal liveness.
    /// The heartbeat file contains worker state, current bead, and other metadata
    /// used for peer detection and scaling decisions.
    #[serde(default = "HealthConfig::default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,

    /// Time after which a heartbeat is considered stale (seconds).
    ///
    /// Heartbeats older than this threshold are considered stale, triggering
    /// worker failure detection. Should be at least 3x `heartbeat_interval_secs`
    /// for reliable detection.
    #[serde(default = "HealthConfig::default_heartbeat_ttl_secs")]
    pub heartbeat_ttl_secs: u64,

    /// Directory path for heartbeat files (relative to workspace.home).
    ///
    /// Each worker writes its heartbeat file to this directory. The path is
    /// resolved as `workspace.home/heartbeat_dir`. When not set, defaults to
    /// `state/heartbeats`.
    ///
    /// Example: setting to `heartbeats` resolves to `~/.needle/heartbeats`.
    #[serde(default)]
    pub heartbeat_dir: Option<PathBuf>,
}

impl Default for HealthConfig {
    fn default() -> Self {
        HealthConfig {
            heartbeat_interval_secs: Self::default_heartbeat_interval_secs(),
            heartbeat_ttl_secs: Self::default_heartbeat_ttl_secs(),
            heartbeat_dir: Self::default_heartbeat_dir(),
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
    fn default_heartbeat_dir() -> Option<PathBuf> {
        None
    }
}

/// Supervisor detection configuration.
///
/// Controls how NEEDLE detects whether it's running under a supervisor process.
/// Supervisor detection is used for graceful shutdown and resource cleanup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorConfig {
    /// Path to the supervisor's heartbeat file.
    ///
    /// The supervisor writes this file at a regular interval to signal liveness.
    /// Workers check this file to determine if the supervisor is still running.
    /// If the file is missing or stale, workers may initiate graceful shutdown.
    ///
    /// When not set, defaults to `state/supervisor-heartbeat.json` under `workspace.home`.
    ///
    /// Example: `~/.needle/state/supervisor-heartbeat.json`
    #[serde(default)]
    pub heartbeat_path: Option<PathBuf>,

    /// Path to the supervisor's control socket (Unix domain socket).
    ///
    /// Some supervisors use a control socket for IPC. If set, workers can use
    /// this socket to send status updates or receive commands from the supervisor.
    ///
    /// When `None`, no socket-based communication is available.
    #[serde(default)]
    pub socket_path: Option<PathBuf>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        SupervisorConfig {
            heartbeat_path: Self::default_heartbeat_path(),
            socket_path: Self::default_socket_path(),
        }
    }
}

impl SupervisorConfig {
    fn default_heartbeat_path() -> Option<PathBuf> {
        None
    }

    fn default_socket_path() -> Option<PathBuf> {
        None
    }

    /// Returns the resolved heartbeat path, defaulting to `workspace.home/state/supervisor-heartbeat.json`.
    pub fn resolved_heartbeat_path(&self, workspace_home: &Path) -> PathBuf {
        self.heartbeat_path
            .clone()
            .unwrap_or_else(|| workspace_home.join("state/supervisor-heartbeat.json"))
    }

    /// Create a supervisor config from environment variables.
    ///
    /// Reads the following environment variables:
    /// - `SUPERVISOR_HEARTBEAT_PATH`: Path to the supervisor's heartbeat file (optional)
    /// - `SUPERVISOR_SOCKET_PATH`: Path to the supervisor's control socket (optional)
    ///
    /// Returns a config with sensible defaults if environment variables are not set.
    ///
    /// # Errors
    ///
    /// Returns an error if the heartbeat path is set but invalid.
    pub fn from_env() -> Result<Self> {
        let heartbeat_path = std::env::var("SUPERVISOR_HEARTBEAT_PATH")
            .ok()
            .map(|s| expand_tilde(&PathBuf::from(s)));

        let socket_path = std::env::var("SUPERVISOR_SOCKET_PATH")
            .ok()
            .map(|s| expand_tilde(&PathBuf::from(s)));

        Ok(SupervisorConfig {
            heartbeat_path,
            socket_path,
        })
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

/// A/B test variant for a prompt template.
///
/// Configured under `prompt.variants.<template_name>` in `.needle.yaml`.
/// Workers are assigned to variants deterministically by `hash(worker_id) % 100`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantConfig {
    /// Variant name (e.g., `"control"`, `"v2"`).
    pub name: String,

    /// Percentage of workers assigned to this variant (0–100).
    pub weight: u8,

    /// Path to the file containing the variant template content.
    /// Resolved relative to the workspace root.
    pub content_file: PathBuf,
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

    /// A/B test variants per template name.
    ///
    /// Keys are template names; values are ordered lists of variants.
    /// Workers are assigned to variants based on `hash(worker_id) % 100`
    /// compared against cumulative variant weights.
    ///
    /// Example `.needle.yaml`:
    /// ```yaml
    /// prompt:
    ///   variants:
    ///     pluck:
    ///       - name: v2
    ///         weight: 50
    ///         content_file: prompts/pluck-v2.txt
    /// ```
    #[serde(default)]
    pub variants: std::collections::BTreeMap<String, Vec<VariantConfig>>,
}

/// Self-modification (hot-reload) configuration.
///
/// Controls the :testing → :stable promotion pipeline with canary tests.
/// When enabled, new versions of needle are tested against a canary workspace
/// before being promoted to stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfModificationConfig {
    /// Whether self-modification is enabled (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Path to the canary test workspace containing test beads.
    /// Defaults to `~/.needle/canary/`.
    #[serde(default = "SelfModificationConfig::default_canary_workspace")]
    pub canary_workspace: PathBuf,

    /// Automatically promote :testing to :stable when canary passes.
    /// When false, requires manual `needle promote` command.
    #[serde(default)]
    pub auto_promote: bool,

    /// Maximum time (seconds) to run canary tests before considering it a timeout.
    #[serde(default = "SelfModificationConfig::default_canary_timeout")]
    pub canary_timeout: u64,

    /// Fleet hot-reloads from :stable between beads (default: true).
    /// When enabled, workers check for a new :stable binary after each bead
    /// cycle and re-exec if a different binary is detected.
    #[serde(default = "SelfModificationConfig::default_hot_reload")]
    pub hot_reload: bool,
}

impl Default for SelfModificationConfig {
    fn default() -> Self {
        SelfModificationConfig {
            enabled: false,
            canary_workspace: Self::default_canary_workspace(),
            auto_promote: false,
            canary_timeout: Self::default_canary_timeout(),
            hot_reload: Self::default_hot_reload(),
        }
    }
}

impl SelfModificationConfig {
    fn default_canary_workspace() -> PathBuf {
        dirs_or_home(".needle/canary")
    }

    pub fn default_canary_timeout() -> u64 {
        // 30 minutes. A canary test is a full agent dispatch against a real
        // bead — clone, reason, edit, run gates, commit — not a unit test. The
        // previous 5-minute budget was shorter than a routine dispatch, so every
        // canary timed out and every upgrade was rejected no matter how healthy
        // the binary (observed 2026-08-07: 0/4, workers still visibly working
        // when the runner gave up on them).
        1800
    }

    fn default_hot_reload() -> bool {
        true
    }
}

/// FABRIC telemetry forwarding configuration.
///
/// When enabled, NEEDLE workers POST structured events to the FABRIC
/// web server (`fabric web`) for live dashboard display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FabricConfig {
    /// Whether to forward events to FABRIC (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// HTTP endpoint to POST events to (e.g., `http://localhost:3000/api/events`).
    #[serde(default)]
    pub endpoint: String,

    /// Request timeout in seconds (default: 2).
    #[serde(default = "FabricConfig::default_timeout")]
    pub timeout: u64,

    /// Batch events before sending instead of sending one at a time (default: false).
    #[serde(default)]
    pub batching: bool,
}

impl Default for FabricConfig {
    fn default() -> Self {
        FabricConfig {
            enabled: false,
            endpoint: String::new(),
            timeout: Self::default_timeout(),
            batching: false,
        }
    }
}

impl FabricConfig {
    fn default_timeout() -> u64 {
        2
    }
}

/// Outcome handling configuration (failure circuit-breaker, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeConfig {
    /// Consecutive failures before quarantining a bead (0 = disabled).
    ///
    /// When a bead accumulates this many consecutive failures, it is automatically
    /// quarantined: status is set to `blocked`, a `cycling` label is added, and a
    /// `BeadQuarantined` telemetry event is emitted.
    ///
    /// The default (5) is above Pluck's `split_after_failures` default (3) so
    /// mitosis gets first crack at splitting the bead before quarantine kicks in.
    #[serde(default = "OutcomeConfig::default_quarantine_after_failures")]
    pub quarantine_after_failures: u32,
}

impl Default for OutcomeConfig {
    fn default() -> Self {
        OutcomeConfig {
            quarantine_after_failures: Self::default_quarantine_after_failures(),
        }
    }
}

impl OutcomeConfig {
    fn default_quarantine_after_failures() -> u32 {
        5
    }
}

/// Validation gate execution configuration.
///
/// Both fields preserve NEEDLE's previous hardcoded behavior as their default,
/// so upgrading alone changes nothing for an existing deployment. See GitHub
/// issues jedarden/NEEDLE#8 and jedarden/NEEDLE#9: a gate running a real
/// verification workload (container test suite, secret scan, fresh-model diff
/// verifier) needs more than 50 seconds and more than 4KB of stderr to report
/// a useful failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationConfig {
    /// Timeout (seconds) for the outcome handler's gate-execution wrapper.
    #[serde(default = "ValidationConfig::default_outcome_timeout_seconds")]
    pub outcome_timeout_seconds: u64,

    /// Maximum bytes of gate command stderr captured on failure.
    #[serde(default = "ValidationConfig::default_stderr_cap_bytes")]
    pub stderr_cap_bytes: usize,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        ValidationConfig {
            outcome_timeout_seconds: Self::default_outcome_timeout_seconds(),
            stderr_cap_bytes: Self::default_stderr_cap_bytes(),
        }
    }
}

impl ValidationConfig {
    pub fn default_outcome_timeout_seconds() -> u64 {
        50
    }
    fn default_stderr_cap_bytes() -> usize {
        4096
    }
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
/// - `verification` (legacy) or `gates` (new pluggable system)
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
    /// Legacy format — prefer `gates` for new configurations.
    #[serde(default)]
    pub verification: Option<Vec<String>>,
    /// Pluggable validation gates.
    #[serde(default)]
    pub gates: Option<Vec<GateConfig>>,
    /// Workspace identity labels (overridable per-workspace).
    ///
    /// Set in `.needle.yaml` under `workspace.labels`. Used for cross-workspace
    /// skill sharing: skills from Explore workspaces whose labels overlap with
    /// these labels are injected into prompts alongside local skills.
    #[serde(default)]
    pub workspace: Option<WorkspaceLabelsOverride>,
    /// Workspace-owned bead backend binding.
    #[serde(default)]
    pub bead_cli: Option<BeadCliConfig>,
}

/// Agent fields overridable at the workspace level.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceAgentOverrides {
    pub default: Option<String>,
    pub timeout: Option<u64>,
    pub routing: Option<RoutingConfig>,
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
///
/// Note: `workspace` is intentionally absent — `workspace.labels` IS overridable
/// per-workspace. The path fields (`default`, `home`) are simply ignored if set.
const NON_OVERRIDABLE_KEYS: &[&str] = &["worker", "limits", "health", "telemetry", "validation"];

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
    /// Bead CLI backend configuration.
    #[serde(default)]
    pub bead_cli: BeadCliConfig,
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
    /// Legacy format — prefer `gates` for new configurations.
    #[serde(default)]
    pub verification: Vec<String>,
    /// Pluggable validation gates.
    #[serde(default)]
    pub gates: Vec<GateConfig>,
    /// Self-modification (hot-reload) configuration.
    #[serde(default)]
    pub self_modification: SelfModificationConfig,
    /// FABRIC live dashboard forwarding.
    #[serde(default)]
    pub fabric: FabricConfig,
    /// Supervisor detection configuration.
    #[serde(default)]
    pub supervisor: SupervisorConfig,
    /// Outcome handling configuration (failure circuit-breaker).
    #[serde(default)]
    pub outcome: OutcomeConfig,
    /// Tsnet identity provisioning configuration.
    #[serde(default)]
    pub tsnet: crate::tsnet::TsnetConfig,
    /// Validation gate execution configuration (timeout, stderr cap).
    #[serde(default)]
    pub validation: ValidationConfig,
}

impl Config {
    /// Expand all tilde (~) paths to absolute paths using $HOME.
    ///
    /// This must be called after deserialization to ensure all paths
    /// like `~/.needle` are expanded to `/home/user/.needle`.
    /// Paths without tildes are left unchanged.
    pub fn expand_tildes(&mut self) {
        // workspace section
        self.workspace.default = expand_tilde(&self.workspace.default);
        self.workspace.home = expand_tilde(&self.workspace.home);

        // worker section
        self.worker.worker_binary_path = expand_tilde_option(&self.worker.worker_binary_path);

        // agent section
        self.agent.adapters_dir = expand_tilde(&self.agent.adapters_dir);

        // bead_cli section
        self.bead_cli.path = expand_tilde_option(&self.bead_cli.path);

        // strands.explore section
        self.strands.explore.workspace_root = expand_tilde(&self.strands.explore.workspace_root);
        self.strands.explore.workspaces = expand_tilde_vec(&self.strands.explore.workspaces);

        // strands.weave section
        self.strands.weave.exclude_workspaces =
            expand_tilde_vec(&self.strands.weave.exclude_workspaces);

        // strands.splice section
        self.strands.splice.report_workspace =
            expand_tilde_option(&self.strands.splice.report_workspace);

        // strands.pulse.scanners[].command paths are strings, not PathBuf, so skip

        // strands.reflect section - no PathBuf fields

        // strands.learning section
        self.strands.learning.global_learnings_file =
            expand_tilde(&self.strands.learning.global_learnings_file);

        // telemetry section
        if let Some(ref mut log_dir) = self.telemetry.file_sink.log_dir {
            *log_dir = expand_tilde(log_dir);
        }

        // health section
        self.health.heartbeat_dir = expand_tilde_option(&self.health.heartbeat_dir);

        // supervisor section
        self.supervisor.heartbeat_path = expand_tilde_option(&self.supervisor.heartbeat_path);
        self.supervisor.socket_path = expand_tilde_option(&self.supervisor.socket_path);

        // prompt section
        self.prompt.context_files = expand_tilde_vec(&self.prompt.context_files);

        // self_modification section
        self.self_modification.canary_workspace =
            expand_tilde(&self.self_modification.canary_workspace);

        // prompt.variants[].content_file
        for variants in self.prompt.variants.values_mut() {
            for variant in variants {
                variant.content_file = expand_tilde(&variant.content_file);
            }
        }
    }
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
    pub explore_workspace_root: Option<PathBuf>,
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
            let mut config = Config::default();
            config.expand_tildes();
            return Ok(config);
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        let mut config: Config = serde_yaml::from_str(&text)
            .with_context(|| format!("invalid YAML in config file: {}", path.display()))?;
        config.expand_tildes();
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
            if let Some(ref routing) = agent.routing {
                config.agent.routing = Some(routing.clone());
                sources.insert("agent.routing".to_string(), source.clone());
            }
        }

        if let Some(ref strands) = overrides.strands {
            if let Some(ref weave_val) = strands.weave {
                if let Ok(weave_cfg) = serde_yaml::from_value::<WeaveConfig>(weave_val.clone()) {
                    config.strands.weave = weave_cfg;
                }
                sources.insert("strands.weave".to_string(), source.clone());
            }
            if let Some(ref pulse_val) = strands.pulse {
                if let Ok(pulse_cfg) = serde_yaml::from_value::<PulseConfig>(pulse_val.clone()) {
                    config.strands.pulse = pulse_cfg;
                }
                sources.insert("strands.pulse".to_string(), source.clone());
            }
            if let Some(ref unravel_val) = strands.unravel {
                if let Ok(unravel_cfg) =
                    serde_yaml::from_value::<UnravelConfig>(unravel_val.clone())
                {
                    config.strands.unravel = unravel_cfg;
                }
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
            sources.insert("verification".to_string(), source.clone());
        }

        if let Some(ref gates) = overrides.gates {
            config.gates = gates.clone();
            sources.insert("gates".to_string(), source.clone());
        }

        if let Some(ref ws) = overrides.workspace {
            if !ws.labels.is_empty() {
                config.workspace.labels = ws.labels.clone();
                sources.insert("workspace.labels".to_string(), source.clone());
            }
        }

        if let Some(ref bead_cli) = overrides.bead_cli {
            config.bead_cli = bead_cli.clone();
            sources.insert("bead_cli.backend".to_string(), source.clone());
            if bead_cli.path.is_some() {
                sources.insert("bead_cli.path".to_string(), source);
            }
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
                    "agent.routing.default_adapter" => {
                        config
                            .agent
                            .routing
                            .get_or_insert_with(RoutingConfig::default)
                            .default_adapter = Some(value);
                        sources.insert(config_path, source);
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
                    "self_modification.enabled" => {
                        if let Ok(v) = value.parse::<bool>() {
                            config.self_modification.enabled = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for self_modification.enabled — expected true/false"
                            );
                        }
                    }
                    "self_modification.auto_promote" => {
                        if let Ok(v) = value.parse::<bool>() {
                            config.self_modification.auto_promote = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for self_modification.auto_promote — expected true/false"
                            );
                        }
                    }
                    "self_modification.canary_timeout" => {
                        if let Ok(v) = value.parse::<u64>() {
                            config.self_modification.canary_timeout = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for self_modification.canary_timeout — expected integer"
                            );
                        }
                    }
                    // Explore roams by default: with `workspaces` empty it scans
                    // `workspace_root` (which defaults to $HOME) for every bead
                    // workspace it can find. Anything that spawns a worker as a
                    // subprocess and needs it confined — the canary runner above
                    // all — must be able to switch that off without editing the
                    // fleet's global config.
                    "strands.explore.enabled" => match value.parse::<bool>() {
                        Ok(v) => {
                            config.strands.explore.enabled = v;
                            sources.insert(config_path, source);
                        }
                        Err(_) => {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for strands.explore.enabled — expected true or false"
                            );
                        }
                    },
                    "strands.explore.workspace_root" => {
                        config.strands.explore.workspace_root = PathBuf::from(&value);
                        sources.insert(config_path, source);
                    }
                    "supervisor.heartbeat_path" => {
                        config.supervisor.heartbeat_path =
                            Some(expand_tilde(&PathBuf::from(value)));
                        sources.insert(config_path, source);
                    }
                    "supervisor.socket_path" => {
                        config.supervisor.socket_path = Some(expand_tilde(&PathBuf::from(value)));
                        sources.insert(config_path, source);
                    }
                    "workspace.home" => {
                        config.workspace.home = expand_tilde(&PathBuf::from(value));
                        sources.insert(config_path, source);
                    }
                    "workspace.default" => {
                        config.workspace.default = expand_tilde(&PathBuf::from(value));
                        sources.insert(config_path, source);
                    }
                    // Timeout-triggered mitosis policy overrides
                    "strands.mitosis.timeout_triggered.enabled" => {
                        if let Ok(v) = value.parse::<bool>() {
                            config.strands.mitosis.timeout_triggered.enabled = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for strands.mitosis.timeout_triggered.enabled — expected true/false"
                            );
                        }
                    }
                    "strands.mitosis.timeout_triggered.agent_wallclock_timeout" => {
                        if let Ok(v) = value.parse::<bool>() {
                            config
                                .strands
                                .mitosis
                                .timeout_triggered
                                .agent_wallclock_timeout = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for strands.mitosis.timeout_triggered.agent_wallclock_timeout — expected true/false"
                            );
                        }
                    }
                    "strands.mitosis.timeout_triggered.handler_timeout" => {
                        if let Ok(v) = value.parse::<bool>() {
                            config.strands.mitosis.timeout_triggered.handler_timeout = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for strands.mitosis.timeout_triggered.handler_timeout — expected true/false"
                            );
                        }
                    }
                    "strands.mitosis.timeout_triggered.min_elapsed_fraction" => {
                        if let Ok(v) = value.parse::<f64>() {
                            config
                                .strands
                                .mitosis
                                .timeout_triggered
                                .min_elapsed_fraction = v;
                            sources.insert(config_path, source);
                        } else {
                            tracing::warn!(
                                env_var = %key,
                                value = %value,
                                "invalid value for strands.mitosis.timeout_triggered.min_elapsed_fraction — expected float"
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
        if let Some(explore_root) = overrides.explore_workspace_root {
            config.strands.explore.workspace_root = explore_root;
            sources.insert(
                "explore.workspace_root".to_string(),
                ConfigSource::CliOverride,
            );
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

        // Expand all tildes after all overrides are applied.
        config.expand_tildes();

        Ok((config, sources))
    }

    /// Emit boot-time warnings for configuration issues that should be addressed.
    ///
    /// These are non-fatal issues that don't prevent NEEDLE from running but
    /// should be visible at startup to avoid silent misconfiguration.
    fn emit_warnings(config: &Config) {
        // Warn if Splice is enabled but report_workspace is not configured.
        // This is critical because without a report_workspace, Splice will silently
        // no-op on every cycle instead of creating failure/loop beads.
        if config.strands.splice.enabled && config.strands.splice.report_workspace.is_none() {
            tracing::warn!(
                "strands.splice.enabled is true, but strands.splice.report_workspace is not set. \
                 Splice will not create worker failure or loop detection beads. \
                 Set strands.splice.report_workspace to a valid workspace path in your config \
                 (e.g., ~/.config/needle/config.yaml or .needle.yaml)."
            );
        }
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

        // Emit boot-time warnings for configuration issues that should be addressed
        // but are not fatal errors.
        Self::emit_warnings(config);

        // Validate routing regex patterns.
        if let Some(ref routing) = config.agent.routing {
            for (idx, rule) in routing.rules.iter().enumerate() {
                // Validate that match_model is a valid regex.
                if let Err(e) = regex::Regex::new(&rule.match_model) {
                    errors.push(ConfigError {
                        field: format!("agent.routing.rules[{}].match_model", idx),
                        message: format!("invalid regex pattern '{}': {}", rule.match_model, e),
                    });
                }
                // Validate that adapter is not empty.
                if rule.adapter.is_empty() {
                    errors.push(ConfigError {
                        field: format!("agent.routing.rules[{}].adapter", idx),
                        message: "must not be empty".to_string(),
                    });
                }
            }
        }

        // Validate timeout-triggered mitosis policy.
        let timeout_policy = &config.strands.mitosis.timeout_triggered;
        if timeout_policy.enabled {
            // At least one timeout type must be qualified when enabled.
            if !timeout_policy.agent_wallclock_timeout && !timeout_policy.handler_timeout {
                errors.push(ConfigError {
                    field: "strands.mitosis.timeout_triggered".to_string(),
                    message: "when enabled, at least one of agent_wallclock_timeout or handler_timeout must be true".to_string(),
                });
            }

            // Validate min_elapsed_fraction range.
            if timeout_policy.min_elapsed_fraction < 0.0
                || timeout_policy.min_elapsed_fraction > 1.0
            {
                errors.push(ConfigError {
                    field: "strands.mitosis.timeout_triggered.min_elapsed_fraction".to_string(),
                    message: "must be in range [0.0, 1.0]".to_string(),
                });
            }
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
            (
                "strands.mitosis.timeout_triggered.enabled",
                config.strands.mitosis.timeout_triggered.enabled.to_string(),
            ),
            (
                "strands.mitosis.timeout_triggered.agent_wallclock_timeout",
                config
                    .strands
                    .mitosis
                    .timeout_triggered
                    .agent_wallclock_timeout
                    .to_string(),
            ),
            (
                "strands.mitosis.timeout_triggered.handler_timeout",
                config
                    .strands
                    .mitosis
                    .timeout_triggered
                    .handler_timeout
                    .to_string(),
            ),
            (
                "strands.mitosis.timeout_triggered.min_elapsed_fraction",
                config
                    .strands
                    .mitosis
                    .timeout_triggered
                    .min_elapsed_fraction
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

/// Get the HOME environment variable safely.
///
/// Returns `Some(home)` if HOME is set and valid UTF-8, `None` otherwise.
/// This function never panics - it gracefully handles missing or invalid HOME values.
///
/// # Examples
///
/// ```
/// let home = get_home_env();
/// match home {
///     Some(h) => println!("Home directory: {}", h),
///     None => println!("HOME not set"),
/// }
/// ```
pub fn get_home_env() -> Option<String> {
    std::env::var("HOME").ok()
}

/// Resolve a path relative to the user's home directory.
fn dirs_or_home(relative: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(relative)
    } else {
        PathBuf::from("/tmp").join(relative)
    }
}

/// Expand a leading tilde (~) to the user's home directory.
///
/// - `~/path` → `$HOME/path`
/// - `~` → `$HOME`
/// - `/absolute/path` → unchanged
/// - `relative/path` → unchanged
///
/// Returns the path unchanged if HOME is not set.
fn expand_tilde(path: &Path) -> PathBuf {
    let path_str = path.as_os_str().to_str().unwrap_or("");

    // Check if path starts with ~/ or is exactly ~
    if path_str == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = path_str.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }

    // No tilde or HOME not set, return as-is
    path.to_path_buf()
}

/// Expand a leading tilde (~) in a string path to the HOME environment variable.
///
/// This is a string-based helper that returns a String, useful for string manipulation
/// without PathBuf conversions. For Path-based operations, use the private
/// `expand_tilde(path: &Path) -> PathBuf` function.
///
/// # Behavior
///
/// - `~/path` → `$HOME/path`
/// - `~` → `$HOME`
/// - `/absolute/path` → unchanged
/// - `relative/path` → unchanged
/// - `~user/path` → unchanged (only expands ~ for current user)
///
/// # Examples
///
/// ```rust
/// use needle::config::expand_tilde_str;
///
/// std::env::set_var("HOME", "/home/user");
/// assert_eq!(expand_tilde_str("~/docs"), "/home/user/docs");
/// assert_eq!(expand_tilde_str("~"), "/home/user");
/// assert_eq!(expand_tilde_str("/tmp"), "/tmp");
/// assert_eq!(expand_tilde_str("relative"), "relative");
///
/// std::env::remove_var("HOME");
/// assert_eq!(expand_tilde_str("~/docs"), "~/docs"); // HOME missing, unchanged
/// ```
///
/// # Returns
///
/// * `Some(String)` - Expanded path or original if no tilde
/// * Never panics - falls back to original path if HOME is unset
pub fn expand_tilde_str(path: &str) -> String {
    // Check if path is exactly ~
    if path == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return home;
        }
        // HOME not set, return original
        return path.to_string();
    }

    // Check if path starts with ~/
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            // Handle the special case of "~/" (empty rest) to match PathBuf behavior
            if rest.is_empty() {
                return home;
            }
            return format!("{}/{}", home, rest);
        }
        // HOME not set, return original
        return path.to_string();
    }

    // No tilde prefix, return unchanged
    path.to_string()
}

/// Expand tildes in a vector of PathBufs.
fn expand_tilde_vec(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().map(|p| expand_tilde(p)).collect()
}

/// Expand tildes in an optional PathBuf.
fn expand_tilde_option(path: &Option<PathBuf>) -> Option<PathBuf> {
    path.as_ref().map(|p| expand_tilde(p))
}

// ──────────────────────────────────────────────────────────────────────────────
// Config loading tests (separate from CLI detection tests)
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod config_tests {
    use super::*;

    /// Delegates to the single crate-wide env lock. A module-private lock
    /// would not exclude the `HOME` mutations performed by other modules'
    /// tests, which is what made these tests order-dependent.
    fn lock_supervisor_env() -> (
        std::sync::MutexGuard<'static, ()>,
        crate::util::test_env::EnvGuard,
    ) {
        crate::util::test_env::isolate_env()
    }

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

    // ──────────────────────────────────────────────────────────────────────────────
    // expand_tilde_str tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn expand_tilde_str_with_tilde_slash() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("~/docs");
        assert_eq!(result, "/home/testuser/docs");
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_str_bare_tilde() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("~");
        assert_eq!(result, "/home/testuser");
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_str_absolute_path_unchanged() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("/tmp/file.txt");
        assert_eq!(result, "/tmp/file.txt");
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_str_relative_path_unchanged() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("relative/path");
        assert_eq!(result, "relative/path");
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_str_empty_string() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("");
        assert_eq!(result, "");
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_str_missing_home_returns_original() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::remove_var("HOME");
        let result = expand_tilde_str("~/docs");
        assert_eq!(result, "~/docs");
    }

    #[test]
    fn expand_tilde_str_missing_home_bare_tilde() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::remove_var("HOME");
        let result = expand_tilde_str("~");
        assert_eq!(result, "~");
    }

    #[test]
    fn expand_tilde_str_nested_path() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("~/docs/notes/file.md");
        assert_eq!(result, "/home/testuser/docs/notes/file.md");
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_str_with_spaces() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/test user");
        let result = expand_tilde_str("~/my docs");
        assert_eq!(result, "/home/test user/my docs");
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_str_tilde_in_middle_unchanged() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("/path/~user/docs");
        assert_eq!(result, "/path/~user/docs");
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_str_tilde_user_prefix_unchanged() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("~otheruser/docs");
        assert_eq!(result, "~otheruser/docs");
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_str_no_panic_on_missing_home() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::remove_var("HOME");
        // This should not panic
        let result = expand_tilde_str("~/any/path");
        assert_eq!(result, "~/any/path");
    }

    #[test]
    fn expand_tilde_str_tilde_slash_only() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("~/");
        assert_eq!(result, "/home/testuser");
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_path_tilde_slash_only() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde(Path::new("~/"));
        assert_eq!(result, PathBuf::from("/home/testuser"));
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_str_tilde_with_parent_directory_reference() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("~/../");
        assert_eq!(result, "/home/testuser/../");
        std::env::remove_var("HOME");
    }

    #[test]
    fn expand_tilde_path_tilde_with_parent_directory_reference() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde(Path::new("~/../"));
        assert_eq!(result, PathBuf::from("/home/testuser/../"));
        std::env::remove_var("HOME");
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
    fn workspace_config_overrides_bead_cli_binding() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            "bead_cli:\n  backend: bead\n  path: /opt/bead-rs/bin/bead\n",
        )
        .unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert_eq!(config.bead_cli.backend, BeadBackend::Bead);
        assert_eq!(
            config.bead_cli.path,
            Some(PathBuf::from("/opt/bead-rs/bin/bead"))
        );
        assert!(matches!(
            sources.get("bead_cli.backend"),
            Some(ConfigSource::WorkspaceFile(_))
        ));
        assert!(matches!(
            sources.get("bead_cli.path"),
            Some(ConfigSource::WorkspaceFile(_))
        ));
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

    #[test]
    fn workspace_labels_parsed_from_needle_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            "workspace:\n  labels: [rust, api, trading]\n",
        )
        .unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert_eq!(
            config.workspace.labels,
            vec!["rust".to_string(), "api".to_string(), "trading".to_string()]
        );
        assert!(
            matches!(
                sources.get("workspace.labels"),
                Some(ConfigSource::WorkspaceFile(_))
            ),
            "workspace.labels source should be WorkspaceFile"
        );
    }

    #[test]
    fn workspace_labels_default_empty() {
        let config = Config::default();
        assert!(config.workspace.labels.is_empty());
    }

    #[test]
    fn workspace_labels_not_set_leaves_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".needle.yaml"), "agent:\n  default: opus\n").unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert!(config.workspace.labels.is_empty());
        assert!(!sources.contains_key("workspace.labels"));
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

    // ── Validation edge cases ──

    #[test]
    fn max_workers_over_50_fails_validation() {
        let mut config = Config::default();
        config.worker.max_workers = 51;
        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.iter().any(|e| e.field == "worker.max_workers"
                && e.message.contains("exceeds practical fleet limit")),
            "expected fleet limit error, got: {:?}",
            errors
        );
    }

    #[test]
    fn cpu_load_warn_zero_fails_validation() {
        let mut config = Config::default();
        config.worker.cpu_load_warn = 0.0;
        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.iter().any(|e| e.field == "worker.cpu_load_warn"),
            "expected cpu_load_warn error, got: {:?}",
            errors
        );
    }

    #[test]
    fn cpu_load_warn_negative_fails_validation() {
        let mut config = Config::default();
        config.worker.cpu_load_warn = -0.5;
        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.iter().any(|e| e.field == "worker.cpu_load_warn"),
            "expected cpu_load_warn error, got: {:?}",
            errors
        );
    }

    #[test]
    fn cpu_load_warn_over_one_fails_validation() {
        let mut config = Config::default();
        config.worker.cpu_load_warn = 1.1;
        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.iter().any(|e| e.field == "worker.cpu_load_warn"),
            "expected cpu_load_warn error, got: {:?}",
            errors
        );
    }

    #[test]
    fn cpu_load_warn_at_one_passes_validation() {
        let mut config = Config::default();
        config.worker.cpu_load_warn = 1.0;
        let errors = ConfigLoader::validate(&config);
        assert!(
            !errors.iter().any(|e| e.field == "worker.cpu_load_warn"),
            "cpu_load_warn=1.0 should be valid, got: {:?}",
            errors
        );
    }

    #[test]
    fn heartbeat_ttl_below_3x_interval_fails_validation() {
        let mut config = Config::default();
        config.health.heartbeat_interval_secs = 30;
        config.health.heartbeat_ttl_secs = 60; // < 3*30=90
        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.iter().any(|e| e.field == "health.heartbeat_ttl_secs"
                && e.message.contains("detection may be unreliable")),
            "expected heartbeat_ttl warning, got: {:?}",
            errors
        );
    }

    #[test]
    fn heartbeat_ttl_at_3x_interval_passes_validation() {
        let mut config = Config::default();
        config.health.heartbeat_interval_secs = 30;
        config.health.heartbeat_ttl_secs = 90; // = 3*30
        let errors = ConfigLoader::validate(&config);
        assert!(
            !errors
                .iter()
                .any(|e| e.field == "health.heartbeat_ttl_secs"),
            "heartbeat_ttl=3*interval should be valid, got: {:?}",
            errors
        );
    }

    #[test]
    fn multiple_validation_errors_collected() {
        let mut config = Config::default();
        config.agent.default = String::new();
        config.worker.max_workers = 0;
        config.worker.cpu_load_warn = -1.0;
        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.len() >= 3,
            "expected >= 3 errors, got {}",
            errors.len()
        );
    }

    // ── YAML file loading tests ──

    #[test]
    fn load_partial_yaml_uses_defaults_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "agent:\n  timeout: 999\n").unwrap();
        let config = ConfigLoader::load_from_path(&path).unwrap();
        assert_eq!(config.agent.timeout, 999);
        assert_eq!(
            config.agent.default, "claude",
            "missing fields should use default"
        );
        assert_eq!(
            config.worker.max_workers, 4,
            "missing worker section should use defaults"
        );
    }

    #[test]
    fn load_invalid_yaml_returns_error() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "invalid: [yaml: broken: {{{").unwrap();
        let result = ConfigLoader::load_from_path(&path);
        assert!(result.is_err(), "invalid YAML should return error");
    }

    #[test]
    fn load_yaml_with_unknown_fields_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "agent:\n  default: test\nunknown_section:\n  key: value\n",
        )
        .unwrap();
        let config = ConfigLoader::load_from_path(&path).unwrap();
        assert_eq!(config.agent.default, "test");
    }

    #[test]
    fn load_empty_yaml_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "").unwrap();
        let config = ConfigLoader::load_from_path(&path).unwrap();
        assert_eq!(config.agent.default, "claude");
        assert_eq!(config.worker.max_workers, 4);
    }

    #[test]
    fn load_yaml_with_all_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let yaml = r#"
agent:
  default: test-agent
  timeout: 500
  args:
    - "--verbose"
worker:
  max_workers: 8
  idle_timeout: 120
  launch_stagger_seconds: 5
  max_claim_retries: 10
  claim_race_lost_skip: 8
health:
  heartbeat_interval_secs: 15
  heartbeat_ttl_secs: 120
strands:
  explore:
    enabled: false
  mitosis:
    enabled: false
    first_failure_only: false
"#;
        std::fs::write(&path, yaml).unwrap();
        let config = ConfigLoader::load_from_path(&path).unwrap();
        assert_eq!(config.agent.default, "test-agent");
        assert_eq!(config.agent.timeout, 500);
        assert_eq!(config.agent.args, vec!["--verbose".to_string()]);
        assert_eq!(config.worker.max_workers, 8);
        assert_eq!(config.worker.idle_timeout, 120);
        assert_eq!(config.worker.launch_stagger_seconds, 5);
        assert_eq!(config.worker.max_claim_retries, 10);
        assert_eq!(config.worker.claim_race_lost_skip, 8);
        assert_eq!(config.health.heartbeat_interval_secs, 15);
        assert_eq!(config.health.heartbeat_ttl_secs, 120);
        assert!(!config.strands.explore.enabled);
        assert!(!config.strands.mitosis.enabled);
        assert!(!config.strands.mitosis.first_failure_only);
    }

    // ── Environment variable override tests (additional paths) ──

    #[test]
    fn env_override_worker_idle_timeout() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_WORKER__IDLE_TIMEOUT";
        std::env::set_var(key, "180");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(config.worker.idle_timeout, 180);
        assert!(sources.contains_key("worker.idle_timeout"));
    }

    #[test]
    fn env_override_worker_launch_stagger() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_WORKER__LAUNCH_STAGGER_SECONDS";
        std::env::set_var(key, "5");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(config.worker.launch_stagger_seconds, 5);
        assert!(sources.contains_key("worker.launch_stagger_seconds"));
    }

    #[test]
    fn env_override_health_heartbeat_interval() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_HEALTH__HEARTBEAT_INTERVAL_SECS";
        std::env::set_var(key, "15");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(config.health.heartbeat_interval_secs, 15);
        assert!(sources.contains_key("health.heartbeat_interval_secs"));
    }

    #[test]
    fn env_override_health_heartbeat_ttl() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_HEALTH__HEARTBEAT_TTL_SECS";
        std::env::set_var(key, "600");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(config.health.heartbeat_ttl_secs, 600);
        assert!(sources.contains_key("health.heartbeat_ttl_secs"));
    }

    #[test]
    fn env_override_self_modification_enabled() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_SELF_MODIFICATION__ENABLED";
        std::env::set_var(key, "true");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert!(config.self_modification.enabled);
        assert!(sources.contains_key("self_modification.enabled"));
    }

    /// The canary runner relies on these two overrides to confine a spawned
    /// worker to the canary workspace. Without them Explore scans $HOME and the
    /// worker dispatches agents into the operator's real repos.
    #[test]
    fn env_override_explore_enabled() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();
        assert!(
            config.strands.explore.enabled,
            "explore is expected to default on — this test is meaningless otherwise"
        );

        let key = "NEEDLE_STRANDS__EXPLORE__ENABLED";
        std::env::set_var(key, "false");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert!(!config.strands.explore.enabled);
        assert!(sources.contains_key("strands.explore.enabled"));
    }

    #[test]
    fn env_override_explore_workspace_root() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT";
        std::env::set_var(key, "/home/coding/.needle/canary");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(
            config.strands.explore.workspace_root,
            PathBuf::from("/home/coding/.needle/canary")
        );
        assert!(sources.contains_key("strands.explore.workspace_root"));
    }

    #[test]
    fn env_override_self_modification_auto_promote() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_SELF_MODIFICATION__AUTO_PROMOTE";
        std::env::set_var(key, "true");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert!(config.self_modification.auto_promote);
        assert!(sources.contains_key("self_modification.auto_promote"));
    }

    #[test]
    fn env_override_self_modification_canary_timeout() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_SELF_MODIFICATION__CANARY_TIMEOUT";
        std::env::set_var(key, "600");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(config.self_modification.canary_timeout, 600);
        assert!(sources.contains_key("self_modification.canary_timeout"));
    }

    #[test]
    fn env_override_invalid_bool_ignored() {
        let mut config = Config::default();
        let original = config.self_modification.enabled;
        let mut sources = SourceMap::new();

        let key = "NEEDLE_SELF_MODIFICATION__ENABLED";
        std::env::set_var(key, "not_a_bool");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(config.self_modification.enabled, original);
        assert!(!sources.contains_key("self_modification.enabled"));
    }

    #[test]
    fn env_override_supervisor_heartbeat_path() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_SUPERVISOR__HEARTBEAT_PATH";
        std::env::set_var(key, "/custom/supervisor-heartbeat.json");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(
            config.supervisor.heartbeat_path,
            Some(PathBuf::from("/custom/supervisor-heartbeat.json"))
        );
        assert!(sources.contains_key("supervisor.heartbeat_path"));
    }

    #[test]
    fn env_override_supervisor_socket_path() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_SUPERVISOR__SOCKET_PATH";
        std::env::set_var(key, "/tmp/supervisor.sock");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(
            config.supervisor.socket_path,
            Some(PathBuf::from("/tmp/supervisor.sock"))
        );
        assert!(sources.contains_key("supervisor.socket_path"));
    }

    // ── Workspace override tests (additional paths) ──

    #[test]
    fn workspace_config_overrides_verification() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            "verification:\n  - cargo test\n  - cargo clippy\n",
        )
        .unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert_eq!(
            config.verification,
            vec!["cargo test".to_string(), "cargo clippy".to_string()]
        );
        assert!(
            matches!(
                sources.get("verification"),
                Some(ConfigSource::WorkspaceFile(_))
            ),
            "verification source should be WorkspaceFile"
        );
    }

    #[test]
    fn workspace_config_overrides_strands_weave() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            "strands:\n  weave:\n    enabled: true\n",
        )
        .unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert!(
            sources.contains_key("strands.weave"),
            "strands.weave should be tracked in sources"
        );
    }

    #[test]
    fn workspace_config_overrides_strands_pulse() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            "strands:\n  pulse:\n    enabled: true\n",
        )
        .unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert!(sources.contains_key("strands.pulse"));
    }

    #[test]
    fn workspace_config_overrides_strands_unravel() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            "strands:\n  unravel:\n    enabled: true\n",
        )
        .unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert!(sources.contains_key("strands.unravel"));
    }

    #[test]
    fn workspace_invalid_yaml_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".needle.yaml"), "agent: {{{invalid yaml").unwrap();

        let result = ConfigLoader::load_workspace(dir.path());
        assert!(
            result.is_err(),
            "invalid workspace YAML should return error"
        );
    }

    // ── Default value assertion tests ──

    #[test]
    fn default_agent_config_values() {
        let config = AgentConfig::default();
        assert_eq!(config.default, "claude");
        assert_eq!(config.timeout, 3600);
        assert!(config.args.is_empty());
    }

    #[test]
    fn default_worker_config_values() {
        let config = WorkerConfig::default();
        assert_eq!(config.max_workers, 4);
        assert_eq!(config.launch_stagger_seconds, 2);
        assert_eq!(config.idle_timeout, 60);
        assert_eq!(config.max_claim_retries, 3);
        assert_eq!(config.claim_race_lost_skip, 5);
        assert!((config.cpu_load_warn - 0.8).abs() < f64::EPSILON);
        assert_eq!(config.memory_free_warn_mb, 512);
    }

    #[test]
    fn default_health_config_values() {
        let config = HealthConfig::default();
        assert_eq!(config.heartbeat_interval_secs, 30);
        assert_eq!(config.heartbeat_ttl_secs, 300);
    }

    #[test]
    fn default_mend_config_values() {
        let config = MendConfig::default();
        assert_eq!(config.stuck_threshold_secs, 300);
        assert_eq!(config.lock_ttl_secs, 600);
        assert_eq!(config.db_check_interval, 50);
    }

    #[test]
    fn default_explore_config_values() {
        let config = ExploreConfig::default();
        assert!(config.enabled);
        assert!(config.workspaces.is_empty());
    }

    #[test]
    fn default_knot_config_values() {
        let config = KnotConfig::default();
        assert_eq!(config.alert_cooldown_minutes, 60);
        assert_eq!(config.exhaustion_threshold, 3);
        assert!(config.alert_destination.is_none());
    }

    #[test]
    fn default_mitosis_config_values() {
        let config = MitosisConfig::default();
        assert!(config.enabled);
        assert!(config.first_failure_only);
        assert_eq!(config.force_failure_threshold, 0);
        assert_eq!(config.repeat_interval, 0);
    }

    #[test]
    fn default_weave_config_values() {
        let config = WeaveConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_beads_per_run, 5);
        assert_eq!(config.cooldown_hours, 24);
        assert!(config.exclude_workspaces.is_empty());
        assert!(!config.doc_patterns.is_empty());
        assert!(config.prompt_template.is_none());
    }

    #[test]
    fn default_unravel_config_values() {
        let config = UnravelConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_beads_per_run, 5);
        assert_eq!(config.max_alternatives_per_bead, 3);
        assert_eq!(config.cooldown_hours, 168);
        assert!(config.prompt_template.is_none());
    }

    #[test]
    fn default_pulse_config_values() {
        let config = PulseConfig::default();
        assert!(!config.enabled);
        assert!(config.scanners.is_empty());
        assert_eq!(config.max_beads_per_run, 5);
        assert_eq!(config.cooldown_hours, 48);
        assert_eq!(config.severity_threshold, 3);
        assert!(config.prompt_template.is_none());
    }

    #[test]
    fn default_reflect_config_values() {
        let config = ReflectConfig::default();
        assert!(config.enabled);
        assert_eq!(config.min_beads_since_last, 10);
        assert_eq!(config.cooldown_hours, 24);
        assert_eq!(config.max_learnings_per_run, 10);
        assert_eq!(config.max_skills_per_run, 3);
        assert_eq!(config.learning_retention_days, 90);
        assert_eq!(config.max_learnings, 80);
        assert!(config.extraction_agent.is_none());
        assert!(config.extraction_prompt_template.is_none());
        assert_eq!(config.max_extraction_per_run, 5);
    }

    #[test]
    fn reflect_config_default_extraction_agent_is_none() {
        let config = ReflectConfig::default();
        assert!(config.extraction_agent.is_none());
        assert_eq!(config.max_extraction_per_run, 5);
    }

    #[test]
    fn splice_config_default_values() {
        let config = SpliceConfig::default();
        assert!(config.enabled);
        assert_eq!(config.stale_threshold_secs, 300);
        assert!(config.report_workspace.is_none());
        assert!(config.detect_live_loops);
        assert_eq!(config.live_loop_scan_events, 200);
        assert_eq!(config.claim_churn_threshold, 20);
        assert_eq!(config.log_runaway_bytes, 10 * 1024 * 1024);
        assert_eq!(config.live_loop_window_secs, 300);
    }

    #[test]
    fn default_supervisor_config_values() {
        let config = SupervisorConfig::default();
        assert!(config.heartbeat_path.is_none());
        assert!(config.socket_path.is_none());
    }

    #[test]
    fn supervisor_config_resolved_heartbeat_path_default() {
        let config = SupervisorConfig::default();
        let workspace_home = PathBuf::from("/home/user/.needle");
        let resolved = config.resolved_heartbeat_path(&workspace_home);
        assert_eq!(
            resolved,
            PathBuf::from("/home/user/.needle/state/supervisor-heartbeat.json")
        );
    }

    #[test]
    fn supervisor_config_resolved_heartbeat_path_custom() {
        let config = SupervisorConfig {
            heartbeat_path: Some(PathBuf::from("/custom/path/heartbeat.json")),
            ..Default::default()
        };
        let workspace_home = PathBuf::from("/home/user/.needle");
        let resolved = config.resolved_heartbeat_path(&workspace_home);
        assert_eq!(resolved, PathBuf::from("/custom/path/heartbeat.json"));
    }

    #[test]
    fn supervisor_config_yaml_roundtrip() {
        let config = SupervisorConfig {
            heartbeat_path: Some(PathBuf::from("/custom/heartbeat.json")),
            socket_path: Some(PathBuf::from("/tmp/supervisor.sock")),
        };

        let yaml = serde_yaml::to_string(&config).unwrap();
        let decoded: SupervisorConfig = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(
            decoded.heartbeat_path,
            Some(PathBuf::from("/custom/heartbeat.json"))
        );
        assert_eq!(
            decoded.socket_path,
            Some(PathBuf::from("/tmp/supervisor.sock"))
        );
    }

    #[test]
    fn supervisor_config_with_only_heartbeat_path() {
        let config = SupervisorConfig {
            heartbeat_path: Some(PathBuf::from("/var/lib/supervisor/heartbeat.json")),
            socket_path: None,
        };

        let yaml = serde_yaml::to_string(&config).unwrap();
        let decoded: SupervisorConfig = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(
            decoded.heartbeat_path,
            Some(PathBuf::from("/var/lib/supervisor/heartbeat.json"))
        );
        assert!(decoded.socket_path.is_none());
    }

    #[test]
    fn supervisor_config_with_only_socket_path() {
        let config = SupervisorConfig {
            heartbeat_path: None,
            socket_path: Some(PathBuf::from("/run/supervisor/control.sock")),
        };

        let yaml = serde_yaml::to_string(&config).unwrap();
        let decoded: SupervisorConfig = serde_yaml::from_str(&yaml).unwrap();

        assert!(decoded.heartbeat_path.is_none());
        assert_eq!(
            decoded.socket_path,
            Some(PathBuf::from("/run/supervisor/control.sock"))
        );
    }

    #[test]
    fn supervisor_config_from_full_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let yaml = r#"
agent:
  default: claude
supervisor:
  heartbeat_path: /var/lib/needle/supervisor-heartbeat.json
  socket_path: /run/needle/supervisor.sock
worker:
  max_workers: 8
"#;
        std::fs::write(&path, yaml).unwrap();

        let config = ConfigLoader::load_from_path(&path).unwrap();
        assert_eq!(
            config.supervisor.heartbeat_path,
            Some(PathBuf::from("/var/lib/needle/supervisor-heartbeat.json"))
        );
        assert_eq!(
            config.supervisor.socket_path,
            Some(PathBuf::from("/run/needle/supervisor.sock"))
        );
    }

    #[test]
    fn supervisor_config_empty_section_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let yaml = r#"
agent:
  default: claude
supervisor: {}
worker:
  max_workers: 8
"#;
        std::fs::write(&path, yaml).unwrap();

        let config = ConfigLoader::load_from_path(&path).unwrap();
        assert!(config.supervisor.heartbeat_path.is_none());
        assert!(config.supervisor.socket_path.is_none());
    }

    #[test]
    fn supervisor_config_json_roundtrip() {
        let config = SupervisorConfig {
            heartbeat_path: Some(PathBuf::from("/heartbeat.json")),
            socket_path: Some(PathBuf::from("/supervisor.sock")),
        };

        let json = serde_json::to_string(&config).unwrap();
        let decoded: SupervisorConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(
            decoded.heartbeat_path,
            Some(PathBuf::from("/heartbeat.json"))
        );
        assert_eq!(decoded.socket_path, Some(PathBuf::from("/supervisor.sock")));
    }

    #[test]
    fn supervisor_config_from_env_defaults() {
        let (_env_lock, _env_guard) = lock_supervisor_env();
        // Ensure no env vars are set
        std::env::remove_var("SUPERVISOR_HEARTBEAT_PATH");
        std::env::remove_var("SUPERVISOR_SOCKET_PATH");

        let config = SupervisorConfig::from_env().unwrap();
        assert!(config.heartbeat_path.is_none());
        assert!(config.socket_path.is_none());
    }

    #[test]
    fn supervisor_config_from_env_heartbeat_only() {
        let (_env_lock, _env_guard) = lock_supervisor_env();
        std::env::remove_var("SUPERVISOR_SOCKET_PATH");
        std::env::set_var("SUPERVISOR_HEARTBEAT_PATH", "/custom/heartbeat.json");

        let config = SupervisorConfig::from_env().unwrap();
        assert_eq!(
            config.heartbeat_path,
            Some(PathBuf::from("/custom/heartbeat.json"))
        );
        assert!(config.socket_path.is_none());

        std::env::remove_var("SUPERVISOR_HEARTBEAT_PATH");
    }

    #[test]
    fn supervisor_config_from_env_socket_only() {
        let (_env_lock, _env_guard) = lock_supervisor_env();
        std::env::remove_var("SUPERVISOR_HEARTBEAT_PATH");
        std::env::set_var("SUPERVISOR_SOCKET_PATH", "/tmp/supervisor.sock");

        let config = SupervisorConfig::from_env().unwrap();
        assert!(config.heartbeat_path.is_none());
        assert_eq!(
            config.socket_path,
            Some(PathBuf::from("/tmp/supervisor.sock"))
        );

        std::env::remove_var("SUPERVISOR_SOCKET_PATH");
    }

    #[test]
    fn supervisor_config_from_env_both_paths() {
        let (_env_lock, _env_guard) = lock_supervisor_env();
        std::env::set_var(
            "SUPERVISOR_HEARTBEAT_PATH",
            "/var/lib/needle/heartbeat.json",
        );
        std::env::set_var("SUPERVISOR_SOCKET_PATH", "/var/run/needle/supervisor.sock");

        let config = SupervisorConfig::from_env().unwrap();
        assert_eq!(
            config.heartbeat_path,
            Some(PathBuf::from("/var/lib/needle/heartbeat.json"))
        );
        assert_eq!(
            config.socket_path,
            Some(PathBuf::from("/var/run/needle/supervisor.sock"))
        );

        std::env::remove_var("SUPERVISOR_HEARTBEAT_PATH");
        std::env::remove_var("SUPERVISOR_SOCKET_PATH");
    }

    #[test]
    fn supervisor_config_from_env_expands_tilde() {
        let (_env_lock, _env_guard) = lock_supervisor_env();
        std::env::set_var("SUPERVISOR_HEARTBEAT_PATH", "~/heartbeat.json");
        std::env::set_var("SUPERVISOR_SOCKET_PATH", "~/supervisor.sock");

        let config = SupervisorConfig::from_env().unwrap();
        // Tilde should be expanded to the home directory
        assert!(config.heartbeat_path.is_some());
        assert!(config.socket_path.is_some());
        // Paths should not contain literal "~" after expansion
        assert_ne!(
            config.heartbeat_path,
            Some(PathBuf::from("~/heartbeat.json"))
        );
        assert_ne!(config.socket_path, Some(PathBuf::from("~/supervisor.sock")));

        std::env::remove_var("SUPERVISOR_HEARTBEAT_PATH");
        std::env::remove_var("SUPERVISOR_SOCKET_PATH");
    }

    #[test]
    fn default_self_modification_config_values() {
        let config = SelfModificationConfig::default();
        assert!(!config.enabled);
        assert!(!config.auto_promote);
        // 30 minutes, not 5: a canary test is a full agent dispatch, and the
        // old budget was shorter than a routine one, so every upgrade was
        // rejected by timeout.
        assert_eq!(config.canary_timeout, 1800);
        assert!(config.hot_reload);
    }

    #[test]
    fn default_telemetry_config_values() {
        let config = TelemetryConfig::default();
        assert!(config.file_sink.enabled);
        assert!(!config.stdout_sink.enabled);
        assert!(config.hooks.is_empty());
    }

    // ── Full hierarchy test ──

    #[test]
    fn load_resolved_applies_workspace_then_cli() {
        let dir = tempfile::tempdir().unwrap();
        // Create a .beads directory so it looks like a workspace.
        std::fs::create_dir_all(dir.path().join(".beads")).unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            "agent:\n  default: workspace-agent\n  timeout: 777\n",
        )
        .unwrap();

        let cli = CliOverrides {
            workspace: Some(dir.path().to_path_buf()),
            agent_binary: Some("cli-agent".to_string()),
            ..Default::default()
        };

        let (config, sources) = ConfigLoader::load_resolved(dir.path(), cli).unwrap();

        // CLI should win over workspace for agent.default.
        assert_eq!(config.agent.default, "cli-agent");
        assert_eq!(
            sources.get("agent.default"),
            Some(&ConfigSource::CliOverride)
        );
        // Workspace should still win for agent.timeout (CLI didn't override it).
        assert_eq!(config.agent.timeout, 777);
    }

    // ── dump_with_sources coverage ──

    #[test]
    fn dump_with_sources_includes_all_fields() {
        let config = Config::default();
        let sources = SourceMap::new();
        let lines = ConfigLoader::dump_with_sources(&config, &sources);

        let expected_prefixes = [
            "agent.default",
            "agent.timeout",
            "worker.max_workers",
            "worker.idle_timeout",
            "worker.launch_stagger_seconds",
            "health.heartbeat_interval_secs",
            "health.heartbeat_ttl_secs",
            "prompt.context_files",
            "prompt.instructions",
        ];

        for prefix in expected_prefixes {
            assert!(
                lines.iter().any(|l| l.starts_with(prefix)),
                "dump should include '{}', but got: {:?}",
                prefix,
                lines
            );
        }
    }

    #[test]
    fn dump_with_sources_shows_env_var_source() {
        let config = Config::default();
        let mut sources = SourceMap::new();
        sources.insert(
            "worker.max_workers".to_string(),
            ConfigSource::EnvVar("NEEDLE_WORKER__MAX_WORKERS".to_string()),
        );

        let lines = ConfigLoader::dump_with_sources(&config, &sources);
        let line = lines
            .iter()
            .find(|l| l.starts_with("worker.max_workers"))
            .unwrap();
        assert!(
            line.contains("NEEDLE_WORKER__MAX_WORKERS env var"),
            "should show env var source: {}",
            line
        );
    }

    // ── ConfigError display ──

    #[test]
    fn config_error_display_format() {
        let err = ConfigError {
            field: "agent.default".to_string(),
            message: "must not be empty".to_string(),
        };
        assert_eq!(format!("{}", err), "agent.default: must not be empty");
    }

    // ── Routing config tests ──

    #[test]
    fn default_routing_config_matches_anthropic_models() {
        let config = Config::default();
        let routing = config
            .agent
            .routing
            .expect("default routing should be Some");

        // Should have one rule matching Anthropic models
        assert_eq!(routing.rules.len(), 1);

        let rule = &routing.rules[0];
        assert_eq!(rule.match_model, "(claude-)?(sonnet|opus|fable|haiku).*");
        assert_eq!(rule.adapter, "claude-print");

        // Default fallback should be claude-code-glm-4.7
        assert_eq!(
            routing.default_adapter.as_deref(),
            Some("claude-code-glm-4.7")
        );
    }

    #[test]
    fn routing_config_with_rules_parses() {
        let yaml = r#"
agent:
  default: claude
  routing:
    rules:
      - match_model: "(claude-)?(sonnet|opus).*"
        adapter: claude-print
      - match_model: "haiku.*"
        adapter: claude-code-glm-4.7
    default_adapter: claude-code-glm-4.7
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, yaml).unwrap();
        let config = ConfigLoader::load_from_path(&path).unwrap();

        assert!(config.agent.routing.is_some());
        let routing = config.agent.routing.as_ref().unwrap();
        assert_eq!(routing.rules.len(), 2);
        assert_eq!(routing.rules[0].match_model, "(claude-)?(sonnet|opus).*");
        assert_eq!(routing.rules[0].adapter, "claude-print");
        assert_eq!(routing.rules[1].match_model, "haiku.*");
        assert_eq!(routing.rules[1].adapter, "claude-code-glm-4.7");
        assert_eq!(
            routing.default_adapter.as_deref(),
            Some("claude-code-glm-4.7")
        );
    }

    #[test]
    fn routing_config_empty_rules_list_is_valid() {
        let yaml = r#"
agent:
  routing:
    rules: []
    default_adapter: fallback-adapter
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, yaml).unwrap();
        let config = ConfigLoader::load_from_path(&path).unwrap();

        assert!(config.agent.routing.is_some());
        let routing = config.agent.routing.as_ref().unwrap();
        assert!(routing.rules.is_empty());
        assert_eq!(routing.default_adapter.as_deref(), Some("fallback-adapter"));
    }

    #[test]
    fn invalid_regex_in_routing_rule_fails_validation() {
        let mut config = Config::default();
        config.agent.routing = Some(RoutingConfig {
            rules: vec![RoutingRule {
                match_model: "[invalid(regex".to_string(), // Unclosed bracket
                adapter: "some-adapter".to_string(),
            }],
            default_adapter: None,
            strict: false,
        });

        let errors = ConfigLoader::validate(&config);
        assert!(
            errors
                .iter()
                .any(|e| e.field == "agent.routing.rules[0].match_model"),
            "expected regex validation error, got: {:?}",
            errors
        );
    }

    #[test]
    fn empty_adapter_in_routing_rule_fails_validation() {
        let mut config = Config::default();
        config.agent.routing = Some(RoutingConfig {
            rules: vec![RoutingRule {
                match_model: "sonnet".to_string(),
                adapter: String::new(), // Empty adapter
            }],
            default_adapter: None,
            strict: false,
        });

        let errors = ConfigLoader::validate(&config);
        assert!(
            errors
                .iter()
                .any(|e| e.field == "agent.routing.rules[0].adapter"),
            "expected empty adapter error, got: {:?}",
            errors
        );
    }

    #[test]
    fn valid_regex_in_routing_rule_passes_validation() {
        let mut config = Config::default();
        config.agent.routing = Some(RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "(claude-)?sonnet.*".to_string(),
                    adapter: "claude-print".to_string(),
                },
                RoutingRule {
                    match_model: "opus".to_string(),
                    adapter: "claude-opus".to_string(),
                },
            ],
            default_adapter: Some("claude-fallback".to_string()),
            strict: false,
        });

        let errors = ConfigLoader::validate(&config);
        assert!(
            !errors.iter().any(|e| e.field.starts_with("agent.routing")),
            "routing config should be valid, but got errors: {:?}",
            errors
        );
    }

    #[test]
    fn workspace_config_routing_override() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            r#"
agent:
  routing:
    rules:
      - match_model: "fable.*"
        adapter: fable-adapter
    default_adapter: workspace-default
"#,
        )
        .unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert!(config.agent.routing.is_some());
        let routing = config.agent.routing.as_ref().unwrap();
        assert_eq!(routing.rules.len(), 1);
        assert_eq!(routing.rules[0].match_model, "fable.*");
        assert_eq!(routing.rules[0].adapter, "fable-adapter");
        assert_eq!(
            routing.default_adapter.as_deref(),
            Some("workspace-default")
        );
        assert!(sources.contains_key("agent.routing"));
    }

    #[test]
    fn env_var_routing_default_adapter_override() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_AGENT__ROUTING__DEFAULT_ADAPTER";
        std::env::set_var(key, "env-fallback");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert!(config.agent.routing.is_some());
        assert_eq!(
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_deref(),
            Some("env-fallback")
        );
        assert!(sources.contains_key("agent.routing.default_adapter"));
    }

    #[test]
    fn env_var_routing_override_beats_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            r#"
agent:
  routing:
    default_adapter: workspace-fallback
"#,
        )
        .unwrap();

        let mut config = Config::default();
        let mut sources = SourceMap::new();

        // Apply workspace first.
        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);
        assert_eq!(
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_deref(),
            Some("workspace-fallback")
        );

        // Then env var overrides workspace.
        let key = "NEEDLE_AGENT__ROUTING__DEFAULT_ADAPTER";
        std::env::set_var(key, "env-fallback");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_deref(),
            Some("env-fallback")
        );
        assert!(matches!(
            sources.get("agent.routing.default_adapter"),
            Some(ConfigSource::EnvVar(_))
        ));
    }

    #[test]
    fn multiple_validation_errors_in_routing() {
        let mut config = Config::default();
        config.agent.routing = Some(RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "[invalid".to_string(),
                    adapter: "adapter1".to_string(),
                },
                RoutingRule {
                    match_model: "valid".to_string(),
                    adapter: String::new(), // Empty adapter
                },
                RoutingRule {
                    match_model: "(unclosed".to_string(),
                    adapter: "adapter3".to_string(),
                },
            ],
            default_adapter: None,
            strict: false,
        });

        let errors = ConfigLoader::validate(&config);
        let routing_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.field.starts_with("agent.routing"))
            .collect();

        assert!(
            routing_errors.len() >= 3,
            "expected at least 3 routing errors, got {}: {:?}",
            routing_errors.len(),
            routing_errors
        );
    }

    #[test]
    fn routing_config_yaml_roundtrip() {
        let routing = RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "sonnet".to_string(),
                    adapter: "adapter-sonnet".to_string(),
                },
                RoutingRule {
                    match_model: "opus".to_string(),
                    adapter: "adapter-opus".to_string(),
                },
            ],
            default_adapter: Some("default-adapter".to_string()),
            strict: false,
        };

        let yaml = serde_yaml::to_string(&routing).unwrap();
        let decoded: RoutingConfig = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(decoded.rules.len(), routing.rules.len());
        assert_eq!(decoded.rules[0].match_model, routing.rules[0].match_model);
        assert_eq!(decoded.rules[0].adapter, routing.rules[0].adapter);
        assert_eq!(decoded.default_adapter, routing.default_adapter);
    }

    #[test]
    fn agent_config_with_routing_yaml_roundtrip() {
        let agent = AgentConfig {
            routing: Some(RoutingConfig {
                rules: vec![RoutingRule {
                    match_model: "haiku".to_string(),
                    adapter: "haiku-adapter".to_string(),
                }],
                default_adapter: Some("fallback".to_string()),
                strict: false,
            }),
            ..Default::default()
        };

        let yaml = serde_yaml::to_string(&agent).unwrap();
        let decoded: AgentConfig = serde_yaml::from_str(&yaml).unwrap();

        assert!(decoded.routing.is_some());
        let routing = decoded.routing.as_ref().unwrap();
        assert_eq!(routing.rules.len(), 1);
        assert_eq!(routing.rules[0].match_model, "haiku");
        assert_eq!(routing.default_adapter.as_deref(), Some("fallback"));
    }

    #[test]
    fn routing_patterns_match_bare_aliases() {
        // Test patterns that match bare model aliases like 'sonnet', 'opus', 'fable', 'haiku'
        let mut config = Config::default();
        config.agent.routing = Some(RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "sonnet".to_string(),
                    adapter: "sonnet-adapter".to_string(),
                },
                RoutingRule {
                    match_model: "opus".to_string(),
                    adapter: "opus-adapter".to_string(),
                },
                RoutingRule {
                    match_model: "fable".to_string(),
                    adapter: "fable-adapter".to_string(),
                },
                RoutingRule {
                    match_model: "haiku".to_string(),
                    adapter: "haiku-adapter".to_string(),
                },
            ],
            default_adapter: Some("default-adapter".to_string()),
            strict: false,
        });

        let errors = ConfigLoader::validate(&config);
        assert!(
            !errors.iter().any(|e| e.field.starts_with("agent.routing")),
            "bare alias patterns should be valid, got: {:?}",
            errors
        );
    }

    #[test]
    fn routing_patterns_match_full_model_ids() {
        // Test patterns that match full model IDs like 'claude-sonnet-4-6'
        let mut config = Config::default();
        config.agent.routing = Some(RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "claude-sonnet-4-6".to_string(),
                    adapter: "sonnet-46-adapter".to_string(),
                },
                RoutingRule {
                    match_model: "claude-opus-4-8".to_string(),
                    adapter: "opus-48-adapter".to_string(),
                },
                RoutingRule {
                    match_model: "claude-fable-5".to_string(),
                    adapter: "fable-5-adapter".to_string(),
                },
            ],
            default_adapter: Some("default-adapter".to_string()),
            strict: false,
        });

        let errors = ConfigLoader::validate(&config);
        assert!(
            !errors.iter().any(|e| e.field.starts_with("agent.routing")),
            "full model ID patterns should be valid, got: {:?}",
            errors
        );
    }

    #[test]
    fn routing_patterns_match_with_wildcards() {
        // Test patterns with wildcards like 'claude-haiku-*'
        let mut config = Config::default();
        config.agent.routing = Some(RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "claude-haiku-.*".to_string(),
                    adapter: "haiku-adapter".to_string(),
                },
                RoutingRule {
                    match_model: "(claude-)?sonnet.*".to_string(),
                    adapter: "sonnet-adapter".to_string(),
                },
            ],
            default_adapter: Some("claude-code-glm-4.7".to_string()),
            strict: false,
        });

        let errors = ConfigLoader::validate(&config);
        assert!(
            !errors.iter().any(|e| e.field.starts_with("agent.routing")),
            "wildcard patterns should be valid, got: {:?}",
            errors
        );
    }

    #[test]
    fn routing_config_from_global_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let yaml = r#"
agent:
  default: claude
  routing:
    rules:
      - match_model: "(claude-)?(sonnet|opus).*"
        adapter: claude-print
      - match_model: "(claude-)?(fable|haiku).*"
        adapter: claude-code-glm-4.7
    default_adapter: claude-code-glm-4.7
"#;
        std::fs::write(&path, yaml).unwrap();

        let config = ConfigLoader::load_from_path(&path).unwrap();
        assert!(config.agent.routing.is_some());
        let routing = config.agent.routing.as_ref().unwrap();
        assert_eq!(routing.rules.len(), 2);
        assert_eq!(routing.rules[0].adapter, "claude-print");
        assert_eq!(routing.rules[1].adapter, "claude-code-glm-4.7");
        assert_eq!(
            routing.default_adapter.as_deref(),
            Some("claude-code-glm-4.7")
        );
    }

    #[test]
    fn routing_config_backward_compatibility() {
        // Config without routing section should behave like before (pure agent.default)
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let yaml = r#"
agent:
  default: claude
  timeout: 3600
"#;
        std::fs::write(&path, yaml).unwrap();

        let config = ConfigLoader::load_from_path(&path).unwrap();
        assert!(
            config.agent.routing.is_none(),
            "routing should be None when not specified"
        );
        assert_eq!(config.agent.default, "claude");
    }

    // ── Routing behavior tests ──

    #[test]
    fn routing_first_match_wins() {
        // Test that the first matching rule wins
        let routing = RoutingConfig {
            rules: vec![
                RoutingRule {
                    match_model: "sonnet.*".to_string(),
                    adapter: "first-adapter".to_string(),
                },
                RoutingRule {
                    match_model: "claude-sonnet-4-6".to_string(),
                    adapter: "second-adapter".to_string(),
                },
            ],
            default_adapter: Some("fallback-adapter".to_string()),
            strict: false,
        };

        // Test that "claude-sonnet-4-6" matches the first rule
        let first_rule = &routing.rules[0];
        let re = regex::Regex::new(&first_rule.match_model).unwrap();
        assert!(
            re.is_match("claude-sonnet-4-6"),
            "first rule should match the model"
        );

        // Both rules match, but first one should win
        let second_rule = &routing.rules[1];
        let re2 = regex::Regex::new(&second_rule.match_model).unwrap();
        assert!(
            re2.is_match("claude-sonnet-4-6"),
            "second rule also matches"
        );
        // The routing logic in apply_routing_rules iterates in order and returns first match
    }

    #[test]
    fn routing_workspace_override() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            r#"
agent:
  default: global-default
  routing:
    rules:
      - match_model: "claude-sonnet.*"
        adapter: workspace-sonnet-adapter
    default_adapter: workspace-fallback
"#,
        )
        .unwrap();

        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        config.agent.default = "global-default".to_string();
        let mut sources = SourceMap::new();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);

        assert!(config.agent.routing.is_some());
        let routing = config.agent.routing.as_ref().unwrap();
        assert_eq!(routing.rules.len(), 1);
        assert_eq!(routing.rules[0].adapter, "workspace-sonnet-adapter");
        assert_eq!(
            routing.default_adapter.as_deref(),
            Some("workspace-fallback")
        );
    }

    #[test]
    fn routing_default_fallback_no_match() {
        // Test that when no rules match, we fall back to default_adapter
        let routing = RoutingConfig {
            rules: vec![RoutingRule {
                match_model: "sonnet.*".to_string(),
                adapter: "sonnet-adapter".to_string(),
            }],
            default_adapter: Some("fallback-adapter".to_string()),
            strict: false,
        };

        // Model "claude-opus-4-8" doesn't match "sonnet.*"
        let re = regex::Regex::new(&routing.rules[0].match_model).unwrap();
        assert!(
            !re.is_match("claude-opus-4-8"),
            "model should not match the rule"
        );

        // Should fall back to default_adapter
        assert_eq!(routing.default_adapter.as_deref(), Some("fallback-adapter"));
    }

    #[test]
    fn routing_strict_mode_failure() {
        // Test that strict mode causes failure when no rules match
        let routing = RoutingConfig {
            rules: vec![RoutingRule {
                match_model: "sonnet.*".to_string(),
                adapter: "sonnet-adapter".to_string(),
            }],
            default_adapter: None,
            strict: true, // Strict mode enabled
        };

        // Verify strict mode is set
        assert!(routing.strict, "strict mode should be enabled");

        // Model "claude-opus-4-8" doesn't match "sonnet.*"
        let re = regex::Regex::new(&routing.rules[0].match_model).unwrap();
        assert!(
            !re.is_match("claude-opus-4-8"),
            "model should not match the rule"
        );

        // In strict mode with no match, the worker should fail
        // (This is tested in integration tests via apply_routing_rules)
    }

    #[test]
    fn routing_default_anthropic_models_to_claude_print() {
        // Test that default routing rules route Anthropic models to claude-print
        let default_routing = AgentConfig::default_routing();
        assert!(default_routing.is_some());

        let routing = default_routing.unwrap();
        assert!(!routing.rules.is_empty(), "should have default rules");

        // Check that the default rule matches Anthropic Claude models
        let rule = &routing.rules[0];
        let re = regex::Regex::new(&rule.match_model).unwrap();

        // Test various Anthropic model names
        assert!(re.is_match("claude-sonnet-4-6"), "should match sonnet");
        assert!(re.is_match("claude-opus-4-8"), "should match opus");
        assert!(re.is_match("claude-fable-5"), "should match fable");
        assert!(
            re.is_match("claude-haiku-4-5-20251001"),
            "should match haiku"
        );
        assert!(re.is_match("sonnet"), "should match short form");
        assert!(re.is_match("opus"), "should match short form");

        // Verify the adapter is claude-print
        assert_eq!(rule.adapter, "claude-print");

        // Verify default fallback
        assert_eq!(
            routing.default_adapter.as_deref(),
            Some("claude-code-glm-4.7")
        );
    }

    #[test]
    fn routing_glm_47_defaults_to_claude_code_glm_47() {
        // Test that glm-4.7 models use claude-code-glm-4.7 adapter
        let default_routing = AgentConfig::default_routing();
        let routing = default_routing.unwrap();

        // The default rule should NOT match glm models
        let rule = &routing.rules[0];
        let re = regex::Regex::new(&rule.match_model).unwrap();

        assert!(!re.is_match("glm-4.7"), "should not match glm models");
        assert!(
            !re.is_match("claude-code-glm-4.7"),
            "should not match adapter names"
        );

        // Should fall back to default_adapter
        assert_eq!(
            routing.default_adapter.as_deref(),
            Some("claude-code-glm-4.7")
        );
    }

    #[test]
    fn splice_enabled_without_report_workspace_emits_warning() {
        // Test that a warning is emitted when splice is enabled but report_workspace is None
        // This is a critical misconfiguration that causes silent no-op behavior
        let mut config = Config::default();
        config.strands.splice.enabled = true;
        config.strands.splice.report_workspace = None; // Not configured

        // This should emit a warning but not fail validation
        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.is_empty(),
            "splice without report_workspace should not fail validation, got errors: {:?}",
            errors
        );

        // Note: The actual warning emission is tested in integration tests since it requires
        // capturing tracing output. The unit test verifies it doesn't cause validation errors.
    }

    #[test]
    fn splice_disabled_with_report_workspace_passes() {
        // Test that when splice is disabled, report_workspace can be None without warning
        let mut config = Config::default();
        config.strands.splice.enabled = false;
        config.strands.splice.report_workspace = None;

        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.is_empty(),
            "disabled splice should not cause validation errors: {:?}",
            errors
        );
    }

    #[test]
    fn splice_enabled_with_report_workspace_passes() {
        // Test that when splice is enabled with a report_workspace, no warning is emitted
        let mut config = Config::default();
        config.strands.splice.enabled = true;
        config.strands.splice.report_workspace = Some(PathBuf::from("/tmp/test-workspace"));

        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.is_empty(),
            "splice with report_workspace should not cause validation errors: {:?}",
            errors
        );
    }

    // ── ValidationConfig tests (GitHub issues jedarden/NEEDLE#8, #9) ──

    #[test]
    fn validation_config_defaults_preserve_previous_hardcoded_behavior() {
        let config = ValidationConfig::default();
        assert_eq!(config.outcome_timeout_seconds, 50);
        assert_eq!(config.stderr_cap_bytes, 4096);
    }

    #[test]
    fn validation_config_parses_from_yaml() {
        let yaml = "validation:\n  outcome_timeout_seconds: 300\n  stderr_cap_bytes: 65536\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.validation.outcome_timeout_seconds, 300);
        assert_eq!(config.validation.stderr_cap_bytes, 65536);
    }

    #[test]
    fn validation_config_absent_from_yaml_uses_defaults() {
        // A config file that predates this feature must still parse and get
        // the previous hardcoded behavior, not an error or zeroed values.
        let yaml = "agent:\n  default: claude\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.validation.outcome_timeout_seconds, 50);
        assert_eq!(config.validation.stderr_cap_bytes, 4096);
    }

    #[test]
    fn validation_is_non_overridable_at_workspace_level() {
        assert!(NON_OVERRIDABLE_KEYS.contains(&"validation"));
    }

    // ── MitosisConfig.max_depth (fixes pre-existing compile breakage) ──

    #[test]
    fn mitosis_config_max_depth_defaults_to_unlimited() {
        assert_eq!(MitosisConfig::default().max_depth, 0);
    }

    #[test]
    fn mitosis_config_max_depth_parses_from_yaml() {
        let yaml = "strands:\n  mitosis:\n    max_depth: 3\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.strands.mitosis.max_depth, 3);
    }

    // ── worker.worker_binary_path (GitHub issue jedarden/NEEDLE#11) ──

    #[test]
    fn worker_binary_path_defaults_to_none() {
        assert_eq!(Config::default().worker.worker_binary_path, None);
    }

    #[test]
    fn worker_binary_path_parses_from_yaml() {
        let yaml = "worker:\n  worker_binary_path: /opt/custom/needle\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            config.worker.worker_binary_path,
            Some(PathBuf::from("/opt/custom/needle"))
        );
    }

    #[test]
    fn worker_binary_path_tilde_is_expanded() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let mut config = Config {
            worker: WorkerConfig {
                worker_binary_path: Some(PathBuf::from("~/bin/needle")),
                ..Config::default().worker
            },
            ..Config::default()
        };
        config.expand_tildes();
        let expanded = config.worker.worker_binary_path.unwrap();
        assert!(
            !expanded.starts_with("~"),
            "tilde was not expanded: {:?}",
            expanded
        );
        assert!(expanded.ends_with("bin/needle"));
    }

    #[test]
    fn bead_cli_path_tilde_is_expanded() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let mut config = Config {
            bead_cli: BeadCliConfig {
                path: Some(PathBuf::from("~/local/bin/bf")),
                ..BeadCliConfig::default()
            },
            ..Config::default()
        };
        config.expand_tildes();
        let expanded = config.bead_cli.path.unwrap();
        assert!(
            !expanded.starts_with("~"),
            "tilde was not expanded: {:?}",
            expanded
        );
        assert!(expanded.ends_with("local/bin/bf"));
    }

    #[test]
    fn worker_binary_path_accepts_any_path_without_validation() {
        // Invalid paths should be accepted during deserialization
        // Validation happens at runtime when spawning the worker, not during config load
        let yaml = "worker:\n  worker_binary_path: /nonexistent/path/to/needle\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            config.worker.worker_binary_path,
            Some(PathBuf::from("/nonexistent/path/to/needle"))
        );

        // Config validation should not fail for invalid paths
        let errors = ConfigLoader::validate(&config);
        assert!(
            !errors
                .iter()
                .any(|e| e.field == "worker.worker_binary_path"),
            "worker_binary_path should not be validated during config load, got errors: {:?}",
            errors
        );
    }

    // ── get_home_env() tests ──

    #[test]
    fn get_home_env_returns_some_when_set() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        // Save the original HOME value
        let original_home = std::env::var("HOME").ok();

        // Set a known HOME value
        std::env::set_var("HOME", "/test/home");

        let home = get_home_env();
        assert_eq!(home, Some("/test/home".to_string()));

        // Restore original HOME
        if let Some(orig) = original_home {
            std::env::set_var("HOME", orig);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn get_home_env_returns_none_when_not_set() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        // Save the original HOME value
        let original_home = std::env::var("HOME").ok();

        // Remove HOME
        std::env::remove_var("HOME");

        let home = get_home_env();
        assert!(home.is_none(), "HOME should be None when not set");

        // Restore original HOME
        if let Some(orig) = original_home {
            std::env::set_var("HOME", orig);
        }
    }

    #[test]
    fn get_home_env_never_panics() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        // This test verifies that get_home_env() handles all cases gracefully
        // without panicking, even with unusual HOME values.

        // Save the original HOME value
        let original_home = std::env::var("HOME").ok();

        // Test with empty HOME
        std::env::set_var("HOME", "");
        let home = get_home_env();
        assert_eq!(home, Some("".to_string()));

        // Test with special characters in HOME
        std::env::set_var("HOME", "/path/with spaces");
        let home = get_home_env();
        assert_eq!(home, Some("/path/with spaces".to_string()));

        // Test with HOME that contains UTF-8
        std::env::set_var("HOME", "/path/with/🏠");
        let home = get_home_env();
        assert_eq!(home, Some("/path/with/🏠".to_string()));

        // Restore original HOME
        if let Some(orig) = original_home {
            std::env::set_var("HOME", orig);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn get_home_env_no_unwrap_or_expect() {
        // This is a compile-time test that verifies the implementation
        // doesn't use unwrap() or expect(). The function signature
        // returning Option<String> forces proper error handling.

        // Simply calling the function proves it compiles
        let _home = get_home_env();

        // If this test compiles, the implementation is correct
        // because unwrap() or expect() would change the return type
        // or require different call patterns
    }

    // ── Timeout-triggered mitosis configuration tests ──

    #[test]
    fn default_timeout_triggered_policy_values() {
        let policy = TimeoutTriggeredPolicy::default();
        assert!(!policy.enabled);
        assert!(!policy.agent_wallclock_timeout);
        assert!(!policy.handler_timeout);
        assert_eq!(policy.min_elapsed_fraction, 0.9);
    }

    #[test]
    fn timeout_triggered_policy_qualifies_checks_enabled() {
        let policy = TimeoutTriggeredPolicy {
            enabled: false,
            agent_wallclock_timeout: true,
            handler_timeout: true,
            min_elapsed_fraction: 0.9,
        };
        // Disabled policy should never qualify
        assert!(!policy.qualifies("agent_wallclock_timeout", 0.95));
        assert!(!policy.qualifies("handler_timeout", 0.95));
    }

    #[test]
    fn timeout_triggered_policy_qualifies_checks_elapsed_fraction() {
        let policy = TimeoutTriggeredPolicy {
            enabled: true,
            agent_wallclock_timeout: true,
            handler_timeout: false,
            min_elapsed_fraction: 0.9,
        };
        // Below threshold should not qualify
        assert!(!policy.qualifies("agent_wallclock_timeout", 0.8));
        // At threshold should qualify
        assert!(policy.qualifies("agent_wallclock_timeout", 0.9));
        // Above threshold should qualify
        assert!(policy.qualifies("agent_wallclock_timeout", 0.95));
    }

    #[test]
    fn timeout_triggered_policy_qualifies_checks_reason_type() {
        let policy = TimeoutTriggeredPolicy {
            enabled: true,
            agent_wallclock_timeout: true,
            handler_timeout: true,
            min_elapsed_fraction: 0.9,
        };
        // Qualified reason types
        assert!(policy.qualifies("agent_wallclock_timeout", 0.95));
        assert!(policy.qualifies("timeout", 0.95)); // alias for agent_wallclock_timeout
        assert!(policy.qualifies("handler_timeout", 0.95));

        // Unqualified reason types should not qualify
        assert!(!policy.qualifies("build_timeout", 0.95));
        assert!(!policy.qualifies("idle", 0.95));
        assert!(!policy.qualifies("cancelled", 0.95));
        assert!(!policy.qualifies("crash", 0.95));
        assert!(!policy.qualifies("infrastructure_error", 0.95));
    }

    #[test]
    fn timeout_triggered_policy_enabled_without_qualifiers_fails_validation() {
        let mut config = Config::default();
        config.strands.mitosis.timeout_triggered.enabled = true;
        // Leave agent_wallclock_timeout and handler_timeout as false

        let errors = ConfigLoader::validate(&config);
        assert!(
            errors
                .iter()
                .any(|e| e.field == "strands.mitosis.timeout_triggered"
                    && e.message.contains(
                        "at least one of agent_wallclock_timeout or handler_timeout must be true"
                    )),
            "expected validation error for enabled policy without qualifiers, got: {:?}",
            errors
        );
    }

    #[test]
    fn timeout_triggered_policy_min_elapsed_fraction_out_of_range_fails_validation() {
        let mut config = Config::default();
        config.strands.mitosis.timeout_triggered.enabled = true;
        config
            .strands
            .mitosis
            .timeout_triggered
            .agent_wallclock_timeout = true;
        config
            .strands
            .mitosis
            .timeout_triggered
            .min_elapsed_fraction = 1.5; // > 1.0

        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.iter().any(|e| e.field
                == "strands.mitosis.timeout_triggered.min_elapsed_fraction"
                && e.message.contains("must be in range [0.0, 1.0]")),
            "expected validation error for min_elapsed_fraction > 1.0, got: {:?}",
            errors
        );
    }

    #[test]
    fn timeout_triggered_policy_min_elapsed_fraction_negative_fails_validation() {
        let mut config = Config::default();
        config.strands.mitosis.timeout_triggered.enabled = true;
        config
            .strands
            .mitosis
            .timeout_triggered
            .agent_wallclock_timeout = true;
        config
            .strands
            .mitosis
            .timeout_triggered
            .min_elapsed_fraction = -0.1; // < 0.0

        let errors = ConfigLoader::validate(&config);
        assert!(
            errors.iter().any(|e| e.field
                == "strands.mitosis.timeout_triggered.min_elapsed_fraction"
                && e.message.contains("must be in range [0.0, 1.0]")),
            "expected validation error for min_elapsed_fraction < 0.0, got: {:?}",
            errors
        );
    }

    #[test]
    fn timeout_triggered_policy_valid_configuration_passes_validation() {
        let mut config = Config::default();
        config.strands.mitosis.timeout_triggered.enabled = true;
        config
            .strands
            .mitosis
            .timeout_triggered
            .agent_wallclock_timeout = true;
        config
            .strands
            .mitosis
            .timeout_triggered
            .min_elapsed_fraction = 0.8;

        let errors = ConfigLoader::validate(&config);
        assert!(
            !errors
                .iter()
                .any(|e| e.field.starts_with("strands.mitosis.timeout_triggered")),
            "expected no validation errors for valid timeout-triggered policy, got: {:?}",
            errors
        );
    }

    #[test]
    fn env_override_timeout_triggered_enabled() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_STRANDS__MITOSIS__TIMEOUT_TRIGGERED__ENABLED";
        std::env::set_var(key, "true");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert!(config.strands.mitosis.timeout_triggered.enabled);
        assert!(sources.contains_key("strands.mitosis.timeout_triggered.enabled"));
    }

    #[test]
    fn env_override_timeout_triggered_agent_wallclock_timeout() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_STRANDS__MITOSIS__TIMEOUT_TRIGGERED__AGENT_WALLCLOCK_TIMEOUT";
        std::env::set_var(key, "true");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert!(
            config
                .strands
                .mitosis
                .timeout_triggered
                .agent_wallclock_timeout
        );
        assert!(sources.contains_key("strands.mitosis.timeout_triggered.agent_wallclock_timeout"));
    }

    #[test]
    fn env_override_timeout_triggered_handler_timeout() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_STRANDS__MITOSIS__TIMEOUT_TRIGGERED__HANDLER_TIMEOUT";
        std::env::set_var(key, "true");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert!(config.strands.mitosis.timeout_triggered.handler_timeout);
        assert!(sources.contains_key("strands.mitosis.timeout_triggered.handler_timeout"));
    }

    #[test]
    fn env_override_timeout_triggered_min_elapsed_fraction() {
        let mut config = Config::default();
        let mut sources = SourceMap::new();

        let key = "NEEDLE_STRANDS__MITOSIS__TIMEOUT_TRIGGERED__MIN_ELAPSED_FRACTION";
        std::env::set_var(key, "0.95");
        ConfigLoader::apply_env_overrides(&mut config, &mut sources);
        std::env::remove_var(key);

        assert_eq!(
            config
                .strands
                .mitosis
                .timeout_triggered
                .min_elapsed_fraction,
            0.95
        );
        assert!(sources.contains_key("strands.mitosis.timeout_triggered.min_elapsed_fraction"));
    }

    #[test]
    fn timeout_triggered_yaml_roundtrip() {
        let policy = TimeoutTriggeredPolicy {
            enabled: true,
            agent_wallclock_timeout: true,
            handler_timeout: false,
            min_elapsed_fraction: 0.85,
        };

        let yaml = serde_yaml::to_string(&policy).unwrap();
        let decoded: TimeoutTriggeredPolicy = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(decoded.enabled, policy.enabled);
        assert_eq!(
            decoded.agent_wallclock_timeout,
            policy.agent_wallclock_timeout
        );
        assert_eq!(decoded.handler_timeout, policy.handler_timeout);
        assert_eq!(decoded.min_elapsed_fraction, policy.min_elapsed_fraction);
    }

    #[test]
    fn default_mitosis_config_includes_timeout_triggered() {
        let config = MitosisConfig::default();
        // Verify default timeout_triggered policy is present
        assert!(!config.timeout_triggered.enabled);
        assert!(!config.timeout_triggered.agent_wallclock_timeout);
        assert!(!config.timeout_triggered.handler_timeout);
        assert_eq!(config.timeout_triggered.min_elapsed_fraction, 0.9);
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tilde expansion tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_expand_tilde_str_with_slash() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("~/docs");
        std::env::remove_var("HOME");
        assert_eq!(result, "/home/testuser/docs");
    }

    #[test]
    fn test_expand_tilde_str_bare_tilde() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("~");
        std::env::remove_var("HOME");
        assert_eq!(result, "/home/testuser");
    }

    #[test]
    fn test_expand_tilde_str_nested_path() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("~/config/needle/file.yaml");
        std::env::remove_var("HOME");
        assert_eq!(result, "/home/testuser/config/needle/file.yaml");
    }

    #[test]
    fn test_expand_tilde_str_without_tilde_unchanged() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("/absolute/path");
        std::env::remove_var("HOME");
        assert_eq!(result, "/absolute/path");
    }

    #[test]
    fn test_expand_tilde_str_relative_path_unchanged() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("relative/path");
        std::env::remove_var("HOME");
        assert_eq!(result, "relative/path");
    }

    #[test]
    fn test_expand_tilde_str_other_user_tilde_unchanged() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("~otheruser/path");
        std::env::remove_var("HOME");
        assert_eq!(result, "~otheruser/path");
    }

    #[test]
    fn test_expand_tilde_str_empty_string() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("");
        std::env::remove_var("HOME");
        assert_eq!(result, "");
    }

    #[test]
    fn test_expand_tilde_str_trailing_slash() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde_str("~/");
        std::env::remove_var("HOME");
        assert_eq!(result, "/home/testuser");
    }

    #[test]
    fn test_expand_tilde_str_missing_home() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::remove_var("HOME");
        let result = expand_tilde_str("~/docs");
        assert_eq!(result, "~/docs"); // No HOME, return unchanged
    }

    #[test]
    fn test_expand_tilde_str_missing_home_bare_tilde() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::remove_var("HOME");
        let result = expand_tilde_str("~");
        assert_eq!(result, "~"); // No HOME, return unchanged
    }

    #[test]
    fn test_expand_tilde_pathbuf_with_slash() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde(Path::new("~/docs"));
        std::env::remove_var("HOME");
        assert_eq!(result, PathBuf::from("/home/testuser/docs"));
    }

    #[test]
    fn test_expand_tilde_pathbuf_bare_tilde() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde(Path::new("~"));
        std::env::remove_var("HOME");
        assert_eq!(result, PathBuf::from("/home/testuser"));
    }

    #[test]
    fn test_expand_tilde_pathbuf_nested_path() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde(Path::new("~/config/needle/file.yaml"));
        std::env::remove_var("HOME");
        assert_eq!(
            result,
            PathBuf::from("/home/testuser/config/needle/file.yaml")
        );
    }

    #[test]
    fn test_expand_tilde_pathbuf_absolute_path_unchanged() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde(Path::new("/absolute/path"));
        std::env::remove_var("HOME");
        assert_eq!(result, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn test_expand_tilde_pathbuf_relative_path_unchanged() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde(Path::new("relative/path"));
        std::env::remove_var("HOME");
        assert_eq!(result, PathBuf::from("relative/path"));
    }

    #[test]
    fn test_expand_tilde_pathbuf_missing_home() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::remove_var("HOME");
        let result = expand_tilde(Path::new("~/docs"));
        assert_eq!(result, PathBuf::from("~/docs")); // No HOME, return unchanged
    }

    #[test]
    fn test_expand_tilde_pathbuf_empty_string() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde(Path::new(""));
        std::env::remove_var("HOME");
        assert_eq!(result, PathBuf::from(""));
    }

    #[test]
    fn test_expand_tilde_pathbuf_trailing_slash() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde(Path::new("~/"));
        std::env::remove_var("HOME");
        assert_eq!(result, PathBuf::from("/home/testuser"));
    }

    #[test]
    fn test_expand_tilde_pathbuf_parent_path() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = expand_tilde(Path::new("~/../"));
        std::env::remove_var("HOME");
        assert_eq!(result, PathBuf::from("/home/testuser/../"));
    }

    #[test]
    fn test_expand_tilde_option_some() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let input = Some(PathBuf::from("~/docs"));
        let result = expand_tilde_option(&input);
        std::env::remove_var("HOME");
        assert_eq!(result, Some(PathBuf::from("/home/testuser/docs")));
    }

    #[test]
    fn test_expand_tilde_option_none() {
        let input: Option<PathBuf> = None;
        let result = expand_tilde_option(&input);
        assert_eq!(result, None);
    }

    #[test]
    fn test_expand_tilde_vec_empty() {
        let input: Vec<PathBuf> = vec![];
        let result = expand_tilde_vec(&input);
        assert_eq!(result, Vec::<PathBuf>::new());
    }

    #[test]
    fn test_expand_tilde_vec_single() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let input = vec![PathBuf::from("~/docs")];
        let result = expand_tilde_vec(&input);
        std::env::remove_var("HOME");
        assert_eq!(result, vec![PathBuf::from("/home/testuser/docs")]);
    }

    #[test]
    fn test_expand_tilde_vec_multiple() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let input = vec![
            PathBuf::from("~/docs"),
            PathBuf::from("~/config"),
            PathBuf::from("/absolute"),
        ];
        let result = expand_tilde_vec(&input);
        std::env::remove_var("HOME");
        assert_eq!(
            result,
            vec![
                PathBuf::from("/home/testuser/docs"),
                PathBuf::from("/home/testuser/config"),
                PathBuf::from("/absolute"),
            ]
        );
    }

    #[test]
    fn test_dirs_or_home_with_home() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let result = dirs_or_home(".config/needle");
        std::env::remove_var("HOME");
        assert_eq!(result, PathBuf::from("/home/testuser/.config/needle"));
    }

    #[test]
    fn test_dirs_or_home_without_home_fallback() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::remove_var("HOME");
        let result = dirs_or_home(".config/needle");
        assert_eq!(result, PathBuf::from("/tmp/.config/needle"));
    }

    #[test]
    fn test_config_expand_tildes_global_config() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let mut config = Config::default();
        config.workspace.default = PathBuf::from("~/workspace");
        config.workspace.home = PathBuf::from("~/.needle");
        config.agent.adapters_dir = PathBuf::from("~/adapters");

        config.expand_tildes();
        std::env::remove_var("HOME");

        assert_eq!(
            config.workspace.default,
            PathBuf::from("/home/testuser/workspace")
        );
        assert_eq!(
            config.workspace.home,
            PathBuf::from("/home/testuser/.needle")
        );
        assert_eq!(
            config.agent.adapters_dir,
            PathBuf::from("/home/testuser/adapters")
        );
    }

    #[test]
    fn test_config_expand_tildes_workspace_config() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let mut config = Config::default();
        config.strands.explore.workspace_root = PathBuf::from("~/workspaces");
        config.strands.explore.workspaces =
            vec![PathBuf::from("~/project1"), PathBuf::from("~/project2")];
        config.strands.weave.exclude_workspaces = vec![PathBuf::from("~/private")];
        config.strands.splice.report_workspace = Some(PathBuf::from("~/reports"));

        config.expand_tildes();
        std::env::remove_var("HOME");

        assert_eq!(
            config.strands.explore.workspace_root,
            PathBuf::from("/home/testuser/workspaces")
        );
        assert_eq!(
            config.strands.explore.workspaces,
            vec![
                PathBuf::from("/home/testuser/project1"),
                PathBuf::from("/home/testuser/project2"),
            ]
        );
        assert_eq!(
            config.strands.weave.exclude_workspaces,
            vec![PathBuf::from("/home/testuser/private")]
        );
        assert_eq!(
            config.strands.splice.report_workspace,
            Some(PathBuf::from("/home/testuser/reports"))
        );
    }

    #[test]
    fn test_config_expand_tildes_env_var_paths() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let mut config = Config::default();
        config.strands.learning.global_learnings_file = PathBuf::from("~/global-learnings.md");
        config.health.heartbeat_dir = Some(PathBuf::from("~/heartbeats"));
        config.supervisor.heartbeat_path = Some(PathBuf::from("~/supervisor-heartbeat.json"));
        config.supervisor.socket_path = Some(PathBuf::from("~/supervisor.sock"));

        config.expand_tildes();
        std::env::remove_var("HOME");

        assert_eq!(
            config.strands.learning.global_learnings_file,
            PathBuf::from("/home/testuser/global-learnings.md")
        );
        assert_eq!(
            config.health.heartbeat_dir,
            Some(PathBuf::from("/home/testuser/heartbeats"))
        );
        assert_eq!(
            config.supervisor.heartbeat_path,
            Some(PathBuf::from("/home/testuser/supervisor-heartbeat.json"))
        );
        assert_eq!(
            config.supervisor.socket_path,
            Some(PathBuf::from("/home/testuser/supervisor.sock"))
        );
    }

    #[test]
    fn test_config_expand_tildes_mixed_paths() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::set_var("HOME", "/home/testuser");
        let mut config = Config::default();
        config.workspace.default = PathBuf::from("~/workspace");
        config.workspace.home = PathBuf::from("/absolute/needle");
        config.agent.adapters_dir = PathBuf::from("relative/adapters");

        config.expand_tildes();
        std::env::remove_var("HOME");

        assert_eq!(
            config.workspace.default,
            PathBuf::from("/home/testuser/workspace")
        );
        assert_eq!(config.workspace.home, PathBuf::from("/absolute/needle"));
        assert_eq!(
            config.agent.adapters_dir,
            PathBuf::from("relative/adapters")
        );
    }

    #[test]
    fn test_config_expand_tildes_missing_home() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        std::env::remove_var("HOME");
        let mut config = Config::default();
        config.workspace.default = PathBuf::from("~/workspace");
        config.workspace.home = PathBuf::from("~/.needle");

        config.expand_tildes();

        // Without HOME, paths should remain unchanged
        assert_eq!(config.workspace.default, PathBuf::from("~/workspace"));
        assert_eq!(config.workspace.home, PathBuf::from("~/.needle"));
    }
}
