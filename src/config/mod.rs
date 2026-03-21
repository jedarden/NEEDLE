//! Configuration loading and resolution.
//!
//! Hierarchical config: global → workspace → CLI overrides.
//! Leaf module — depends only on `types`.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Fully-resolved NEEDLE configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Path to the workspace root (contains `.beads/`).
    pub workspace: std::path::PathBuf,

    /// Name of the worker (used as assignee in claims).
    pub worker_name: String,

    /// Path to the agent CLI binary (e.g., `claude`).
    pub agent_binary: String,

    /// Arguments to pass to the agent binary before the prompt.
    pub agent_args: Vec<String>,

    /// Maximum number of claim retries before moving to next candidate.
    pub max_claim_retries: u32,

    /// Heartbeat interval in seconds.
    pub heartbeat_interval_secs: u64,

    /// Heartbeat TTL in seconds (staleness threshold).
    pub heartbeat_ttl_secs: u64,

    /// Maximum number of concurrent workers (fleet sizing).
    pub max_fleet_size: u32,
}

impl Config {
    /// Load configuration with layered resolution.
    ///
    /// Resolution order (later layers override earlier):
    /// 1. Built-in defaults
    /// 2. Global config file (`~/.config/needle/config.toml`)
    /// 3. Workspace config file (`.needle/config.toml`)
    /// 4. Environment variables (`NEEDLE_*`)
    /// 5. CLI arguments (passed via overrides)
    pub fn load(_overrides: ConfigOverrides) -> Result<Self> {
        // TODO(needle-0ez): implement full hierarchical loading
        todo!("Config::load not yet implemented")
    }

    /// Return defaults suitable for development/testing.
    pub fn default_for_test() -> Self {
        Config {
            workspace: std::path::PathBuf::from("."),
            worker_name: "needle-test".to_string(),
            agent_binary: "claude".to_string(),
            agent_args: vec![],
            max_claim_retries: 3,
            heartbeat_interval_secs: 10,
            heartbeat_ttl_secs: 60,
            max_fleet_size: 20,
        }
    }
}

/// CLI-level overrides that take highest precedence.
#[derive(Debug, Default)]
pub struct ConfigOverrides {
    pub workspace: Option<std::path::PathBuf>,
    pub worker_name: Option<String>,
    pub agent_binary: Option<String>,
}
