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
    /// Capability document negotiated for the target store.
    pub backend_capabilities: Option<serde_json::Value>,
    /// Adapter identity.
    pub adapter: Option<String>,
    /// Harness identity, when the adapter declares one.
    pub harness: Option<String>,
    /// Model identity.
    pub model: Option<String>,
    /// Digest of the context manifest exposed to the adapter.
    pub context_manifest_hash: Option<String>,
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
    }
}
