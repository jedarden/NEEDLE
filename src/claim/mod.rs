//! Atomic bead claiming with per-workspace flock serialization.
//!
//! The Claimer wraps `BeadStore.claim()` with coordination that prevents
//! thundering herd. A per-workspace flock serializes claim operations so
//! workers take turns rather than racing on the same bead.
//!
//! Depends on: `types`, `bead_store`, `telemetry`.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use fs2::FileExt;

use crate::bead_store::BeadStore;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{Bead, BeadId, BeadStatus, ClaimOutcome, ClaimResult};

/// Flock timeout: maximum time to wait for the workspace lock.
const FLOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Flock poll interval: time between lock acquisition attempts.
const FLOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Number of consecutive claim errors before marking a bead suspect.
const CLAIM_ERROR_THRESHOLD: u32 = 3;

/// Maximum claim-related history entries allowed before a bead is quarantined.
///
/// A single claim can append more than one history entry (for example,
/// `claimed` plus `assignee_changed`), so this is intentionally a hard safety
/// budget rather than a claim-attempt count.  The backend is checked before
/// each mutation whenever it exposes event history.
const MAX_CLAIM_EVENTS_PER_BEAD: u32 = 100;

/// Atomic bead claimer with workspace-level flock serialization.
pub struct Claimer {
    store: Arc<dyn BeadStore>,
    lock_dir: PathBuf,
    max_retries: u32,
    retry_backoff_ms: u64,
    telemetry: Telemetry,
    /// Track consecutive claim errors per bead ID.
    claim_errors: Arc<std::sync::Mutex<HashMap<BeadId, u32>>>,
    /// Track total claim events emitted per bead ID for circuit-breaking.
    claim_events: Arc<std::sync::Mutex<HashMap<BeadId, u32>>>,
}

impl Claimer {
    /// Create a new Claimer.
    ///
    /// - `store`: bead store for verify + claim operations
    /// - `lock_dir`: directory for flock files (default: `/tmp`)
    /// - `max_retries`: maximum claim attempts before giving up (default: 5)
    /// - `retry_backoff_ms`: base backoff between retries in ms (default: 100)
    /// - `telemetry`: telemetry emitter
    pub fn new(
        store: Arc<dyn BeadStore>,
        lock_dir: PathBuf,
        max_retries: u32,
        retry_backoff_ms: u64,
        telemetry: Telemetry,
    ) -> Self {
        Claimer {
            store,
            lock_dir,
            max_retries,
            retry_backoff_ms,
            telemetry,
            claim_errors: Arc::new(std::sync::Mutex::new(HashMap::new())),
            claim_events: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Record a claim error for a bead and check if the threshold is reached.
    ///
    /// Returns `Some((consecutive_errors, last_error))` if the bead has reached
    /// the error threshold, `None` otherwise. This is distinct from race-lost:
    /// claim errors are CLI/store failures, not contention.
    fn record_claim_error(&self, bead_id: &BeadId, error: &str) -> Option<(u32, String)> {
        let mut errors = self.claim_errors.lock().unwrap();
        let count = errors.entry(bead_id.clone()).or_insert(0);
        *count += 1;
        let consecutive = *count;
        let last_error = error.to_string();

        if consecutive >= CLAIM_ERROR_THRESHOLD {
            // Reset the counter after reaching threshold so subsequent attempts
            // start fresh (avoids emitting the same error repeatedly)
            errors.remove(bead_id);
            Some((consecutive, last_error))
        } else {
            None
        }
    }

    /// Clear claim errors for a bead (e.g., after a successful claim).
    fn clear_claim_errors(&self, bead_id: &BeadId) {
        let mut errors = self.claim_errors.lock().unwrap();
        errors.remove(bead_id);
    }

    /// Check the in-memory fallback budget for backends that do not expose
    /// claim history in their `show` projection.
    ///
    /// Returns `Some(total_events)` after the budget is exhausted.
    fn check_event_limit(&self, bead_id: &BeadId) -> Option<u32> {
        let mut events = self.claim_events.lock().unwrap();
        let count = events.entry(bead_id.clone()).or_insert(0);
        *count += 1;
        let total = *count;

        if total > MAX_CLAIM_EVENTS_PER_BEAD {
            Some(total)
        } else {
            None
        }
    }

    async fn trip_event_limit(&self, bead_id: &BeadId, event_count: u32) -> Result<String> {
        let reason = format!(
            "claim history for bead {bead_id} reached {event_count} claim events (limit {MAX_CLAIM_EVENTS_PER_BEAD}); quarantining to prevent event-log runaway"
        );
        if let Err(error) = self.store.block(bead_id).await {
            let failure = format!("{reason}; quarantine failed: {error}");
            tracing::error!(%bead_id, %error, "failed to quarantine bead after claim history limit");
            self.telemetry.emit(EventKind::ClaimFailed {
                bead_id: bead_id.clone(),
                reason: failure.clone(),
            })?;
            return Err(anyhow!(failure));
        }
        self.telemetry.emit(EventKind::ClaimFailed {
            bead_id: bead_id.clone(),
            reason: reason.clone(),
        })?;
        Ok(reason)
    }

    /// Attempt to claim the next available bead from the candidate list.
    ///
    /// Iterates candidates in priority order, skipping those in the exclusion
    /// set. For each candidate, acquires a per-workspace flock, verifies the
    /// bead is still claimable, and attempts the claim.
    ///
    /// The `strand` parameter is the name of the strand that initiated the claim
    /// (e.g., "pluck", "mend"). This is emitted in telemetry for the
    /// `needle.beads.claimed` metric's `strand` attribute.
    ///
    /// Returns:
    /// - `Claimed(bead)`: successfully claimed a bead
    /// - `AllRaceLost`: tried candidates, all race-lost
    /// - `NoCandidates`: no candidates after filtering exclusions
    /// - `StoreError(e)`: bead store or flock error
    ///
    /// NOTE: The caller is responsible for creating the `bead.claim` span
    /// to ensure proper parent/child hierarchy (bead.claim should be a child
    /// of bead.lifecycle, not strand.{name}).
    pub async fn claim_next(
        &self,
        candidates: &[Bead],
        actor: &str,
        exclusions: &HashSet<BeadId>,
        strand: &str,
    ) -> Result<ClaimOutcome> {
        let eligible: Vec<&Bead> = candidates
            .iter()
            .filter(|b| !exclusions.contains(&b.id))
            .collect();

        if eligible.is_empty() {
            return Ok(ClaimOutcome::NoCandidates);
        }

        let mut attempts = 0u32;

        for candidate in &eligible {
            if attempts >= self.max_retries {
                tracing::Span::current().record("needle.claim.result", "max_retries_exceeded");
                // Set Error status on the bead.claim span
                tracing::Span::current().record("otel.status_code", 2u64);
                tracing::Span::current().record("otel.status_description", "max_retries_exceeded");
                return Ok(ClaimOutcome::AllRaceLost);
            }

            attempts += 1;
            let bead_id = &candidate.id;

            // Set bead_id and retry_number as span attributes.
            tracing::Span::current().record("needle.bead.id", bead_id.as_ref());
            tracing::Span::current().record("needle.claim.retry_number", attempts);

            self.telemetry.emit(EventKind::ClaimAttempt {
                bead_id: bead_id.clone(),
                attempt: attempts,
            })?;

            // Compute workspace lock path
            let lock_path = workspace_lock_path(&self.lock_dir, &candidate.workspace);

            // Acquire flock with timeout
            let lock_file = match acquire_flock(&lock_path).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(bead_id = %bead_id, error = %e, "flock timeout, skipping");
                    self.telemetry.emit(EventKind::ClaimFailed {
                        bead_id: bead_id.clone(),
                        reason: format!("flock timeout: {e}"),
                    })?;
                    // Set Error status on the bead.claim span
                    tracing::Span::current().record("otel.status_code", 2u64);
                    tracing::Span::current()
                        .record("otel.status_description", format!("flock timeout: {e}"));
                    return Ok(ClaimOutcome::StoreError(e));
                }
            };

            // Verify bead is still claimable (status=open, no assignee)
            let (current, claim_event_count) =
                match self.store.show_with_claim_history(bead_id).await {
                    Ok(b) => b,
                    Err(e) => {
                        drop(lock_file);
                        self.telemetry.emit(EventKind::ClaimFailed {
                            bead_id: bead_id.clone(),
                            reason: format!("verify failed: {e}"),
                        })?;
                        // Set Error status on the bead.claim span
                        tracing::Span::current().record("otel.status_code", 2u64);
                        tracing::Span::current()
                            .record("otel.status_description", format!("verify failed: {e}"));
                        return Ok(ClaimOutcome::StoreError(e));
                    }
                };

            if current.status != BeadStatus::Open || current.assignee.is_some() {
                drop(lock_file);
                // Distinguish stale-assignee from race-lost: race_lost means another worker
                // won the race (status changed), not that this bead has a leftover assignee.
                if current.status == BeadStatus::Open && current.assignee.is_some() {
                    self.telemetry.emit(EventKind::ClaimFailed {
                        bead_id: bead_id.clone(),
                        reason: "stale assignee".to_string(),
                    })?;
                } else {
                    self.telemetry.emit(EventKind::ClaimRaceLost {
                        bead_id: bead_id.clone(),
                    })?;
                }
                // Apply backoff before trying next candidate (same as store.claim RaceLost path)
                if attempts < self.max_retries {
                    tokio::time::sleep(Duration::from_millis(
                        self.retry_backoff_ms * u64::from(attempts),
                    ))
                    .await;
                }
                continue;
            }

            let in_memory_limit = if claim_event_count.is_none() {
                self.check_event_limit(bead_id)
            } else {
                None
            };
            if let Some(event_count) = claim_event_count.or(in_memory_limit) {
                if event_count >= MAX_CLAIM_EVENTS_PER_BEAD {
                    drop(lock_file);
                    let reason = self.trip_event_limit(bead_id, event_count).await?;
                    return Ok(ClaimOutcome::Suspect {
                        bead_id: bead_id.clone(),
                        consecutive_errors: event_count,
                        last_error: reason,
                    });
                }
            }

            // Attempt claim via store
            let result = self.store.claim(bead_id, actor).await;
            drop(lock_file);

            match result {
                Ok(ClaimResult::Claimed(bead)) => {
                    tracing::Span::current().record("needle.claim.result", "succeeded");
                    // Clear error counter on successful claim
                    self.clear_claim_errors(bead_id);
                    self.telemetry.set_workspace(bead.workspace.clone());
                    self.telemetry.emit(EventKind::ClaimSuccess {
                        bead_id: bead_id.clone(),
                        priority: candidate.priority as i32,
                        strand: strand.to_string(),
                    })?;
                    return Ok(ClaimOutcome::Claimed(bead));
                }
                Ok(ClaimResult::RaceLost { .. }) => {
                    tracing::Span::current().record("needle.claim.result", "race_lost");
                    self.telemetry.emit(EventKind::ClaimRaceLost {
                        bead_id: bead_id.clone(),
                    })?;
                    if attempts < self.max_retries {
                        tokio::time::sleep(Duration::from_millis(
                            self.retry_backoff_ms * u64::from(attempts),
                        ))
                        .await;
                    }
                    continue;
                }
                Ok(ClaimResult::NotClaimable { reason }) => {
                    tracing::Span::current().record("needle.claim.result", &reason);
                    self.telemetry.emit(EventKind::ClaimFailed {
                        bead_id: bead_id.clone(),
                        reason: reason.clone(),
                    })?;
                    continue;
                }
                Ok(ClaimResult::ClaimError { reason }) => {
                    tracing::Span::current().record("needle.claim.result", &reason);
                    self.telemetry.emit(EventKind::ClaimFailed {
                        bead_id: bead_id.clone(),
                        reason: reason.clone(),
                    })?;
                    // Record claim error and check if threshold reached
                    if let Some((consecutive, last_error)) =
                        self.record_claim_error(bead_id, &reason)
                    {
                        self.telemetry.emit(EventKind::ClaimErrorThreshold {
                            bead_id: bead_id.clone(),
                            consecutive_errors: consecutive,
                            last_error: last_error.clone(),
                        })?;
                        // Set Error status on the bead.claim span
                        tracing::Span::current().record("otel.status_code", 2u64);
                        tracing::Span::current().record("otel.status_description", &last_error);
                        return Ok(ClaimOutcome::Suspect {
                            bead_id: bead_id.clone(),
                            consecutive_errors: consecutive,
                            last_error,
                        });
                    }
                    // Continue to next candidate when threshold not yet reached
                    continue;
                }
                Ok(ClaimResult::Suspect {
                    bead_id: id,
                    consecutive_errors,
                    last_error,
                }) => {
                    // Bead is already marked suspect — propagate Suspect immediately
                    tracing::warn!(
                        bead_id = %id,
                        consecutive_errors,
                        %last_error,
                        "bead already marked suspect, propagating without retry"
                    );
                    // Set Error status on the bead.claim span
                    tracing::Span::current().record("otel.status_code", 2u64);
                    tracing::Span::current().record(
                        "otel.status_description",
                        format!(
                            "suspect: {} consecutive errors: {}",
                            consecutive_errors, last_error
                        ),
                    );
                    return Ok(ClaimOutcome::Suspect {
                        bead_id: id,
                        consecutive_errors,
                        last_error,
                    });
                }
                Err(e) => {
                    let reason = format!("store error: {e}");
                    tracing::Span::current().record("needle.claim.result", &reason);
                    self.telemetry.emit(EventKind::ClaimFailed {
                        bead_id: bead_id.clone(),
                        reason: reason.clone(),
                    })?;
                    // Record claim error and check if threshold reached
                    if let Some((consecutive, last_error)) =
                        self.record_claim_error(bead_id, &reason)
                    {
                        self.telemetry.emit(EventKind::ClaimErrorThreshold {
                            bead_id: bead_id.clone(),
                            consecutive_errors: consecutive,
                            last_error: last_error.clone(),
                        })?;
                        // Set Error status on the bead.claim span
                        tracing::Span::current().record("otel.status_code", 2u64);
                        tracing::Span::current().record("otel.status_description", &last_error);
                        return Ok(ClaimOutcome::Suspect {
                            bead_id: bead_id.clone(),
                            consecutive_errors: consecutive,
                            last_error,
                        });
                    }
                    // Set Error status on the bead.claim span
                    tracing::Span::current().record("otel.status_code", 2u64);
                    tracing::Span::current().record("otel.status_description", reason);
                    return Ok(ClaimOutcome::StoreError(e));
                }
            }
        }

        // Exhausted all eligible candidates without success
        tracing::Span::current().record("needle.claim.result", "all_race_lost");
        // Set Error status on the bead.claim span
        tracing::Span::current().record("otel.status_code", 2u64);
        tracing::Span::current().record("otel.status_description", "all_race_lost");
        Ok(ClaimOutcome::AllRaceLost)
    }

    /// Convenience: claim a single bead by ID (fetches the bead, then delegates
    /// to `claim_next` with a single-element candidate list).
    ///
    /// The `exclusions` parameter allows the caller to pass beads that should
    /// be skipped (e.g., beads that recently lost a claim race). This prevents
    /// tight loops when multiple workers are racing on the same bead.
    ///
    /// The `strand` parameter is optional; if not provided, defaults to "unknown".
    pub async fn claim_one(
        &self,
        bead_id: &BeadId,
        actor: &str,
        exclusions: &HashSet<BeadId>,
        strand: Option<&str>,
    ) -> Result<ClaimResult> {
        let bead = self.store.show(bead_id).await?;

        // If the bead is in the exclusion set, return NotClaimable immediately
        // without attempting the claim. This prevents tight loops when the worker
        // has already lost a race on this bead.
        if exclusions.contains(bead_id) {
            return Ok(ClaimResult::NotClaimable {
                reason: "bead is excluded".to_string(),
            });
        }

        match self
            .claim_next(&[bead], actor, exclusions, strand.unwrap_or("unknown"))
            .await?
        {
            ClaimOutcome::Claimed(b) => Ok(ClaimResult::Claimed(b)),
            ClaimOutcome::AllRaceLost => Ok(ClaimResult::RaceLost {
                claimed_by: "(race)".to_string(),
            }),
            ClaimOutcome::NoCandidates => Ok(ClaimResult::NotClaimable {
                reason: "no candidates".to_string(),
            }),
            ClaimOutcome::StoreError(e) => Err(e),
            ClaimOutcome::Suspect {
                bead_id,
                consecutive_errors,
                last_error,
            } => Ok(ClaimResult::Suspect {
                bead_id,
                consecutive_errors,
                last_error,
            }),
        }
    }

    /// Atomically claim the next available bead using server-side selection.
    ///
    /// This is the preferred method for multi-worker scenarios. It calls
    /// `BeadStore::claim_auto()` which atomically finds and claims a bead in
    /// a single transaction, guaranteeing that concurrent workers receive
    /// distinct beads.
    ///
    /// The `strand` parameter is the name of the strand that initiated the claim
    /// (e.g., "pluck", "mend"). This is emitted in telemetry for the
    /// `needle.beads.claimed` metric's `strand` attribute.
    ///
    /// Returns:
    /// - `Claimed(bead)`: successfully claimed a bead
    /// - `NotClaimable(reason)`: no beads available to claim
    /// - `StoreError(e)`: bead store error
    pub async fn claim_auto(&self, actor: &str, strand: &str) -> Result<ClaimResult> {
        self.telemetry.emit(EventKind::ClaimAttempt {
            bead_id: BeadId::from("(auto)".to_string()),
            attempt: 1,
        })?;

        match self.store.claim_auto(actor).await {
            Ok(ClaimResult::Claimed(bead)) => {
                let claim_event_count = match self.store.show_with_claim_history(&bead.id).await {
                    Ok((_, count)) => count,
                    Err(error) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            %error,
                            "unable to inspect claim history after atomic claim"
                        );
                        None
                    }
                };
                let in_memory_count = if claim_event_count.is_none() {
                    Some(self.check_event_limit(&bead.id).unwrap_or(0))
                } else {
                    None
                };
                let event_count = claim_event_count.or(in_memory_count).unwrap_or(0);
                if event_count >= MAX_CLAIM_EVENTS_PER_BEAD {
                    let reason = self.trip_event_limit(&bead.id, event_count).await?;
                    return Ok(ClaimResult::NotClaimable { reason });
                }
                tracing::Span::current().record("needle.bead.id", bead.id.as_ref());
                tracing::Span::current().record("needle.claim.result", "succeeded");
                self.telemetry.set_workspace(bead.workspace.clone());
                self.telemetry.emit(EventKind::ClaimSuccess {
                    bead_id: bead.id.clone(),
                    priority: bead.priority as i32,
                    strand: strand.to_string(),
                })?;
                Ok(ClaimResult::Claimed(bead))
            }
            Ok(ClaimResult::NotClaimable { reason }) => {
                tracing::Span::current().record("needle.claim.result", &reason);
                self.telemetry.emit(EventKind::ClaimFailed {
                    bead_id: BeadId::from("(auto)".to_string()),
                    reason: reason.clone(),
                })?;
                Ok(ClaimResult::NotClaimable { reason })
            }
            Ok(other) => {
                // RaceLost shouldn't happen with claim_auto, but handle it
                tracing::warn!(?other, "claim_auto returned unexpected result");
                Ok(other)
            }
            Err(e) => {
                let reason = format!("store error: {e}");
                tracing::Span::current().record("needle.claim.result", &reason);
                tracing::Span::current().record("otel.status_code", 2u64);
                tracing::Span::current().record("otel.status_description", &reason);
                self.telemetry.emit(EventKind::ClaimFailed {
                    bead_id: BeadId::from("(auto)".to_string()),
                    reason: reason.clone(),
                })?;
                Err(e)
            }
        }
    }
}

/// Compute a deterministic lock file path for a workspace.
///
/// Uses a simple hash (not crypto) of the workspace path. All workers
/// compute the same hash for the same workspace.
fn workspace_lock_path(lock_dir: &Path, workspace: &Path) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workspace.hash(&mut hasher);
    let hash = hasher.finish();
    lock_dir.join(format!("needle-claim-{:016x}.lock", hash))
}

/// Acquire an exclusive flock with a 10-second timeout.
///
/// Returns the locked file on success. The lock is released when the
/// file is dropped (flock auto-releases on close).
pub async fn acquire_flock(lock_path: &Path) -> Result<std::fs::File> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)?;

    let deadline = Instant::now() + FLOCK_TIMEOUT;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "flock timeout after {}s on {}",
                        FLOCK_TIMEOUT.as_secs(),
                        lock_path.display()
                    ));
                }
                tokio::time::sleep(FLOCK_POLL_INTERVAL).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{BeadStore, Filters, RepairReport};
    use crate::telemetry::test_utils::MemorySink;
    use crate::telemetry::Telemetry;
    use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn make_bead(id: &str, workspace: &str) -> Bead {
        Bead {
            id: BeadId::from(id),
            title: format!("Test bead {id}"),
            body: None,
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec![],
            workspace: PathBuf::from(workspace),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// Mock bead store that returns configurable claim results.
    struct MockBeadStore {
        beads: Mutex<Vec<Bead>>,
        /// Claim results consumed in FIFO order; when empty, claims succeed.
        claim_results: Mutex<Vec<ClaimResult>>,
        claim_auto_result: Mutex<Option<ClaimResult>>,
        claim_event_count: Mutex<Option<u32>>,
        blocked_beads: Mutex<Vec<BeadId>>,
    }

    impl MockBeadStore {
        fn new(beads: Vec<Bead>) -> Self {
            MockBeadStore {
                beads: Mutex::new(beads),
                claim_results: Mutex::new(vec![]),
                claim_auto_result: Mutex::new(None),
                claim_event_count: Mutex::new(None),
                blocked_beads: Mutex::new(Vec::new()),
            }
        }

        fn with_claim_results(self, results: Vec<ClaimResult>) -> Self {
            *self.claim_results.lock().unwrap() = results;
            self
        }

        fn with_claim_event_count(self, count: u32) -> Self {
            *self.claim_event_count.lock().unwrap() = Some(count);
            self
        }

        fn with_claim_auto_result(self, result: ClaimResult) -> Self {
            *self.claim_auto_result.lock().unwrap() = Some(result);
            self
        }
    }

    #[async_trait]
    impl BeadStore for MockBeadStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }

        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.beads
                .lock()
                .unwrap()
                .iter()
                .find(|b| b.id == *id)
                .cloned()
                .ok_or_else(|| anyhow!("bead not found: {id}"))
        }

        async fn show_with_claim_history(&self, id: &BeadId) -> Result<(Bead, Option<u32>)> {
            let bead = self.show(id).await?;
            Ok((bead, *self.claim_event_count.lock().unwrap()))
        }

        async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
            {
                let mut results = self.claim_results.lock().unwrap();
                if !results.is_empty() {
                    return Ok(results.remove(0));
                }
            }
            // Default: claim succeeds
            let mut bead = self
                .beads
                .lock()
                .unwrap()
                .iter()
                .find(|b| b.id == *id)
                .cloned()
                .ok_or_else(|| anyhow!("bead not found: {id}"))?;
            bead.status = BeadStatus::InProgress;
            bead.assignee = Some(actor.to_string());
            Ok(ClaimResult::Claimed(bead))
        }

        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            if let Some(result) = self.claim_auto_result.lock().unwrap().take() {
                return Ok(result);
            }
            // Return NotClaimable for tests unless overridden
            Ok(ClaimResult::NotClaimable {
                reason: "no beads available".to_string(),
            })
        }

        async fn release(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn block(&self, id: &BeadId) -> Result<()> {
            self.blocked_beads.lock().unwrap().push(id.clone());
            Ok(())
        }

        async fn flush(&self) -> Result<()> {
            Ok(())
        }

        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }

        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            Ok(vec![])
        }

        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }

        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }

        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new-bead".to_string()))
        }

        async fn doctor_repair(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        fn has_valid_store(&self) -> bool {
            true // Mock store always has a valid store
        }
    }

    fn make_claimer(store: Arc<dyn BeadStore>) -> Claimer {
        Claimer::new(
            store,
            std::env::temp_dir(),
            5,
            10, // short backoff for tests
            Telemetry::new("test-worker".to_string()),
        )
    }

    #[tokio::test]
    async fn successful_claims_update_workspace_for_the_same_telemetry_session() {
        let first = make_bead("needle-workspace-first", "/tmp/needle-workspace-first");
        let second = make_bead("needle-workspace-second", "/tmp/needle-workspace-second");
        let store = Arc::new(MockBeadStore::new(vec![first.clone(), second.clone()]));
        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-worker".to_string(), sink);
        let claimer = Claimer::new(store, std::env::temp_dir(), 5, 10, telemetry.clone());

        claimer
            .claim_next(
                std::slice::from_ref(&first),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();
        claimer
            .claim_next(
                std::slice::from_ref(&second),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();

        drop(claimer);
        drop(telemetry);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let successes: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.event_type == "bead.claim.succeeded")
            .cloned()
            .collect();
        assert_eq!(successes.len(), 2);
        assert_eq!(
            successes[0].workspace.as_deref(),
            Some(first.workspace.as_path())
        );
        assert_eq!(
            successes[1].workspace.as_deref(),
            Some(second.workspace.as_path())
        );
    }

    #[tokio::test]
    async fn claim_next_empty_candidates_returns_no_candidates() {
        let store = Arc::new(MockBeadStore::new(vec![]));
        let claimer = make_claimer(store);
        let result = claimer
            .claim_next(&[], "worker-1", &HashSet::new(), "test-strand")
            .await
            .unwrap();
        assert!(matches!(result, ClaimOutcome::NoCandidates));
    }

    #[tokio::test]
    async fn claim_history_limit_quarantines_before_claim_mutation() {
        let bead = make_bead("needle-bloated", "/tmp/ws");
        let store = Arc::new(
            MockBeadStore::new(vec![bead.clone()])
                .with_claim_event_count(MAX_CLAIM_EVENTS_PER_BEAD),
        );
        let claimer = make_claimer(store.clone());

        let result = claimer
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();

        match result {
            ClaimOutcome::Suspect {
                bead_id,
                last_error,
                ..
            } => {
                assert_eq!(bead_id, bead.id);
                assert!(last_error.contains("event-log runaway"));
            }
            other => panic!("expected event-limit suspect outcome, got {other:?}"),
        }
        assert_eq!(store.blocked_beads.lock().unwrap().as_slice(), &[bead.id]);
    }

    #[tokio::test]
    async fn claim_auto_history_limit_quarantines_after_atomic_claim() {
        let bead = make_bead("needle-bloated-auto", "/tmp/ws");
        let store = Arc::new(
            MockBeadStore::new(vec![bead.clone()])
                .with_claim_auto_result(ClaimResult::Claimed(bead.clone()))
                .with_claim_event_count(MAX_CLAIM_EVENTS_PER_BEAD),
        );
        let claimer = make_claimer(store.clone());

        let result = claimer.claim_auto("worker-1", "test-strand").await.unwrap();

        match result {
            ClaimResult::NotClaimable { reason } => {
                assert!(reason.contains("event-log runaway"));
            }
            other => panic!("expected event-limit not-claimable outcome, got {other:?}"),
        }
        assert_eq!(store.blocked_beads.lock().unwrap().as_slice(), &[bead.id]);
    }

    #[tokio::test]
    async fn claim_next_all_excluded_returns_no_candidates() {
        let bead = make_bead("needle-abc", "/tmp/ws");
        let store = Arc::new(MockBeadStore::new(vec![bead.clone()]));
        let claimer = make_claimer(store);
        let mut exclusions = HashSet::new();
        exclusions.insert(BeadId::from("needle-abc"));

        let result = claimer
            .claim_next(&[bead], "worker-1", &exclusions, "test-strand")
            .await
            .unwrap();
        assert!(matches!(result, ClaimOutcome::NoCandidates));
    }

    #[tokio::test]
    async fn claim_next_happy_path_returns_claimed() {
        let bead = make_bead("needle-abc", "/tmp/ws");
        let store = Arc::new(MockBeadStore::new(vec![bead.clone()]));
        let claimer = make_claimer(store);

        let result = claimer
            .claim_next(&[bead], "worker-1", &HashSet::new(), "test-strand")
            .await
            .unwrap();
        match result {
            ClaimOutcome::Claimed(b) => {
                assert_eq!(b.id, BeadId::from("needle-abc"));
                assert_eq!(b.assignee, Some("worker-1".to_string()));
            }
            other => panic!("expected Claimed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn claim_next_race_lost_tries_next_candidate() {
        let bead1 = make_bead("needle-aaa", "/tmp/ws");
        let bead2 = make_bead("needle-bbb", "/tmp/ws");
        let store = Arc::new(
            MockBeadStore::new(vec![bead1.clone(), bead2.clone()]).with_claim_results(vec![
                ClaimResult::RaceLost {
                    claimed_by: "other-worker".to_string(),
                },
                // Second claim (bead2) uses the default → success
            ]),
        );
        let claimer = make_claimer(store);

        let result = claimer
            .claim_next(&[bead1, bead2], "worker-1", &HashSet::new(), "test-strand")
            .await
            .unwrap();
        match result {
            ClaimOutcome::Claimed(b) => {
                assert_eq!(b.id, BeadId::from("needle-bbb"));
            }
            other => panic!("expected Claimed on second candidate, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn claim_next_all_race_lost_returns_all_race_lost() {
        let bead1 = make_bead("needle-aaa", "/tmp/ws");
        let bead2 = make_bead("needle-bbb", "/tmp/ws");
        let store = Arc::new(
            MockBeadStore::new(vec![bead1.clone(), bead2.clone()]).with_claim_results(vec![
                ClaimResult::RaceLost {
                    claimed_by: "x".to_string(),
                },
                ClaimResult::RaceLost {
                    claimed_by: "y".to_string(),
                },
            ]),
        );
        let claimer = make_claimer(store);

        let result = claimer
            .claim_next(&[bead1, bead2], "worker-1", &HashSet::new(), "test-strand")
            .await
            .unwrap();
        assert!(matches!(result, ClaimOutcome::AllRaceLost));
    }

    #[tokio::test]
    async fn claim_next_not_claimable_skips_to_next() {
        let bead1 = make_bead("needle-aaa", "/tmp/ws");
        let bead2 = make_bead("needle-bbb", "/tmp/ws");
        let store = Arc::new(
            MockBeadStore::new(vec![bead1.clone(), bead2.clone()]).with_claim_results(vec![
                ClaimResult::NotClaimable {
                    reason: "bead is blocked".to_string(),
                },
                // Second claim uses default → success
            ]),
        );
        let claimer = make_claimer(store);

        let result = claimer
            .claim_next(&[bead1, bead2], "worker-1", &HashSet::new(), "test-strand")
            .await
            .unwrap();
        match result {
            ClaimOutcome::Claimed(b) => {
                assert_eq!(b.id, BeadId::from("needle-bbb"));
            }
            other => panic!("expected Claimed on second candidate, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn claim_one_happy_path() {
        let bead = make_bead("needle-abc", "/tmp/ws");
        let store = Arc::new(MockBeadStore::new(vec![bead]));
        let claimer = make_claimer(store);

        let result = claimer
            .claim_one(
                &BeadId::from("needle-abc"),
                "worker-1",
                &HashSet::new(),
                Some("test-strand"),
            )
            .await
            .unwrap();
        assert!(matches!(result, ClaimResult::Claimed(_)));
    }

    #[test]
    fn workspace_lock_path_is_deterministic() {
        let dir = PathBuf::from("/tmp");
        let ws = Path::new("/home/coding/NEEDLE");
        let path1 = workspace_lock_path(&dir, ws);
        let path2 = workspace_lock_path(&dir, ws);
        assert_eq!(path1, path2);
        // Filename starts with needle-claim-
        let name = path1.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("needle-claim-"));
        assert!(name.ends_with(".lock"));
    }

    #[test]
    fn workspace_lock_path_differs_for_different_workspaces() {
        let dir = PathBuf::from("/tmp");
        let path1 = workspace_lock_path(&dir, Path::new("/workspace/a"));
        let path2 = workspace_lock_path(&dir, Path::new("/workspace/b"));
        assert_ne!(path1, path2);
    }

    #[tokio::test]
    async fn flock_acquire_and_release() {
        let dir = std::env::temp_dir().join("needle-test-flock");
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("test.lock");

        // Acquire lock
        let file = acquire_flock(&lock_path).await.unwrap();
        assert!(lock_path.exists());

        // Drop releases the lock
        drop(file);

        // Can re-acquire immediately
        let file2 = acquire_flock(&lock_path).await.unwrap();
        drop(file2);

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn exclusion_set_prevents_reclaim() {
        let bead = make_bead("needle-abc", "/tmp/ws");
        let store = Arc::new(MockBeadStore::new(vec![bead.clone()]));
        let claimer = make_claimer(store);
        let mut exclusions = HashSet::new();
        exclusions.insert(BeadId::from("needle-abc"));

        let result = claimer
            .claim_next(&[bead], "worker-1", &exclusions, "test-strand")
            .await
            .unwrap();
        assert!(matches!(result, ClaimOutcome::NoCandidates));
    }

    #[tokio::test]
    async fn max_retries_caps_attempts() {
        // Create more candidates than max_retries, all race-lost
        let beads: Vec<Bead> = (0..10)
            .map(|i| make_bead(&format!("needle-{i:03}"), "/tmp/ws"))
            .collect();
        let results: Vec<ClaimResult> = (0..10)
            .map(|_| ClaimResult::RaceLost {
                claimed_by: "x".to_string(),
            })
            .collect();
        let store = Arc::new(MockBeadStore::new(beads.clone()).with_claim_results(results));
        // max_retries = 3 — should stop after 3 attempts
        let claimer = Claimer::new(
            store,
            std::env::temp_dir(),
            3,
            10,
            Telemetry::new("test-worker".to_string()),
        );

        let result = claimer
            .claim_next(&beads, "worker-1", &HashSet::new(), "test-strand")
            .await
            .unwrap();
        assert!(matches!(result, ClaimOutcome::AllRaceLost));
    }

    #[tokio::test]
    async fn claim_error_returns_error_not_race_lost() {
        // ClaimResult::ClaimError should be distinguished from RaceLost
        let bead = make_bead("needle-abc", "/tmp/ws");
        let store = Arc::new(
            MockBeadStore::new(vec![bead.clone()]).with_claim_results(vec![
                ClaimResult::ClaimError {
                    reason: "br update exited with code 1".to_string(),
                },
            ]),
        );
        let claimer = make_claimer(store);

        let result = claimer
            .claim_next(&[bead], "worker-1", &HashSet::new(), "test-strand")
            .await
            .unwrap();

        // Single claim error with no other candidates should return AllRaceLost
        // (not Suspect yet - need consecutive errors across calls)
        assert!(matches!(result, ClaimOutcome::AllRaceLost));
    }

    #[tokio::test]
    async fn consecutive_claim_errors_trigger_suspect_outcome() {
        // After N consecutive claim errors on the same bead, should return Suspect
        let bead = make_bead("needle-abc", "/tmp/ws");
        let error_result = ClaimResult::ClaimError {
            reason: "br update exited with code 1".to_string(),
        };

        // Set up store to always return ClaimError (empty queue → default behavior)
        let store = Arc::new(
            MockBeadStore::new(vec![bead.clone()]).with_claim_results(vec![
                error_result.clone(),
                error_result.clone(),
                error_result.clone(),
            ]),
        );
        let claimer = make_claimer(store.clone());

        // First call: first error
        let result1 = claimer
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();
        assert!(matches!(result1, ClaimOutcome::AllRaceLost));

        // Second call: second error
        let result2 = claimer
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();
        assert!(matches!(result2, ClaimOutcome::AllRaceLost));

        // Third call: third error triggers Suspect
        let result3 = claimer
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();

        // After 3 errors (CLAIM_ERROR_THRESHOLD), should return Suspect
        match result3 {
            ClaimOutcome::Suspect {
                bead_id,
                consecutive_errors,
                last_error,
            } => {
                assert_eq!(bead_id, BeadId::from("needle-abc"));
                assert_eq!(consecutive_errors, 3);
                assert!(last_error.contains("br update exited with code 1"));
            }
            other => panic!("expected Suspect outcome on third call, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn successful_claim_clears_error_counter() {
        // A successful claim should reset the error counter for that bead
        let bead = make_bead("needle-abc", "/tmp/ws");
        let error_result = ClaimResult::ClaimError {
            reason: "br update exited with code 1".to_string(),
        };

        let store = Arc::new(
            MockBeadStore::new(vec![bead.clone()]).with_claim_results(vec![
                // First call: error
                error_result.clone(),
            ]),
        );
        let claimer = make_claimer(store.clone());

        // First call: record an error
        let result1 = claimer
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();
        assert!(matches!(result1, ClaimOutcome::AllRaceLost));

        // Second call: successful claim (empty results queue → default MockBeadStore behavior)
        let result2 = claimer
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();
        assert!(matches!(result2, ClaimOutcome::Claimed(_)));

        // Error counter should be cleared after successful claim
        // Verify by calling again with error - should NOT trigger Suspect (counter reset)
        let store3 = Arc::new(
            MockBeadStore::new(vec![bead.clone()]).with_claim_results(vec![
                error_result.clone(),
                error_result.clone(),
                error_result.clone(),
            ]),
        );
        let claimer3 = Claimer::new(
            store3,
            std::env::temp_dir(),
            5,
            10,
            Telemetry::new("test-worker".to_string()),
        );

        // Need 3 errors to trigger Suspect again (counter was reset to 0)
        let _ = claimer3
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();
        let _ = claimer3
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();
        let result3 = claimer3
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();

        // Should trigger Suspect on the 3rd error (counter started from 0 after success)
        match result3 {
            ClaimOutcome::Suspect {
                consecutive_errors, ..
            } => {
                assert_eq!(consecutive_errors, 3);
            }
            other => panic!("expected Suspect after 3 errors, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn store_error_returns_store_error_outcome() {
        // When the store itself returns an Err (not ClaimResult), should get StoreError
        let bead = make_bead("needle-abc", "/tmp/ws");

        // Create a custom MockBeadStore that returns errors from show()
        struct FailingShowStore {
            beads: Mutex<Vec<Bead>>,
        }

        impl FailingShowStore {
            fn new(beads: Vec<Bead>) -> Self {
                FailingShowStore {
                    beads: Mutex::new(beads),
                }
            }
        }

        #[async_trait]
        impl BeadStore for FailingShowStore {
            async fn list_all(&self) -> Result<Vec<Bead>> {
                Ok(self.beads.lock().unwrap().clone())
            }
            async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
                Ok(self.beads.lock().unwrap().clone())
            }

            async fn show(&self, _id: &BeadId) -> Result<Bead> {
                Err(anyhow!("store error: show failed"))
            }

            async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
                Ok(ClaimResult::ClaimError {
                    reason: "show failed".to_string(),
                })
            }

            async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
                Ok(ClaimResult::NotClaimable {
                    reason: "no beads available".to_string(),
                })
            }

            async fn release(&self, _id: &BeadId) -> Result<()> {
                Ok(())
            }

            async fn block(&self, _id: &BeadId) -> Result<()> {
                Ok(())
            }

            async fn flush(&self) -> Result<()> {
                Ok(())
            }

            async fn remove_dependency(
                &self,
                _blocked_id: &BeadId,
                _blocker_id: &BeadId,
            ) -> Result<()> {
                Ok(())
            }

            async fn reopen(&self, _id: &BeadId) -> Result<()> {
                Ok(())
            }

            async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
                Ok(vec![])
            }

            async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
                Ok(())
            }

            async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
                Ok(())
            }

            async fn create_bead(
                &self,
                _title: &str,
                _body: &str,
                _labels: &[&str],
            ) -> Result<BeadId> {
                Ok(BeadId::from("new-bead".to_string()))
            }

            async fn doctor_repair(&self) -> Result<RepairReport> {
                Ok(RepairReport::default())
            }
            async fn doctor_check(&self) -> Result<RepairReport> {
                Ok(RepairReport::default())
            }
            async fn full_rebuild(&self) -> Result<()> {
                Ok(())
            }
            async fn add_dependency(
                &self,
                _blocker_id: &BeadId,
                _blocked_id: &BeadId,
            ) -> Result<()> {
                Ok(())
            }

            async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
                Ok(())
            }

            fn has_valid_store(&self) -> bool {
                true
            }
        }

        let store = Arc::new(FailingShowStore::new(vec![bead.clone()]));
        let claimer = make_claimer(store);

        let result = claimer
            .claim_next(&[bead], "worker-1", &HashSet::new(), "test-strand")
            .await
            .unwrap();

        // StoreError is wrapped in Ok(), not Err()
        match result {
            ClaimOutcome::StoreError(e) => {
                assert!(e.to_string().contains("store error"));
            }
            other => panic!("expected StoreError outcome, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn suspect_outcome_includes_consecutive_count() {
        // Suspect outcome should include the count of consecutive errors
        let bead = make_bead("needle-xyz", "/tmp/ws");
        let error_result = ClaimResult::ClaimError {
            reason: "database locked".to_string(),
        };

        let store = Arc::new(
            MockBeadStore::new(vec![bead.clone()]).with_claim_results(vec![
                error_result.clone(),
                error_result.clone(),
                error_result.clone(),
            ]),
        );
        let claimer = make_claimer(store);

        // Make 3 separate claim_next calls to accumulate errors
        let _ = claimer
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();
        let _ = claimer
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();
        let result = claimer
            .claim_next(
                std::slice::from_ref(&bead),
                "worker-1",
                &HashSet::new(),
                "test-strand",
            )
            .await
            .unwrap();

        match result {
            ClaimOutcome::Suspect {
                consecutive_errors, ..
            } => {
                // Should be 3 (threshold is 3)
                assert_eq!(consecutive_errors, 3);
            }
            other => panic!("expected Suspect, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn claim_one_preserves_suspect_outcome() {
        // claim_one should preserve Suspect outcome, not convert to ClaimError
        let bead = make_bead("needle-suspect", "/tmp/ws");
        let error_result = ClaimResult::ClaimError {
            reason: "corrupted database".to_string(),
        };

        let store = Arc::new(
            MockBeadStore::new(vec![bead.clone()]).with_claim_results(vec![
                error_result.clone(),
                error_result.clone(),
                error_result.clone(),
            ]),
        );
        let claimer = make_claimer(store);

        // Make 3 separate calls to accumulate errors
        let bead_id = BeadId::from("needle-suspect");
        let _ = claimer
            .claim_one(&bead_id, "worker-1", &HashSet::new(), Some("test-strand"))
            .await
            .unwrap();
        let _ = claimer
            .claim_one(&bead_id, "worker-1", &HashSet::new(), Some("test-strand"))
            .await
            .unwrap();
        let result = claimer
            .claim_one(&bead_id, "worker-1", &HashSet::new(), Some("test-strand"))
            .await
            .unwrap();

        match result {
            ClaimResult::Suspect {
                bead_id,
                consecutive_errors,
                last_error,
            } => {
                assert_eq!(bead_id, BeadId::from("needle-suspect"));
                assert_eq!(consecutive_errors, 3);
                assert!(last_error.contains("corrupted database"));
            }
            other => panic!("expected Suspect result from claim_one, got {:?}", other),
        }
    }
}
