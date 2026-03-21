//! Structured telemetry emission.
//!
//! All state transitions, claim attempts, dispatches, and outcomes emit
//! structured JSONL records. Telemetry is never interleaved with agent output.
//!
//! Leaf module — depends only on `types`.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{BeadId, WorkerId, WorkerState};

/// A single telemetry event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    pub ts: DateTime<Utc>,
    pub worker_id: WorkerId,
    pub event: EventKind,
}

/// Discriminated union of all event kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    WorkerStarted { worker_name: String },
    WorkerStopped { reason: String },
    StateTransition { from: WorkerState, to: WorkerState },
    ClaimAttempt { bead_id: BeadId, attempt: u32 },
    ClaimSuccess { bead_id: BeadId },
    ClaimRaceLost { bead_id: BeadId },
    DispatchStarted { bead_id: BeadId, agent: String },
    DispatchCompleted { bead_id: BeadId, exit_code: i32 },
    HeartbeatEmitted { bead_id: Option<BeadId> },
    StuckDetected { bead_id: BeadId, age_secs: u64 },
}

/// Telemetry sink — writes events to a JSONL file or stdout.
pub struct Telemetry {
    worker_id: WorkerId,
    // TODO(needle-nva): add configurable sink (file, stdout, remote)
}

impl Telemetry {
    pub fn new(worker_id: WorkerId) -> Self {
        Telemetry { worker_id }
    }

    /// Emit a telemetry event.
    pub fn emit(&self, event: EventKind) -> Result<()> {
        let record = TelemetryEvent {
            ts: Utc::now(),
            worker_id: self.worker_id.clone(),
            event,
        };
        // TODO(needle-nva): write to configured sink
        let line = serde_json::to_string(&record)?;
        tracing::debug!(telemetry = %line, "event emitted");
        Ok(())
    }
}
