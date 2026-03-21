//! Abstract interface to the bead backend.
//!
//! NEEDLE interacts with beads exclusively through this trait. The default
//! implementation shells out to the `br` CLI. Future backends (direct SQLite,
//! remote API) implement the same trait.
//!
//! Leaf module — depends only on `types`.

use anyhow::Result;
use async_trait::async_trait;

use crate::types::{Bead, BeadId, BeadStatus};

/// Abstract bead store interface.
#[async_trait]
pub trait BeadStore: Send + Sync {
    /// List all beads with no incomplete blockers (ready to work on).
    async fn list_ready(&self) -> Result<Vec<Bead>>;

    /// Fetch a single bead by ID.
    async fn get(&self, id: &BeadId) -> Result<Bead>;

    /// Atomically claim a bead: set status=in_progress and assignee.
    ///
    /// Returns `Ok(true)` if the claim succeeded, `Ok(false)` if another
    /// worker raced us to it.
    async fn claim(&self, id: &BeadId, assignee: &str) -> Result<bool>;

    /// Update a bead's status.
    async fn set_status(&self, id: &BeadId, status: BeadStatus) -> Result<()>;

    /// Close a bead with a completion summary.
    async fn close(&self, id: &BeadId, body: &str) -> Result<()>;

    /// Check connectivity to the bead store.
    async fn health_check(&self) -> Result<()>;
}

/// `br` CLI-backed bead store implementation.
pub struct BrCliBeadStore {
    /// Path to the `br` binary.
    pub br_path: std::path::PathBuf,
    /// Workspace directory (where `.beads/` lives).
    pub workspace: std::path::PathBuf,
}

impl BrCliBeadStore {
    pub fn new(br_path: std::path::PathBuf, workspace: std::path::PathBuf) -> Self {
        BrCliBeadStore { br_path, workspace }
    }
}

#[async_trait]
impl BeadStore for BrCliBeadStore {
    async fn list_ready(&self) -> Result<Vec<Bead>> {
        // TODO(needle-0ez): implement br ready parsing
        todo!("BrCliBeadStore::list_ready")
    }

    async fn get(&self, _id: &BeadId) -> Result<Bead> {
        // TODO(needle-0ez): implement br show parsing
        todo!("BrCliBeadStore::get")
    }

    async fn claim(&self, _id: &BeadId, _assignee: &str) -> Result<bool> {
        // TODO(needle-0ez): implement atomic claim via br update --claim
        todo!("BrCliBeadStore::claim")
    }

    async fn set_status(&self, _id: &BeadId, _status: BeadStatus) -> Result<()> {
        // TODO(needle-0ez): implement br update --status
        todo!("BrCliBeadStore::set_status")
    }

    async fn close(&self, _id: &BeadId, _body: &str) -> Result<()> {
        // TODO(needle-0ez): implement br close
        todo!("BrCliBeadStore::close")
    }

    async fn health_check(&self) -> Result<()> {
        // TODO(needle-0ez): implement br doctor check
        todo!("BrCliBeadStore::health_check")
    }
}
