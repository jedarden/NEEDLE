//! LOOM (Weft) worker configuration.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{ConfigTier, ReloadTier};

/// Where Weft is placed in NEEDLE's strand waterfall.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoomPosition {
    BeforePluck,
    #[default]
    AfterPluck,
}

/// Adapter and optional model used to serve one LOOM strand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoomServeStrand {
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Settings for a NEEDLE worker that polls LOOM for Weft turns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoomConfig {
    /// Disable the Weft worker unless explicitly enabled.
    pub enabled: bool,
    /// LOOM API root. Restart is required after changing this value.
    pub base_url: String,
    /// Private file containing the bearer token. Config parsing never reads it.
    pub token_file: PathBuf,
    /// Empty means use this NEEDLE process's worker name.
    pub worker_name: String,
    pub position: LoomPosition,
    pub poll_interval_secs: u64,
    pub lease_secs: u64,
    pub heartbeat_secs: u64,
    pub workspace_root: PathBuf,
    pub scratch_dir: PathBuf,
    /// Strand name to adapter/model mapping. `adapter: none` is the
    /// adapter-free courier path and does not require a model.
    pub serve_strands: BTreeMap<String, LoomServeStrand>,
}

impl Default for LoomConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://loom.ardenone.com".to_string(),
            token_file: PathBuf::from("~/.config/needle/loom.token"),
            worker_name: String::new(),
            position: LoomPosition::AfterPluck,
            poll_interval_secs: 2,
            lease_secs: 120,
            heartbeat_secs: 30,
            workspace_root: PathBuf::from("/home/coding"),
            scratch_dir: PathBuf::from("~/.cache/needle/weft"),
            serve_strands: BTreeMap::new(),
        }
    }
}

impl LoomConfig {
    pub fn expand_tildes(&mut self) {
        self.token_file = super::expand_tilde(&self.token_file);
        self.workspace_root = super::expand_tilde(&self.workspace_root);
        self.scratch_dir = super::expand_tilde(&self.scratch_dir);
    }

    /// Read a token only when a client is about to authenticate. The file
    /// must have mode 0600. Errors never include file contents.
    pub fn read_token(&self) -> std::io::Result<String> {
        let metadata = fs::metadata(&self.token_file)?;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "LOOM token file must have mode 0600",
            ));
        }
        let token = fs::read_to_string(&self.token_file)?;
        let token = token.trim().to_string();
        if token.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "LOOM token file is empty",
            ));
        }
        Ok(token)
    }
}

impl ConfigTier for LoomConfig {
    fn reload_tier(&self) -> ReloadTier {
        ReloadTier::Live
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ConfigLoader};

    #[test]
    fn defaults_match_contract_and_remain_disabled() {
        let loom = LoomConfig::default();
        assert!(!loom.enabled);
        assert_eq!(loom.base_url, "https://loom.ardenone.com");
        assert_eq!(
            loom.token_file,
            PathBuf::from("~/.config/needle/loom.token")
        );
        assert_eq!(loom.worker_name, "");
        assert_eq!(loom.position, LoomPosition::AfterPluck);
        assert_eq!(loom.poll_interval_secs, 2);
        assert_eq!(loom.lease_secs, 120);
        assert_eq!(loom.heartbeat_secs, 30);
        assert_eq!(loom.workspace_root, PathBuf::from("/home/coding"));
        assert!(loom.serve_strands.is_empty());
    }

    #[test]
    fn validation_requires_strands_when_enabled_and_rejects_unknown_adapters() {
        let mut config = Config::default();
        config.loom.enabled = true;
        assert!(ConfigLoader::validate(&config)
            .iter()
            .any(|error| error.full_path == "loom.serve_strands"));
        config.loom.serve_strands.insert(
            "advisor".to_string(),
            LoomServeStrand {
                adapter: "not-a-real-adapter".to_string(),
                model: Some("some-model".to_string()),
            },
        );
        assert!(ConfigLoader::validate(&config).iter().any(|error| {
            error.full_path == "loom.serve_strands.advisor.adapter"
                && error.message.contains("unknown adapter")
        }));
    }

    #[test]
    fn courier_none_adapter_does_not_require_model() {
        let mut config = Config::default();
        config.loom.enabled = true;
        config.loom.serve_strands.insert(
            "courier".to_string(),
            LoomServeStrand {
                adapter: "none".to_string(),
                model: None,
            },
        );
        assert!(!ConfigLoader::validate(&config)
            .iter()
            .any(|error| { error.full_path.starts_with("loom.serve_strands.courier") }));
    }

    #[test]
    fn workspace_block_overrides_global_fields() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".needle.yaml"),
            "loom:\n  enabled: true\n  position: before_pluck\n  serve_strands:\n    courier:\n      adapter: none\n",
        )
        .unwrap();
        let overrides = ConfigLoader::load_workspace(dir.path()).unwrap().unwrap();
        let mut config = Config::default();
        let mut sources = Default::default();
        ConfigLoader::apply_workspace(&mut config, &overrides, dir.path(), &mut sources);
        assert!(config.loom.enabled);
        assert_eq!(config.loom.position, LoomPosition::BeforePluck);
        assert!(config.loom.serve_strands.contains_key("courier"));
        assert_eq!(config.loom.base_url, LoomConfig::default().base_url);
    }

    #[test]
    fn token_file_requires_private_mode_and_is_read_at_use() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loom.token");
        fs::write(&path, "test-token\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let loom = LoomConfig {
            token_file: path,
            ..LoomConfig::default()
        };
        assert_eq!(loom.read_token().unwrap(), "test-token");
        fs::set_permissions(&loom.token_file, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            loom.read_token().unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn base_url_and_token_file_are_restart_required() {
        assert_eq!(
            crate::config::tiers::get_tier_for_key("loom.serve_strands"),
            Some(ReloadTier::Live)
        );
        assert_eq!(
            crate::config::tiers::get_tier_for_key("loom.base_url"),
            Some(ReloadTier::RestartRequired)
        );
        assert_eq!(
            crate::config::tiers::get_tier_for_key("loom.token_file"),
            Some(ReloadTier::RestartRequired)
        );
        let current = Config::default();
        let mut candidate = current.clone();
        candidate.loom.base_url.push_str("/changed");
        candidate.loom.token_file.push("changed");
        assert_eq!(
            current.changed_restart_required_keys(&candidate),
            vec!["loom.base_url", "loom.token_file"]
        );
    }
}
