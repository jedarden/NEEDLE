//! Atomic bead claiming with per-workspace flock serialization.
//!
//! The Claimer wraps `BeadStore.claim()` with coordination that prevents
//! thundering herd. A per-workspace flock serializes claim operations so
//! workers take turns rather than racing on the same bead.
//!
//! Depends on: `types`, `bead_store`, `telemetry`.

use std::collections::HashSet;
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

/// Atomic bead claimer with workspace-level flock serialization.
pub struct Claimer {
    store: Arc<dyn BeadStore>,
    lock_dir: PathBuf,
    max_retries: u32,
    retry_backoff_ms: u64,
    telemetry: Telemetry,
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
        }
    }

    /// Attempt to claim the next available bead from the candidate list.
    ///
    /// Iterates candidates in priority order, skipping those in the exclusion
    /// set. For each candidate, acquires a per-workspace flock, verifies the
    /// bead is still claimable, and attempts the claim.
    ///
    /// Returns:
    /// - `Claimed(bead)`: successfully claimed a bead
    /// - `AllRaceLost`: tried candidates, all race-lost
    /// - `NoCandidates`: no candidates after filtering exclusions
    /// - `StoreError(e)`: bead store or flock error
    pub async fn claim_next(
        &self,
        candidates: &[Bead],
        actor: &str,
        exclusions: &HashSet<BeadId>,
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
                return Ok(ClaimOutcome::AllRaceLost);
            }

            attempts += 1;
            let bead_id = &candidate.id;

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
                    return Ok(ClaimOutcome::StoreError(e));
                }
            };

            // Verify bead is still claimable (status=open, no assignee)
            let current = match self.store.show(bead_id).await {
                Ok(b) => b,
                Err(e) => {
                    drop(lock_file);
                    self.telemetry.emit(EventKind::ClaimFailed {
                        bead_id: bead_id.clone(),
                        reason: format!("verify failed: {e}"),
                    })?;
                    return Ok(ClaimOutcome::StoreError(e));
                }
            };

            if current.status != BeadStatus::Open || current.assignee.is_some() {
                drop(lock_file);
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

            // Attempt claim via store
            let result = self.store.claim(bead_id, actor).await;
            drop(lock_file);

            match result {
                Ok(ClaimResult::Claimed(bead)) => {
                    self.telemetry.emit(EventKind::ClaimSuccess {
                        bead_id: bead_id.clone(),
                    })?;
                    return Ok(ClaimOutcome::Claimed(bead));
                }
                Ok(ClaimResult::RaceLost { .. }) => {
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
                    self.telemetry.emit(EventKind::ClaimFailed {
                        bead_id: bead_id.clone(),
                        reason: reason.clone(),
                    })?;
                    continue;
                }
                Err(e) => {
                    self.telemetry.emit(EventKind::ClaimFailed {
                        bead_id: bead_id.clone(),
                        reason: format!("store error: {e}"),
                    })?;
                    return Ok(ClaimOutcome::StoreError(e));
                }
            }
        }

        // Exhausted all eligible candidates without success
        Ok(ClaimOutcome::AllRaceLost)
    }

    /// Convenience: claim a single bead by ID (fetches the bead, then delegates
    /// to `claim_next` with a single-element candidate list).
    pub async fn claim_one(&self, bead_id: &BeadId, actor: &str) -> Result<ClaimResult> {
        let bead = self.store.show(bead_id).await?;
        let exclusions = HashSet::new();
        match self.claim_next(&[bead], actor, &exclusions).await? {
            ClaimOutcome::Claimed(b) => Ok(ClaimResult::Claimed(b)),
            ClaimOutcome::AllRaceLost => Ok(ClaimResult::RaceLost {
                claimed_by: "(race)".to_string(),
            }),
            ClaimOutcome::NoCandidates => Ok(ClaimResult::NotClaimable {
                reason: "no candidates".to_string(),
            }),
            ClaimOutcome::StoreError(e) => Err(e),
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
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// Mock bead store that returns configurable claim results.
    struct MockBeadStore {
        beads: Mutex<Vec<Bead>>,
        /// Claim results consumed in FIFO order; when empty, claims succeed.
        claim_results: Mutex<Vec<ClaimResult>>,
    }

    impl MockBeadStore {
        fn new(beads: Vec<Bead>) -> Self {
            MockBeadStore {
                beads: Mutex::new(beads),
                claim_results: Mutex::new(vec![]),
            }
        }

        fn with_claim_results(self, results: Vec<ClaimResult>) -> Self {
            *self.claim_results.lock().unwrap() = results;
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

        async fn release(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn flush(&self) -> Result<()> {
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
    async fn claim_next_empty_candidates_returns_no_candidates() {
        let store = Arc::new(MockBeadStore::new(vec![]));
        let claimer = make_claimer(store);
        let result = claimer
            .claim_next(&[], "worker-1", &HashSet::new())
            .await
            .unwrap();
        assert!(matches!(result, ClaimOutcome::NoCandidates));
    }

    #[tokio::test]
    async fn claim_next_all_excluded_returns_no_candidates() {
        let bead = make_bead("needle-abc", "/tmp/ws");
        let store = Arc::new(MockBeadStore::new(vec![bead.clone()]));
        let claimer = make_claimer(store);
        let mut exclusions = HashSet::new();
        exclusions.insert(BeadId::from("needle-abc"));

        let result = claimer
            .claim_next(&[bead], "worker-1", &exclusions)
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
            .claim_next(&[bead], "worker-1", &HashSet::new())
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
            .claim_next(&[bead1, bead2], "worker-1", &HashSet::new())
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
            .claim_next(&[bead1, bead2], "worker-1", &HashSet::new())
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
            .claim_next(&[bead1, bead2], "worker-1", &HashSet::new())
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
            .claim_one(&BeadId::from("needle-abc"), "worker-1")
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
            .claim_next(&[bead], "worker-1", &exclusions)
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
            .claim_next(&beads, "worker-1", &HashSet::new())
            .await
            .unwrap();
        assert!(matches!(result, ClaimOutcome::AllRaceLost));
    }
}
