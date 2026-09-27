//! Identity and provenance shared by one claimed attempt.
//!
//! An attempt ID is minted before the claim mutation.  The small provenance
//! value below is then copied into every durable artifact that can outlive the
//! worker process (the ledger row, trace metadata, and heartbeat activity).

use sha2::{Digest, Sha256};

/// The identity facts that must remain attached to one attempt from claim to
/// resolution.  Optional fields represent backend capability gaps; they are
/// never silently replaced with facts from a later retry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttemptProvenance {
    /// Revision returned by the backend after the claim landed.
    pub claim_revision: Option<u64>,
    /// Assignee used for the claim and later resolution.
    pub assignee: Option<String>,
    /// Backend fencing/lease epoch returned with the claim.
    pub claim_epoch: Option<u64>,
    /// Credential-free renewable claim metadata captured from the original
    /// claim and refreshed only after an accepted compare-and-swap renewal.
    pub claim_handle: Option<crate::claim::ClaimHandleMetadata>,
    /// Capability document negotiated for the target store.
    pub backend_capabilities: Option<serde_json::Value>,
    /// Adapter identity.
    pub adapter: Option<String>,
    /// Harness identity, when the adapter declares one.
    pub harness: Option<String>,
    /// Model identifier requested through the selected adapter. The legacy
    /// `model` fields in persisted telemetry remain aliases of this value.
    pub requested_model: Option<String>,
    /// Model identifier returned in provider response metadata, when observed.
    pub effective_model: Option<String>,
    /// Metadata field that supplied `effective_model` (for example
    /// `claude_message.model` or `claude_system_init.model`).
    pub model_resolution_source: Option<String>,
    /// Digest of the context manifest exposed to the adapter.
    pub context_manifest_hash: Option<String>,
}

/// The provider-reported model identity from a supported response field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveModel {
    pub identifier: String,
    pub source: &'static str,
}

/// Read the first non-empty model identifier from Claude's raw JSONL stream.
///
/// Only the identifier and its source field are returned. Transcript text,
/// tool arguments, and reasoning content are never copied into provenance.
pub fn effective_model_from_claude_stream(stream: &str) -> Option<EffectiveModel> {
    for line in stream.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) == Some("assistant") {
            if let Some(identifier) = value
                .get("message")
                .and_then(|message| message.get("model"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|model| !model.is_empty())
            {
                return Some(EffectiveModel {
                    identifier: identifier.to_string(),
                    source: "claude_message.model",
                });
            }
        }
        if value.get("type").and_then(serde_json::Value::as_str) == Some("system")
            && value.get("subtype").and_then(serde_json::Value::as_str) == Some("init")
        {
            if let Some(identifier) = value
                .get("model")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|model| !model.is_empty())
            {
                return Some(EffectiveModel {
                    identifier: identifier.to_string(),
                    source: "claude_system_init.model",
                });
            }
        }
    }
    None
}

/// Mint the one opaque identity for a claim/dispatch attempt.
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Hash the exact context identity bound to an attempt.
///
/// The input is deliberately a small, canonical JSON object.  It records the
/// context boundary even when a backend has no richer manifest API, and keeps
/// retries distinct because the attempt ID is part of the digest.
#[allow(clippy::too_many_arguments)]
pub fn context_manifest_hash(
    attempt_id: &str,
    bead_id: &str,
    workspace: &str,
    worker: &str,
    adapter: &str,
    model: Option<&str>,
    template: &str,
    template_version: &str,
) -> String {
    let manifest = serde_json::json!({
        "schema_version": 1,
        "attempt_id": attempt_id,
        "bead_id": bead_id,
        "workspace": workspace,
        "worker": worker,
        "adapter": adapter,
        "model": model,
        "prompt_template": template,
        "template_version": template_version,
    });
    let bytes = serde_json::to_vec(&manifest).unwrap_or_default();
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_ids_are_unique_and_context_hash_is_attempt_scoped() {
        let first = new_id();
        let retry = new_id();
        assert_ne!(first, retry);
        assert_eq!(uuid::Uuid::parse_str(&first).unwrap().get_version_num(), 7);

        let hash = |id: &str| {
            context_manifest_hash(
                id,
                "needle-test",
                "/workspace",
                "worker",
                "adapter",
                Some("model"),
                "pluck",
                "pluck-default",
            )
        };
        assert_eq!(hash(&first), hash(&first));
        assert_ne!(hash(&first), hash(&retry));
        let stream = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"  \",\"session_id\":\"s\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"model\":\"glm-5.3-flash\",\"content\":[]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"model\":\"later-model\"}}\n"
        );
        assert_eq!(
            effective_model_from_claude_stream(stream),
            Some(EffectiveModel {
                identifier: "glm-5.3-flash".to_string(),
                source: "claude_message.model",
            })
        );
        assert_eq!(
            effective_model_from_claude_stream(
                "{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"glm-5.3-flash\"}"
            ),
            Some(EffectiveModel {
                identifier: "glm-5.3-flash".to_string(),
                source: "claude_system_init.model",
            })
        );
        assert_eq!(
            effective_model_from_claude_stream(
                "{\"type\":\"assistant\",\"message\":{\"content\":[],\"model\":\"\"}}"
            ),
            None
        );
    }
}
