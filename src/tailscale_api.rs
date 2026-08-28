//! Tailscale API client for ephemeral key generation.
//!
//! This module provides HTTP client functionality to call SEAM's Tailscale API
//! endpoint and retrieve ephemeral auth keys for worker processes.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Configuration for the Tailscale API client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// SEAM endpoint URL for Tailscale key provisioning.
    /// Example: "https://seam-rs-manager.tail1b1987.ts.net"
    #[serde(default = "ApiConfig::default_seam_endpoint")]
    pub seam_endpoint: String,

    /// API key for SEAM authentication (if required).
    /// Can be loaded from environment variable: NEEDLE_SEAM_API_KEY
    #[serde(default)]
    pub api_key: Option<String>,

    /// Request timeout in seconds (default: 30).
    #[serde(default = "ApiConfig::default_timeout")]
    pub timeout_secs: u64,

    /// Whether to enable debug logging (default: false).
    #[serde(default)]
    pub debug: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        ApiConfig {
            seam_endpoint: Self::default_seam_endpoint(),
            api_key: None,
            timeout_secs: Self::default_timeout(),
            debug: false,
        }
    }
}

impl ApiConfig {
    fn default_seam_endpoint() -> String {
        // Default to SEAM's production endpoint
        "https://seam-rs-manager.tail1b1987.ts.net".to_string()
    }

    fn default_timeout() -> u64 {
        30 // seconds
    }

    /// Load API key from environment variable if not already set.
    pub fn load_api_key_from_env(mut self) -> Self {
        if self.api_key.is_none() {
            if let Ok(key) = std::env::var("NEEDLE_SEAM_API_KEY") {
                self.api_key = Some(key);
            }
        }
        self
    }
}

/// Request to create an ephemeral Tailscale key.
#[derive(Debug, Serialize)]
pub struct CreateKeyRequest {
    /// Worker ID for this key.
    pub worker_id: String,
    /// Bead ID for this key.
    pub bead_id: String,
    /// Desired hostname for the worker node.
    pub hostname: String,
    /// Tags to apply to the node.
    pub tags: Vec<String>,
    /// Time-to-live for the key in seconds.
    pub ttl_secs: u64,
}

/// Response from SEAM's Tailscale key creation endpoint.
#[derive(Debug, Deserialize)]
pub struct CreateKeyResponse {
    /// The ephemeral auth key.
    pub key: String,
    /// When the key expires (ISO 8601 timestamp).
    pub expires_at: String,
    /// The actual hostname assigned.
    pub hostname: String,
}

/// Error response from SEAM API.
#[derive(Debug, Deserialize)]
pub struct ErrorResponse {
    /// Error message.
    pub error: String,
    /// HTTP status code.
    pub status: u16,
}

/// Tailscale API client for calling SEAM's endpoint.
#[derive(Debug)]
pub struct TailscaleClient {
    config: ApiConfig,
    client: ureq::Agent,
    mock_mode: bool,
    mock_key: Option<String>,
}

impl TailscaleClient {
    /// Create a new Tailscale API client.
    pub fn new(config: ApiConfig) -> Result<Self> {
        let timeout = Duration::from_secs(config.timeout_secs);

        // Configure HTTP client with appropriate settings
        let mut agent_builder = ureq::AgentBuilder::new();
        agent_builder = agent_builder.timeout(timeout);

        // Add user agent for identification
        agent_builder = agent_builder.user_agent(&format!(
            "NEEDLE/{} (Tailscale Integration)",
            env!("CARGO_PKG_VERSION")
        ));

        let client = agent_builder.build();

        Ok(Self {
            config,
            client,
            mock_mode: false,
            mock_key: None,
        })
    }

    /// Create a new Tailscale API client in mock mode for testing.
    pub fn new_mock(mock_key: String) -> Self {
        Self {
            config: ApiConfig::default(),
            client: ureq::AgentBuilder::new().build(),
            mock_mode: true,
            mock_key: Some(mock_key),
        }
    }

    /// Create an ephemeral Tailscale key via SEAM.
    pub fn create_ephemeral_key(
        &self,
        worker_id: &str,
        bead_id: &str,
        hostname: &str,
        tags: &[String],
        ttl_secs: u64,
    ) -> Result<String> {
        // If in mock mode, return the mock key
        if self.mock_mode {
            if let Some(ref key) = self.mock_key {
                tracing::debug!(
                    worker_id = %worker_id,
                    bead_id = %bead_id,
                    hostname = %hostname,
                    "returning mock Tailscale key"
                );
                return Ok(key.clone());
            }
            anyhow::bail!("mock mode enabled but no mock key configured");
        }

        let url = format!(
            "{}/api/v1/tailscale/ephemeral-key",
            self.config.seam_endpoint.trim_end_matches('/')
        );

        let request = CreateKeyRequest {
            worker_id: worker_id.to_string(),
            bead_id: bead_id.to_string(),
            hostname: hostname.to_string(),
            tags: tags.to_vec(),
            ttl_secs,
        };

        if self.config.debug {
            tracing::debug!(
                url = %url,
                worker_id = %worker_id,
                bead_id = %bead_id,
                hostname = %hostname,
                tags = ?tags,
                "requesting ephemeral Tailscale key from SEAM"
            );
        }

        // Build the request
        let mut req = self.client.post(&url);
        req = req.timeout(Duration::from_secs(self.config.timeout_secs));

        // Add API key header if configured
        if let Some(ref api_key) = self.config.api_key {
            req = req.set("Authorization", &format!("Bearer {}", api_key));
        }

        // Serialize request body
        let request_body = serde_json::to_string(&request)
            .context("failed to serialize Tailscale key request")?;

        // Send the request
        let response = req
            .send_string(&request_body)
            .context("failed to send request to SEAM for Tailscale key")?;

        // Handle response
        match response.status() {
            200..=299 => {
                let response_text = response
                    .into_string()
                    .context("failed to read SEAM response body")?;

                let resp: CreateKeyResponse = serde_json::from_str(&response_text)
                    .context("failed to parse SEAM response")?;

                if self.config.debug {
                    tracing::debug!(
                        hostname = %resp.hostname,
                        expires_at = %resp.expires_at,
                        "received ephemeral Tailscale key from SEAM"
                    );
                }

                Ok(resp.key)
            }
            status => {
                let error_text = response
                    .into_string()
                    .unwrap_or_else(|_| "unable to read error response".to_string());

                anyhow::bail!(
                    "SEAM returned error status {} for Tailscale key request: {}",
                    status,
                    error_text
                );
            }
        }
    }

    /// Check if the SEAM endpoint is accessible.
    pub fn health_check(&self) -> Result<bool> {
        let url = format!(
            "{}/health",
            self.config.seam_endpoint.trim_end_matches('/')
        );

        match self.client.get(&url).call() {
            Ok(response) if response.status() == 200 => Ok(true),
            Ok(_) => Ok(false),
            Err(e) => {
                // Connection errors mean the service is down
                if e.to_string().contains("connect") || e.to_string().contains("dns") {
                    Ok(false)
                } else {
                    Err(e.into())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_config_defaults() {
        let config = ApiConfig::default();
        assert_eq!(
            config.seam_endpoint,
            "https://seam-rs-manager.tail1b1987.ts.net"
        );
        assert_eq!(config.timeout_secs, 30);
        assert!(config.api_key.is_none());
        assert!(!config.debug);
    }

    #[test]
    fn test_api_config_load_from_env() {
        std::env::set_var("NEEDLE_SEAM_API_KEY", "test-key-123");
        let config = ApiConfig::default().load_api_key_from_env();
        assert_eq!(config.api_key, Some("test-key-123".to_string()));
        std::env::remove_var("NEEDLE_SEAM_API_KEY");
    }

    #[test]
    fn test_create_key_request_serialization() {
        let request = CreateKeyRequest {
            worker_id: "worker-1".to_string(),
            bead_id: "bf-test".to_string(),
            hostname: "needle-worker-1-bf-test".to_string(),
            tags: vec!["tag:needle-worker".to_string()],
            ttl_secs: 3600,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("worker-1"));
        assert!(json.contains("bf-test"));
        assert!(json.contains("tag:needle-worker"));
        assert!(json.contains("3600"));
    }

    #[test]
    fn test_create_key_response_deserialization() {
        let json = r#"{"key":"tskey-auth-test","expires_at":"2026-08-28T12:00:00Z","hostname":"needle-worker-1-bf-test"}"#;

        let response: CreateKeyResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.key, "tskey-auth-test");
        assert_eq!(response.expires_at, "2026-08-28T12:00:00Z");
        assert_eq!(response.hostname, "needle-worker-1-bf-test");
    }
}
