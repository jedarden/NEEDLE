//! Native checkpoint admission for every descriptor-driven claim path.
//! Read-only diagnostics remain available while the workspace is paused.

use super::CliBeadStore;
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::Deserialize;
use std::fs::{File, OpenOptions};
use std::time::Duration;

#[derive(Deserialize)]
struct SyncStatus {
    relationship: String,
    ready_to_commit: bool,
    #[serde(default)]
    not_ready_reasons: Vec<String>,
}

#[derive(Default, Deserialize)]
struct WorkspaceQueueConfig {
    #[serde(default)]
    queue: QueueOwner,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueOwner {
    owner_host: Option<String>,
}

fn check_queue_owner(workspace: &std::path::Path) -> Result<()> {
    let path = workspace.join(".needle.yaml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let config: WorkspaceQueueConfig =
        serde_yaml::from_str(&text).context("cannot validate queue.owner_host in .needle.yaml")?;
    if let Some(owner) = config.queue.owner_host {
        let hostname = gethostname::gethostname();
        if owner.trim().is_empty() || hostname.to_str() != Some(owner.as_str()) {
            bail!(
                "workspace_queue_owner_mismatch: queue.owner_host is {owner:?}; this host is {:?}",
                hostname
            );
        }
    }
    Ok(())
}

pub(super) fn guarded_operation(name: &str) -> bool {
    matches!(
        name,
        "ready"
            | "claim"
            | "claim_auto"
            | "release"
            | "block"
            | "clear_assignee"
            | "flush"
            | "reopen"
            | "close"
            | "label_add"
            | "label_remove"
            | "create"
            | "create_id"
            | "dep_add"
            | "split"
            | "dep_remove"
            | "update"
            | "doctor_repair"
            | "import"
            | "ref_add"
            | "ref_remove"
            | "data_set"
            | "data_remove"
            | "recurrence_add"
            | "recurrence_remove"
    )
}

impl CliBeadStore {
    async fn checkpoint_status(&self) -> Result<SyncStatus> {
        let args = ["sync", "status", "--format", "json"].map(str::to_string);
        let stdout = self.run_argv_unchecked("sync_status", &args, 30).await?;
        serde_json::from_str(&stdout).context("bead-rs sync status returned an invalid contract")
    }

    /// Serialize this worker fleet's status/reconcile/claim sequence. Native
    /// bead-rs additionally guards its own writes; this lock does not claim
    /// to coordinate independent host databases or uncooperative Git pulls.
    pub(super) async fn prepare_checkpoint(&self) -> Result<Option<File>> {
        if self.backend().name != "bead-rs"
            || !self.workspace().join(".beads/config.json").is_file()
        {
            return Ok(None);
        }
        let result = self.prepare_checkpoint_inner().await;
        match &result {
            Ok(_) => self.set_sync_pause(None),
            Err(error) => self.set_sync_pause(Some(format!("{error:#}"))),
        }
        result
    }

    async fn prepare_checkpoint_inner(&self) -> Result<Option<File>> {
        check_queue_owner(self.workspace())?;
        let path = self.workspace().join(".beads/needle-sync.lock");
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if tokio::time::Instant::now() >= deadline {
                        bail!("workspace_sync_busy: another worker is synchronizing this queue");
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let status = self.checkpoint_status().await?;
        match status.relationship.as_str() {
            "aligned" if status.ready_to_commit => return Ok(Some(file)),
            "remote-advanced" => {
                let args = ["sync", "reconcile", "--actor", "needle-sync"].map(str::to_string);
                self.run_argv_unchecked("sync_reconcile", &args, 30).await?;
            }
            "behind" | "absent" => {
                let args = ["sync", "flush-only"].map(str::to_string);
                self.run_argv_unchecked("sync_flush", &args, 30).await?;
            }
            _ => bail!(
                "workspace_sync_paused: {}: {}",
                status.relationship,
                status.not_ready_reasons.join("; ")
            ),
        }
        self.verify_checkpoint_published().await?;
        Ok(Some(file))
    }

    pub(super) async fn verify_checkpoint_published(&self) -> Result<()> {
        let status = self.checkpoint_status().await?;
        if status.relationship != "aligned" || !status.ready_to_commit {
            bail!(
                "workspace_sync_paused: checkpoint publication is not verified: {}: {}",
                status.relationship,
                status.not_ready_reasons.join("; ")
            );
        }
        Ok(())
    }
}
