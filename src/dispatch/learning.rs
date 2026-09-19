//! Transition adapters for dispatcher observations.

use needle_learning::{ContentHash, ProcessObservation};
use sha2::{Digest, Sha256};

use super::ExecutionResult;

/// Convert a process result to an observation without carrying raw output into
/// the kernel. Hashing is performed by this effect-owning adapter; the kernel
/// receives only the resulting immutable digests.
pub fn process_observation(result: &ExecutionResult) -> ProcessObservation {
    ProcessObservation {
        exit_code: Some(result.exit_code),
        duration_ms: result.elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
        stdout_digest: Some(content_hash(&result.stdout)),
        stderr_digest: Some(content_hash(&result.stderr)),
        interrupted: result.timeout_reason.is_some(),
    }
}

fn content_hash(value: &str) -> ContentHash {
    let digest = Sha256::digest(value.as_bytes());
    let encoded = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    ContentHash::new(encoded).expect("SHA-256 hex digest is a valid content hash")
}
