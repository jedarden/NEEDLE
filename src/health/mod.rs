//! Health monitoring: heartbeats, stuck detection, peer awareness.
//!
//! Workers emit periodic heartbeats. Peers with stale heartbeats are
//! considered stuck. Recovery paths are explicit.
//!
//! Depends on: `types`, `telemetry`, `config`.

use anyhow::Result;

use crate::config::Config;
use crate::telemetry::Telemetry;
use crate::types::BeadId;

/// Health monitor for a single worker.
pub struct HealthMonitor {
    config: Config,
    telemetry: Telemetry,
    worker_name: String,
}

impl HealthMonitor {
    pub fn new(config: Config, worker_name: String, telemetry: Telemetry) -> Self {
        HealthMonitor {
            config,
            telemetry,
            worker_name,
        }
    }

    /// Start the heartbeat emitter (runs in background task).
    ///
    /// Phase 2 stub: logs intent, does not emit heartbeat files yet.
    pub async fn start_heartbeat(&self, current_bead: Option<BeadId>) -> Result<()> {
        let _ = (
            current_bead,
            &self.config,
            &self.telemetry,
            &self.worker_name,
        );
        tracing::debug!(worker = %self.worker_name, "heartbeat not yet implemented (Phase 2)");
        Ok(())
    }

    /// Update the heartbeat with the currently-claimed bead.
    ///
    /// Phase 2 stub: no-op in Phase 1.
    pub async fn update_heartbeat(&self, bead_id: Option<&BeadId>) -> Result<()> {
        let _ = bead_id;
        Ok(())
    }

    /// Scan for peer workers with stale heartbeats.
    ///
    /// Phase 2 stub: returns empty list.
    pub async fn detect_stuck_peers(&self) -> Result<Vec<StuckPeer>> {
        Ok(vec![])
    }

    /// Attempt recovery for a stuck peer's claimed bead.
    ///
    /// Phase 2 stub: no-op.
    pub async fn recover_stuck_bead(&self, bead_id: &BeadId) -> Result<()> {
        let _ = bead_id;
        Ok(())
    }
}

/// A peer worker detected as stuck (heartbeat TTL exceeded).
#[derive(Debug)]
pub struct StuckPeer {
    pub worker_name: String,
    pub claimed_bead: Option<BeadId>,
    pub age_secs: u64,
}
