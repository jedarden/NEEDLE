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
    pub fn new(config: Config, telemetry: Telemetry) -> Self {
        let worker_name = config.worker_name.clone();
        HealthMonitor {
            config,
            telemetry,
            worker_name,
        }
    }

    /// Start the heartbeat emitter (runs in background task).
    pub async fn start_heartbeat(&self, current_bead: Option<BeadId>) -> Result<()> {
        // TODO(needle-nva): implement file-based heartbeat with TTL
        let _ = (
            current_bead,
            &self.config,
            &self.telemetry,
            &self.worker_name,
        );
        todo!("HealthMonitor::start_heartbeat")
    }

    /// Update the heartbeat with the currently-claimed bead.
    pub async fn update_heartbeat(&self, bead_id: Option<&BeadId>) -> Result<()> {
        // TODO(needle-nva): write heartbeat file
        let _ = bead_id;
        todo!("HealthMonitor::update_heartbeat")
    }

    /// Scan for peer workers with stale heartbeats.
    pub async fn detect_stuck_peers(&self) -> Result<Vec<StuckPeer>> {
        // TODO(needle-nva): scan heartbeat files, compare timestamps
        todo!("HealthMonitor::detect_stuck_peers")
    }

    /// Attempt recovery for a stuck peer's claimed bead.
    pub async fn recover_stuck_bead(&self, bead_id: &BeadId) -> Result<()> {
        // TODO(needle-nva): reset bead to open if peer is gone
        let _ = bead_id;
        todo!("HealthMonitor::recover_stuck_bead")
    }
}

/// A peer worker detected as stuck (heartbeat TTL exceeded).
#[derive(Debug)]
pub struct StuckPeer {
    pub worker_name: String,
    pub claimed_bead: Option<BeadId>,
    pub age_secs: u64,
}
