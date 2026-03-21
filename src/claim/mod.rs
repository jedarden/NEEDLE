//! Atomic bead claiming with serialization.
//!
//! Claimer attempts to atomically set a bead to in_progress. If another
//! worker races us, we get a race-lost signal and retry with backoff.
//!
//! Depends on: `types`, `config`, `bead_store`, `telemetry`.

use anyhow::Result;

use crate::bead_store::BeadStore;
use crate::config::Config;
use crate::telemetry::Telemetry;
use crate::types::{BeadId, ClaimResult};

/// Handles the claim protocol for a single bead.
pub struct Claimer {
    config: Config,
    telemetry: Telemetry,
}

impl Claimer {
    pub fn new(config: Config, telemetry: Telemetry) -> Self {
        Claimer { config, telemetry }
    }

    /// Attempt to claim a bead, with retries up to `config.max_claim_retries`.
    ///
    /// On success, fetches and returns the full bead via `store.get()`.
    pub async fn claim(&self, store: &dyn BeadStore, bead_id: &BeadId) -> Result<ClaimResult> {
        for attempt in 1..=self.config.max_claim_retries {
            tracing::debug!(bead_id = %bead_id, attempt, "attempting claim");
            self.telemetry
                .emit(crate::telemetry::EventKind::ClaimAttempt {
                    bead_id: bead_id.clone(),
                    attempt,
                })?;

            match store.claim(bead_id, &self.config.worker_name).await {
                Ok(true) => {
                    self.telemetry
                        .emit(crate::telemetry::EventKind::ClaimSuccess {
                            bead_id: bead_id.clone(),
                        })?;
                    let bead = store.get(bead_id).await?;
                    return Ok(ClaimResult::Claimed(bead));
                }
                Ok(false) => {
                    self.telemetry
                        .emit(crate::telemetry::EventKind::ClaimRaceLost {
                            bead_id: bead_id.clone(),
                        })?;
                    if attempt < self.config.max_claim_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            100 * u64::from(attempt),
                        ))
                        .await;
                    } else {
                        return Ok(ClaimResult::RaceLost {
                            claimed_by: "(unknown)".to_string(),
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(bead_id = %bead_id, error = %e, "claim error");
                    return Ok(ClaimResult::NotClaimable {
                        reason: e.to_string(),
                    });
                }
            }
        }
        Ok(ClaimResult::RaceLost {
            claimed_by: "(unknown)".to_string(),
        })
    }
}
