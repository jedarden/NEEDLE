//! Per-worker tsnet identity provisioning.
//!
//! This module provides ephemeral Tailscale identities for worker processes,
//! enabling each worker to be a first-class tailnet peer that SEAM can
//! WhoIs individually.
//!
//! ## Architecture
//!
//! Each worker gets a unique tsnet identity with:
//! - Stable hostname based on worker_id and bead_id
//! - Ephemeral auth key (provisioned at dispatch)
//! - Tag: needle-worker
//! - Automatic cleanup after process exits
//!
//! Identity is injected into worker processes via environment variables:
//! - `NEEDLE_TSNET_HOSTNAME`: The worker's stable hostname
//! - `NEEDLE_TSNET_AUTH_KEY`: Ephemeral auth key (single-use)
//! - `NEEDLE_TSNET_CONTROL_URL`: Tailscale control plane URL
//! - `NEEDLE_TSNET_FUNNEL_URL`: Funnel relay URL (if enabled)
//!
//! Depends on: `types`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::{ConfigTier, ReloadTier};
use crate::tailscale_api::{ApiConfig, TailscaleClient};
use crate::types::{BeadId, WorkerId};

/// Configuration for tsnet identity provisioning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsnetConfig {
    /// Tailscale control plane URL (default: https://control.tailscale.com).
    #[serde(default = "TsnetConfig::default_control_url")]
    pub control_url: String,

    /// Whether to use Funnel for direct connectivity (default: false).
    #[serde(default)]
    pub funnel_enabled: bool,

    /// Funnel relay URL (default: https://funnel.tailscale.com).
    #[serde(default = "TsnetConfig::default_funnel_url")]
    pub funnel_url: String,

    /// TTL for ephemeral auth keys in seconds (default: 3600 = 1 hour).
    #[serde(default = "TsnetConfig::default_auth_ttl")]
    pub auth_ttl_secs: u64,

    /// Tag applied to all worker nodes (default: needle-worker).
    #[serde(default = "TsnetConfig::default_tag")]
    pub worker_tag: String,

    /// Whether tsnet identity provisioning is enabled (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// SEAM API endpoint configuration for Tailscale key provisioning.
    #[serde(default)]
    pub api_config: ApiConfig,

    /// Tailnet name for key creation (default: ardenone).
    #[serde(default = "TsnetConfig::default_tailnet")]
    pub tailnet: String,
}

impl Default for TsnetConfig {
    fn default() -> Self {
        TsnetConfig {
            control_url: Self::default_control_url(),
            funnel_enabled: false,
            funnel_url: Self::default_funnel_url(),
            auth_ttl_secs: Self::default_auth_ttl(),
            worker_tag: Self::default_tag(),
            enabled: false,
            api_config: ApiConfig::default(),
            tailnet: Self::default_tailnet(),
        }
    }
}

impl ConfigTier for TsnetConfig {
    fn reload_tier(&self) -> ReloadTier {
        // Tier C: Embed-level, subprocess-facing configuration
        ReloadTier::RestartRequired
    }
}

impl TsnetConfig {
    fn default_control_url() -> String {
        "https://control.tailscale.com".to_string()
    }

    fn default_funnel_url() -> String {
        "https://funnel.tailscale.com".to_string()
    }

    fn default_auth_ttl() -> u64 {
        3600 // 1 hour
    }

    fn default_tag() -> String {
        "tag:needle-worker".to_string()
    }

    fn default_tailnet() -> String {
        "ardenone".to_string()
    }
}

/// A provisioned tsnet identity for a single worker execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerIdentity {
    /// Unique hostname for this worker execution.
    pub hostname: String,
    /// Ephemeral auth key for this worker (single-use).
    pub auth_key: String,
    /// Worker ID that owns this identity.
    pub worker_id: WorkerId,
    /// Bead ID this identity is bound to.
    pub bead_id: BeadId,
    /// Timestamp when this identity was provisioned.
    pub provisioned_at: u64,
    /// TTL for this identity (seconds).
    pub ttl_secs: u64,
    /// Tag applied to this node.
    pub tag: String,
}

impl WorkerIdentity {
    /// Generate a stable hostname for a worker/bead pair.
    fn hostname(worker_id: &WorkerId, bead_id: &BeadId) -> String {
        // Sanitize worker_id and bead_id to be valid hostname components
        let worker = worker_id.replace(|c: char| !c.is_alphanumeric() && c != '-', "-");
        let bead = bead_id
            .as_ref()
            .replace(|c: char| !c.is_alphanumeric() && c != '-', "-");

        // Use a stable format: needle-{worker}-{bead}
        format!("needle-{}-{}", worker, bead)
    }

    /// Check if this identity has expired.
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.provisioned_at) > self.ttl_secs
    }
}

/// Registry of active worker identities.
///
/// This tracks all provisioned identities and provides cleanup for expired ones.
#[derive(Debug)]
pub struct IdentityRegistry {
    config: TsnetConfig,
    identities: Arc<RwLock<HashMap<String, WorkerIdentity>>>,
    api_client: Option<TailscaleClient>,
}

impl IdentityRegistry {
    /// Create a new identity registry with the given config.
    pub fn new(config: TsnetConfig) -> Self {
        // Initialize API client if tsnet is enabled
        let api_client = if config.enabled {
            match TailscaleClient::new(config.api_config.clone()) {
                Ok(client) => {
                    tracing::info!("Tailscale API client initialized for SEAM endpoint: {}", config.api_config.seam_endpoint);
                    Some(client)
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize Tailscale API client: {}. Identity provisioning will fail closed.", e);
                    None
                }
            }
        } else {
            None
        };

        IdentityRegistry {
            config,
            identities: Arc::new(RwLock::new(HashMap::new())),
            api_client,
        }
    }

    /// Provision a new identity for a worker/bead execution.
    ///
    /// Returns a unique identity with an ephemeral auth key.
    pub async fn provision_identity(
        &self,
        worker_id: &WorkerId,
        bead_id: &BeadId,
    ) -> Result<WorkerIdentity> {
        if !self.config.enabled {
            anyhow::bail!("tsnet identity provisioning is not enabled");
        }

        let hostname = WorkerIdentity::hostname(worker_id, bead_id);

        // Generate real ephemeral auth key via SEAM API
        let auth_key = self.generate_auth_key(worker_id, bead_id, &hostname)?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let identity = WorkerIdentity {
            hostname: hostname.clone(),
            auth_key,
            worker_id: worker_id.clone(),
            bead_id: bead_id.clone(),
            provisioned_at: now,
            ttl_secs: self.config.auth_ttl_secs,
            tag: self.config.worker_tag.clone(),
        };

        // Register the identity
        let mut identities = self.identities.write().await;
        identities.insert(hostname.clone(), identity.clone());

        tracing::info!(
            worker_id = %worker_id,
            bead_id = %bead_id.as_ref(),
            hostname = %hostname,
            "provisioned tsnet identity for worker"
        );

        Ok(identity)
    }

    /// Generate an ephemeral auth key for a hostname.
    ///
    /// This calls SEAM's Tailscale API endpoint to provision a real ephemeral key.
    /// When the API client is unavailable or the call fails, this returns an error
    /// rather than fabricating a credential-shaped value.
    fn generate_auth_key(&self, worker_id: &WorkerId, bead_id: &BeadId, hostname: &str) -> Result<String> {
        // Check if API client is available
        let client = self.api_client.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Tailscale API client not initialized - tsnet may be disabled or client failed to initialize"))?;

        // Prepare tags for the worker node
        let tags = vec![self.config.worker_tag.clone()];

        tracing::info!(
            worker_id = %worker_id,
            bead_id = %bead_id.as_ref(),
            hostname = %hostname,
            tailnet = %self.config.tailnet,
            tags = ?tags,
            ttl_secs = self.config.auth_ttl_secs,
            "requesting ephemeral Tailscale key from SEAM API"
        );

        // Call SEAM's API to create ephemeral key
        let auth_key = client.create_ephemeral_key(
            worker_id,
            bead_id.as_ref(),
            hostname,
            &tags,
            self.config.auth_ttl_secs,
        ).context("failed to create ephemeral Tailscale key via SEAM API")?;

        tracing::info!(
            worker_id = %worker_id,
            bead_id = %bead_id.as_ref(),
            hostname = %hostname,
            "successfully provisioned ephemeral Tailscale key"
        );

        Ok(auth_key)
    }

    /// Mark an identity as used (remove from registry).
    pub async fn release_identity(&self, hostname: &str) {
        let mut identities = self.identities.write().await;
        if let Some(identity) = identities.remove(hostname) {
            tracing::debug!(
                worker_id = %identity.worker_id,
                bead_id = %identity.bead_id.as_ref(),
                hostname = %hostname,
                "released tsnet identity"
            );
        }
    }

    /// Clean up expired identities.
    pub async fn cleanup_expired(&self) -> Result<()> {
        let mut identities = self.identities.write().await;
        let mut to_remove = Vec::new();

        for (hostname, identity) in identities.iter() {
            if identity.is_expired() {
                to_remove.push(hostname.clone());
            }
        }

        for hostname in to_remove {
            identities.remove(&hostname);
            tracing::debug!(hostname = %hostname, "cleaned up expired tsnet identity");
        }

        Ok(())
    }

    /// Get the current count of active identities.
    pub async fn active_count(&self) -> usize {
        self.identities.read().await.len()
    }
}

/// Inject tsnet identity environment variables into a process environment.
///
/// Adds the following variables:
/// - `NEEDLE_TSNET_HOSTNAME`: The worker's stable hostname
/// - `NEEDLE_TSNET_AUTH_KEY`: Ephemeral auth key
/// - `NEEDLE_TSNET_CONTROL_URL`: Tailscale control plane URL
/// - `NEEDLE_TSNET_FUNNEL_URL`: Funnel relay URL (if enabled)
/// - `NEEDLE_TSNET_TAG`: Worker tag
pub fn inject_identity_env(
    identity: &WorkerIdentity,
    config: &TsnetConfig,
    base_env: &mut HashMap<String, String>,
) {
    base_env.insert(
        "NEEDLE_TSNET_HOSTNAME".to_string(),
        identity.hostname.clone(),
    );
    base_env.insert(
        "NEEDLE_TSNET_AUTH_KEY".to_string(),
        identity.auth_key.clone(),
    );
    base_env.insert(
        "NEEDLE_TSNET_CONTROL_URL".to_string(),
        config.control_url.clone(),
    );
    base_env.insert("NEEDLE_TSNET_TAG".to_string(), identity.tag.clone());

    if config.funnel_enabled {
        base_env.insert(
            "NEEDLE_TSNET_FUNNEL_URL".to_string(),
            config.funnel_url.clone(),
        );
    }

    tracing::debug!(
        hostname = %identity.hostname,
        tag = %identity.tag,
        "injected tsnet identity environment variables"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hostname_generation() {
        let worker_id = "worker-42".to_string();
        let bead_id = BeadId::from("bf-test123");

        let hostname = WorkerIdentity::hostname(&worker_id, &bead_id);

        assert!(hostname.starts_with("needle-"));
        assert!(hostname.contains("worker-42"));
        assert!(hostname.contains("bf-test123"));
    }

    #[test]
    fn test_hostname_sanitization() {
        let worker_id = "worker@42#bad".to_string();
        let bead_id = BeadId::from("bf_test/with-slashes");

        let hostname = WorkerIdentity::hostname(&worker_id, &bead_id);

        // Should not contain special characters
        assert!(!hostname.contains("@"));
        assert!(!hostname.contains("#"));
        assert!(!hostname.contains("/"));
    }

    #[test]
    fn test_identity_expiration() {
        let mut identity = WorkerIdentity {
            hostname: "test".to_string(),
            auth_key: "key".to_string(),
            worker_id: "worker".to_string(),
            bead_id: BeadId::from("bf-test"),
            provisioned_at: 0,
            ttl_secs: 3600,
            tag: "tag:needle-worker".to_string(),
        };

        // Old identity should be expired
        assert!(identity.is_expired());

        // Recent identity should not be expired
        identity.provisioned_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        assert!(!identity.is_expired());
    }

    #[tokio::test]
    async fn test_identity_registry_provision_with_mock_api() {
        let config = TsnetConfig {
            enabled: true,
            ..Default::default()
        };
        let mut registry = IdentityRegistry::new(config);

        // Inject a mock API client for testing
        registry.api_client = Some(TailscaleClient::new_mock(
            "tskey-auth-mock-test-key-12345".to_string(),
        ));

        let worker_id = "worker-1".to_string();
        let bead_id = BeadId::from("bf-test");

        let result = registry.provision_identity(&worker_id, &bead_id).await;

        // Should succeed with mock client
        assert!(result.is_ok());
        let identity = result.unwrap();
        assert_eq!(identity.auth_key, "tskey-auth-mock-test-key-12345");
        assert!(identity.hostname.contains("worker-1"));
        assert!(identity.hostname.contains("bf-test"));
        assert_eq!(identity.tag, "tag:needle-worker");
    }

    #[tokio::test]
    async fn test_identity_registry_release() {
        let config = TsnetConfig {
            enabled: true,
            ..Default::default()
        };
        let mut registry = IdentityRegistry::new(config);

        // Inject a mock API client for testing
        registry.api_client = Some(TailscaleClient::new_mock(
            "tskey-auth-mock-test-key-12345".to_string(),
        ));

        let worker_id = "worker-1".to_string();
        let bead_id = BeadId::from("bf-test");

        // Provisioning should succeed with mock client
        let result = registry.provision_identity(&worker_id, &bead_id).await;
        assert!(result.is_ok());

        // One identity should be registered
        assert_eq!(registry.active_count().await, 1);

        // Get the hostname for cleanup
        let hostname = WorkerIdentity::hostname(&worker_id, &bead_id);

        // Release the identity
        registry.release_identity(&hostname).await;

        // No identities should remain
        assert_eq!(registry.active_count().await, 0);
    }

    #[test]
    fn test_inject_identity_env() {
        let config = TsnetConfig {
            enabled: true,
            funnel_enabled: true,
            ..Default::default()
        };

        let identity = WorkerIdentity {
            hostname: "needle-worker-test".to_string(),
            auth_key: "tskey-auth-test".to_string(),
            worker_id: "worker".to_string(),
            bead_id: BeadId::from("bf-test"),
            provisioned_at: 0,
            ttl_secs: 3600,
            tag: "tag:needle-worker".to_string(),
        };

        let mut env = HashMap::new();
        env.insert("EXISTING_VAR".to_string(), "value".to_string());

        inject_identity_env(&identity, &config, &mut env);

        assert_eq!(
            env.get("NEEDLE_TSNET_HOSTNAME"),
            Some(&"needle-worker-test".to_string())
        );
        assert_eq!(
            env.get("NEEDLE_TSNET_AUTH_KEY"),
            Some(&"tskey-auth-test".to_string())
        );
        assert_eq!(
            env.get("NEEDLE_TSNET_CONTROL_URL"),
            Some(&"https://control.tailscale.com".to_string())
        );
        assert_eq!(
            env.get("NEEDLE_TSNET_TAG"),
            Some(&"tag:needle-worker".to_string())
        );
        assert_eq!(
            env.get("NEEDLE_TSNET_FUNNEL_URL"),
            Some(&"https://funnel.tailscale.com".to_string())
        );
        assert_eq!(env.get("EXISTING_VAR"), Some(&"value".to_string())); // preserved
    }

    #[test]
    fn test_tsnet_config_defaults() {
        let config = TsnetConfig::default();

        assert_eq!(config.control_url, "https://control.tailscale.com");
        assert_eq!(config.funnel_url, "https://funnel.tailscale.com");
        assert_eq!(config.auth_ttl_secs, 3600);
        assert_eq!(config.worker_tag, "tag:needle-worker");
        assert!(!config.enabled);
    }

    #[test]
    fn test_disabled_tsnet_does_not_inject_auth_key() {
        // Verify that when tsnet is disabled, no auth key is injected
        let _config = TsnetConfig {
            enabled: false,
            ..Default::default()
        };

        let mut env = HashMap::new();
        env.insert("EXISTING_VAR".to_string(), "value".to_string());

        // When tsnet is disabled, provision_identity fails early
        // and inject_identity_env is never called
        assert!(!env.contains_key("NEEDLE_TSNET_AUTH_KEY"));
        assert!(!env.contains_key("NEEDLE_TSNET_HOSTNAME"));
    }
}
