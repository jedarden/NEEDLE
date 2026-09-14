//! A permissive bead store for tests that exercise dispatch mechanics.
//!
//! The dispatcher refuses to spawn an agent unless its pre-spawn claim
//! verification passes (fail closed). Tests that target timeouts, process
//! contracts, and lifecycle mechanics — not claim verification — wire this
//! store so verification passes and the tested spawn behavior runs.
//! Verification-specific behavior belongs to the fail-closed tests, never
//! here.

use async_trait::async_trait;

use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::types::{Bead, BeadId, BeadStatus, ClaimResult, ClaimStatus};

/// Answers every `claim_status` with an `in_progress` claim owned by the
/// configured worker, so pre-spawn verification passes.
pub struct AlwaysClaimedStore {
    worker_id: String,
}

impl AlwaysClaimedStore {
    pub fn new(worker_id: &str) -> Self {
        AlwaysClaimedStore {
            worker_id: worker_id.to_string(),
        }
    }
}

#[async_trait]
impl BeadStore for AlwaysClaimedStore {
    async fn ready(&self, _filters: &Filters) -> anyhow::Result<Vec<Bead>> {
        Ok(vec![])
    }

    async fn list_all(&self) -> anyhow::Result<Vec<Bead>> {
        Ok(vec![])
    }

    async fn show(&self, _id: &BeadId) -> anyhow::Result<Bead> {
        anyhow::bail!("AlwaysClaimedStore serves claim_status only")
    }

    async fn claim_status(&self, _id: &BeadId) -> anyhow::Result<ClaimStatus> {
        Ok(ClaimStatus {
            status: BeadStatus::InProgress,
            assignee: Some(self.worker_id.clone()),
            revision: None,
            claim_epoch: None,
        })
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> anyhow::Result<ClaimResult> {
        anyhow::bail!("AlwaysClaimedStore serves claim_status only")
    }

    async fn claim_auto(&self, _actor: &str) -> anyhow::Result<ClaimResult> {
        anyhow::bail!("AlwaysClaimedStore serves claim_status only")
    }

    async fn release(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn block(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn create_bead(
        &self,
        _title: &str,
        _body: &str,
        _labels: &[&str],
    ) -> anyhow::Result<BeadId> {
        anyhow::bail!("AlwaysClaimedStore serves claim_status only")
    }

    async fn add_dependency(
        &self,
        _blocker_id: &BeadId,
        _blocked_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_dependency(
        &self,
        _blocked_id: &BeadId,
        _blocker_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> anyhow::Result<RepairReport> {
        Ok(RepairReport {
            warnings: Vec::new(),
            fixed: Vec::new(),
        })
    }

    async fn doctor_check(&self) -> anyhow::Result<RepairReport> {
        Ok(RepairReport {
            warnings: Vec::new(),
            fixed: Vec::new(),
        })
    }

    async fn full_rebuild(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}
