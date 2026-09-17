//! Per-workspace worker-capacity checks.
//!
//! A workspace cap is based on the work actually held in the workspace, not
//! on the host-local worker count. An assignee only consumes a slot while its
//! heartbeat is fresh, which lets a later worker reclaim capacity after a
//! crashed or stopped worker has left an old in-progress bead behind.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::bead_store::BeadStore;
use crate::config::ConfigLoader;
use crate::health::HealthMonitor;
use crate::types::{Bead, BeadStatus};

/// The result of checking a workspace with an explicit positive cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapacitySnapshot {
    pub max_workers: u32,
    pub active_workers: usize,
}

impl CapacitySnapshot {
    pub(crate) fn at_capacity(&self) -> bool {
        self.active_workers >= self.max_workers as usize
    }
}

/// Read the workspace-owned worker cap from `.needle.yaml`.
pub(crate) fn configured_max_workers(workspace: &Path) -> Result<Option<u32>> {
    let overrides = ConfigLoader::load_workspace(workspace)?;
    Ok(overrides
        .and_then(|overrides| overrides.workspace)
        .and_then(|workspace| workspace.max_workers)
        .filter(|max_workers| *max_workers > 0))
}

/// Count distinct assignees whose heartbeat is fresh among in-progress beads.
pub(crate) fn count_live_assignees(
    in_progress: &[Bead],
    heartbeat_dir: &Path,
    heartbeat_ttl: Duration,
) -> Result<usize> {
    let fresh_worker_ids: HashSet<String> = HealthMonitor::read_all_heartbeats(heartbeat_dir)
        .with_context(|| {
            format!(
                "failed to read heartbeats while checking workspace capacity: {}",
                heartbeat_dir.display()
            )
        })?
        .into_iter()
        .filter(|heartbeat| !HealthMonitor::is_stale(heartbeat, heartbeat_ttl))
        .map(|heartbeat| heartbeat.qualified_id)
        .collect();

    Ok(in_progress
        .iter()
        .filter(|bead| bead.status == BeadStatus::InProgress)
        .filter_map(|bead| bead.assignee.as_deref())
        .filter(|assignee| fresh_worker_ids.contains(*assignee))
        .map(str::to_owned)
        .collect::<HashSet<_>>()
        .len())
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
    let active_workers = count_live_assignees(&in_progress, heartbeat_dir, heartbeat_ttl)?;

    Ok(Some(CapacitySnapshot {
        max_workers,
        active_workers,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::health::HeartbeatData;
    use crate::types::{BeadId, WorkerState};

    fn bead(id: &str, assignee: Option<&str>) -> Bead {
        Bead {
            id: BeadId::from(id),
            title: id.to_string(),
            body: None,
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: assignee.map(str::to_owned),
            labels: Vec::new(),
            workspace: std::path::PathBuf::new(),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn heartbeat(worker_id: &str) -> HeartbeatData {
        HeartbeatData {
            worker_id: worker_id.to_string(),
            qualified_id: worker_id.to_string(),
            pid: std::process::id(),
            state: WorkerState::Building,
            current_bead: Some(BeadId::from("bead")),
            workspace: std::path::PathBuf::from("/workspace"),
            last_heartbeat: Utc::now(),
            started_at: Utc::now(),
            beads_processed: 0,
            session: "test".to_string(),
            is_idle: false,
            current_task: None,
            model: String::new(),
            heartbeat_file: None,
        }
    }

    #[test]
    fn counts_distinct_fresh_assignees_only() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join(".needle.yaml"),
            "workspace:\n  max_workers: 1\n",
        )
        .unwrap();
        assert_eq!(configured_max_workers(workspace.path()).unwrap(), Some(1));

        for worker in ["worker-a", "worker-b"] {
            let data = heartbeat(worker);
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
            count_live_assignees(&beads, dir.path(), Duration::from_secs(300)).unwrap(),
            2
        );
    }
}
