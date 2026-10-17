//! Per-workspace worker-capacity checks.
//!
//! A workspace cap is based on the work actually held in the workspace, not
//! on the host-local worker count. An in-progress assignee only consumes a
//! slot while its fresh heartbeat shows it working here: on one of the beads
//! it holds in this workspace, or in the claim window before its heartbeat
//! file has caught up with the claim. A crashed or stopped worker (stale
//! heartbeat) and a live worker that moved on to another workspace or bead
//! (heartbeat elsewhere) both leave their old claims behind without holding
//! a slot, so a later worker can reclaim the capacity (needle-a6c92ddd).
//!
//! # When the heartbeat shows a claim
//!
//! The worker publishes the candidate bead and that bead's workspace to its
//! heartbeat state at selection, *before* the claim (needle-c63c), and keeps
//! them through CLAIMING, BUILDING, DISPATCHING, EXECUTING and HANDLING until
//! the next SELECTING cycle clears them. The heartbeat *file*, however, is
//! rewritten only once per heartbeat interval. For up to one interval after a
//! claim the file can still show the pre-selection state: SELECTING or
//! EXHAUSTED, no bead, and the worker's home workspace, which for an
//! Explore-roaming claim is not this workspace. That lag is the claim window.
//! It is recognized by timestamp order, not by workspace: a heartbeat written
//! no later than a held bead's last update (the claim) cannot show that claim
//! yet, so it is not evidence that the holder left.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::bead_store::BeadStore;
use crate::config::ConfigLoader;
use crate::health::{HealthMonitor, HeartbeatData};
use crate::types::{Bead, BeadStatus, WorkerState};

/// Clock-precision allowance when ordering a heartbeat against a claim. Both
/// timestamps come from this host's clock, but a backend may store the
/// claim time at a coarser precision than the heartbeat.
const CLAIM_WINDOW_SKEW: chrono::Duration = chrono::Duration::seconds(2);

/// The result of checking a workspace with an explicit positive cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapacitySnapshot {
    pub max_workers: u32,
    pub active_workers: usize,
    /// Holders whose fresh heartbeat places them away from every bead they
    /// hold here. Their claims are abandoned in practice; a non-zero value
    /// is how a ghost-locked workspace shows up.
    pub excluded_elsewhere: usize,
}

impl CapacitySnapshot {
    pub(crate) fn at_capacity(&self) -> bool {
        self.active_workers >= self.max_workers as usize
    }
}

/// How the distinct in-progress assignees of one workspace were classified.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HolderCount {
    /// Holders working here: on a held bead, or in the claim window.
    pub active: usize,
    /// Holders with a fresh heartbeat that shows another bead or workspace.
    pub elsewhere: usize,
    /// Holders with no heartbeat, or one older than the TTL.
    pub stale: usize,
}

/// Where one holder's heartbeat places it relative to its claims here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// The heartbeat shows the holder on one of the beads it holds here.
    OnHeldBead,
    /// The heartbeat predates a held bead's claim, so it cannot show it yet.
    ClaimWindow,
    /// A fresh heartbeat that shows the holder anywhere else.
    Elsewhere,
    /// No heartbeat, or one past the TTL.
    Stale,
}

/// Read the workspace-owned worker cap from `.needle.yaml`.
pub(crate) fn configured_max_workers(workspace: &Path) -> Result<Option<u32>> {
    let overrides = ConfigLoader::load_workspace(workspace)?;
    Ok(overrides
        .and_then(|overrides| overrides.workspace)
        .and_then(|workspace| workspace.max_workers)
        .filter(|max_workers| *max_workers > 0))
}

/// Whether a heartbeat state can place a worker on a bead. Mirrors the
/// states that may publish claim ownership in the heartbeat.
fn state_works_bead(state: &WorkerState) -> bool {
    !matches!(
        state,
        WorkerState::Exhausted | WorkerState::Stopped | WorkerState::Errored
    )
}

fn place_holder(
    held: &[&Bead],
    heartbeat: Option<&HeartbeatData>,
    heartbeat_ttl: Duration,
) -> Placement {
    let Some(heartbeat) = heartbeat.filter(|hb| !HealthMonitor::is_stale(hb, heartbeat_ttl)) else {
        return Placement::Stale;
    };

    let on_held_bead = state_works_bead(&heartbeat.state)
        && heartbeat
            .current_bead
            .as_ref()
            .is_some_and(|current| held.iter().any(|bead| &bead.id == current));
    if on_held_bead {
        return Placement::OnHeldBead;
    }

    if held
        .iter()
        .any(|bead| heartbeat.last_heartbeat <= bead.updated_at + CLAIM_WINDOW_SKEW)
    {
        return Placement::ClaimWindow;
    }

    Placement::Elsewhere
}

/// Classify the distinct assignees of in-progress beads against heartbeats.
pub(crate) fn classify_holders(
    in_progress: &[Bead],
    heartbeats: &[HeartbeatData],
    heartbeat_ttl: Duration,
) -> HolderCount {
    let mut held_by: HashMap<&str, Vec<&Bead>> = HashMap::new();
    for bead in in_progress
        .iter()
        .filter(|bead| bead.status == BeadStatus::InProgress)
    {
        if let Some(assignee) = bead.assignee.as_deref() {
            held_by.entry(assignee).or_default().push(bead);
        }
    }

    // A worker identity has one heartbeat file. Should two readings ever
    // share an identity, the newest one is the worker's current position.
    let mut latest: HashMap<&str, &HeartbeatData> = HashMap::new();
    for heartbeat in heartbeats {
        let entry = latest
            .entry(heartbeat.qualified_id.as_str())
            .or_insert(heartbeat);
        if heartbeat.last_heartbeat > entry.last_heartbeat {
            *entry = heartbeat;
        }
    }

    let mut count = HolderCount::default();
    for (assignee, held) in &held_by {
        match place_holder(held, latest.get(assignee).copied(), heartbeat_ttl) {
            Placement::OnHeldBead | Placement::ClaimWindow => count.active += 1,
            Placement::Elsewhere => {
                tracing::debug!(
                    assignee = %assignee,
                    held = ?held.iter().map(|bead| bead.id.to_string()).collect::<Vec<_>>(),
                    "in-progress holder is live elsewhere; not counted toward workspace capacity"
                );
                count.elsewhere += 1;
            }
            Placement::Stale => count.stale += 1,
        }
    }
    count
}

/// Read the heartbeat directory and classify in-progress holders.
pub(crate) fn count_holders(
    in_progress: &[Bead],
    heartbeat_dir: &Path,
    heartbeat_ttl: Duration,
) -> Result<HolderCount> {
    let heartbeats = HealthMonitor::read_all_heartbeats(heartbeat_dir).with_context(|| {
        format!(
            "failed to read heartbeats while checking workspace capacity: {}",
            heartbeat_dir.display()
        )
    })?;
    Ok(classify_holders(in_progress, &heartbeats, heartbeat_ttl))
}

/// Check a workspace cap using one status-filtered bead inventory query.
///
/// `None` means the workspace is unlimited (including the conventional `0`
/// value). Errors are returned so callers can skip the workspace rather than
/// accidentally dispatching while a configured cap cannot be evaluated.
pub(crate) async fn check(
    workspace: &Path,
    store: &dyn BeadStore,
    heartbeat_dir: &Path,
    heartbeat_ttl: Duration,
) -> Result<Option<CapacitySnapshot>> {
    let Some(max_workers) = configured_max_workers(workspace)? else {
        return Ok(None);
    };

    let in_progress = store.list_in_progress().await.with_context(|| {
        format!(
            "failed to list in-progress beads in {}",
            workspace.display()
        )
    })?;
    let holders = count_holders(&in_progress, heartbeat_dir, heartbeat_ttl)?;

    Ok(Some(CapacitySnapshot {
        max_workers,
        active_workers: holders.active,
        excluded_elsewhere: holders.elsewhere,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use std::path::PathBuf;

    use crate::types::BeadId;

    const TTL: Duration = Duration::from_secs(300);

    fn bead(id: &str, assignee: Option<&str>) -> Bead {
        Bead {
            id: BeadId::from(id),
            title: id.to_string(),
            body: None,
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: assignee.map(str::to_owned),
            labels: Vec::new(),
            workspace: PathBuf::from("/workspace/needle"),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: Utc::now() - chrono::Duration::days(4),
            updated_at: Utc::now() - chrono::Duration::days(4),
        }
    }

    fn claimed_at(mut bead: Bead, at: DateTime<Utc>) -> Bead {
        bead.updated_at = at;
        bead
    }

    fn heartbeat(
        worker_id: &str,
        state: WorkerState,
        current_bead: Option<&str>,
        workspace: &str,
    ) -> HeartbeatData {
        HeartbeatData {
            worker_id: worker_id.to_string(),
            qualified_id: worker_id.to_string(),
            pid: std::process::id(),
            state,
            current_bead: current_bead.map(BeadId::from),
            workspace: PathBuf::from(workspace),
            last_heartbeat: Utc::now(),
            started_at: Utc::now(),
            beads_processed: 0,
            beads_completed: 0,
            session: "test".to_string(),
            is_idle: false,
            current_task: None,
            model: String::new(),
            heartbeat_file: None,
            activity: None,
            claim_identity: None,
        }
    }

    fn written_at(mut heartbeat: HeartbeatData, at: DateTime<Utc>) -> HeartbeatData {
        heartbeat.last_heartbeat = at;
        heartbeat
    }

    #[test]
    fn counts_distinct_holders_on_their_beads_through_the_heartbeat_dir() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join(".needle.yaml"),
            "workspace:\n  max_workers: 1\n",
        )
        .unwrap();
        assert_eq!(configured_max_workers(workspace.path()).unwrap(), Some(1));

        for (worker, current) in [("worker-a", "two"), ("worker-b", "three")] {
            let data = heartbeat(
                worker,
                WorkerState::Executing,
                Some(current),
                "/workspace/needle",
            );
            std::fs::write(
                dir.path().join(format!("{worker}.json")),
                serde_json::to_vec(&data).unwrap(),
            )
            .unwrap();
        }

        let beads = vec![
            bead("one", Some("worker-a")),
            bead("two", Some("worker-a")),
            bead("three", Some("worker-b")),
            bead("four", Some("stale-worker")),
        ];

        assert_eq!(
            count_holders(&beads, dir.path(), TTL).unwrap(),
            HolderCount {
                active: 2,
                elsewhere: 0,
                stale: 1,
            }
        );
    }

    #[test]
    fn holder_executing_elsewhere_is_not_counted() {
        // The 2026-10-17 ghost lock: a NEEDLE claim from days ago, while the
        // live worker's heartbeat shows it executing a bead in ~/shephrd.
        // Also crash-and-relaunch under the same name: the worker sits
        // EXHAUSTED in this very workspace beside its prior incarnation's
        // claim. Being in the workspace is not working the bead.
        let beads = vec![
            bead("needle-b184d5d7", Some("glm-front-archivist")),
            bead("needle-old", Some("relaunched")),
        ];
        let heartbeats = vec![
            heartbeat(
                "glm-front-archivist",
                WorkerState::Executing,
                Some("shephrd-c525438f"),
                "/home/coding/shephrd",
            ),
            heartbeat(
                "relaunched",
                WorkerState::Exhausted,
                None,
                "/workspace/needle",
            ),
        ];

        assert_eq!(
            classify_holders(&beads, &heartbeats, TTL),
            HolderCount {
                active: 0,
                elsewhere: 2,
                stale: 0,
            }
        );
    }

    #[test]
    fn holder_executing_this_bead_is_counted() {
        let beads = vec![
            bead("needle-old", Some("worker-a")),
            bead("needle-live", Some("worker-a")),
        ];
        let heartbeats = vec![heartbeat(
            "worker-a",
            WorkerState::Executing,
            Some("needle-live"),
            "/workspace/needle",
        )];

        assert_eq!(
            classify_holders(&beads, &heartbeats, TTL),
            HolderCount {
                active: 1,
                elsewhere: 0,
                stale: 0,
            }
        );
    }

    #[test]
    fn holder_in_the_claim_window_is_counted() {
        // Explore-roaming claim: the heartbeat file was written before the
        // claim and still shows the worker selecting in its home workspace.
        let now = Utc::now();
        let beads = vec![claimed_at(
            bead("needle-new", Some("roamer")),
            now - chrono::Duration::seconds(3),
        )];
        let heartbeats = vec![written_at(
            heartbeat("roamer", WorkerState::Selecting, None, "/workspace/home"),
            now - chrono::Duration::seconds(20),
        )];

        assert_eq!(
            classify_holders(&beads, &heartbeats, TTL),
            HolderCount {
                active: 1,
                elsewhere: 0,
                stale: 0,
            }
        );

        // Once the heartbeat file is rewritten after the claim without the
        // bead, the window has closed and the holder is elsewhere.
        let rewritten = vec![heartbeat(
            "roamer",
            WorkerState::Executing,
            Some("other-1"),
            "/workspace/other",
        )];
        assert_eq!(classify_holders(&beads, &rewritten, TTL).elsewhere, 1);
    }

    #[test]
    fn holder_with_stale_heartbeat_is_not_counted() {
        let now = Utc::now();
        let beads = vec![
            bead("needle-a", Some("crashed")),
            bead("needle-b", Some("missing")),
        ];
        let heartbeats = vec![written_at(
            heartbeat(
                "crashed",
                WorkerState::Executing,
                Some("needle-a"),
                "/workspace/needle",
            ),
            now - chrono::Duration::seconds(600),
        )];

        assert_eq!(
            classify_holders(&beads, &heartbeats, TTL),
            HolderCount {
                active: 0,
                elsewhere: 0,
                stale: 2,
            }
        );
    }
}
