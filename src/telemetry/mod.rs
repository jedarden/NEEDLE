//! Structured telemetry — JSONL event stream, never on stdout/stderr.
//!
//! Every state transition, claim attempt, dispatch, and outcome emits a typed
//! event. The emitter is non-blocking: events are queued and written by a
//! background task. A broken sink never blocks or panics the worker.
//!
//! ## Architecture
//! ```text
//! worker → emit() → mpsc::Sender → [background task] → TelemetrySink
//! ```
//!
//! Depends on: `types`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::config::{ColorMode, StdoutFormat, StdoutSinkConfig};
use crate::types::{BeadId, WorkerId, WorkerState};

// ─── TelemetryEvent ──────────────────────────────────────────────────────────

/// A single structured telemetry record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    /// UTC timestamp with millisecond precision.
    pub timestamp: DateTime<Utc>,
    /// Discriminant string, e.g. `"state_transition"`.
    pub event_type: String,
    /// The worker that emitted this event.
    pub worker_id: WorkerId,
    /// Session identifier (8 hex chars, unique per worker boot).
    pub session_id: String,
    /// Monotonically increasing counter within the session.
    pub sequence: u64,
    /// Bead context when relevant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bead_id: Option<BeadId>,
    /// Workspace directory when relevant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// Event-specific payload as a JSON value.
    pub data: serde_json::Value,
    /// Optional duration in milliseconds for timed operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

// ─── EventKind ───────────────────────────────────────────────────────────────

/// Typed event variants emitted by all NEEDLE components.
///
/// Every variant maps to a `TelemetryEvent` with `event_type` matching
/// the snake_case discriminant name.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EventKind {
    // ── Worker lifecycle ──
    WorkerStarted {
        worker_name: String,
        version: String,
    },
    WorkerStopped {
        reason: String,
        beads_processed: u64,
        uptime_secs: u64,
    },
    WorkerErrored {
        error_type: String,
        error_message: String,
        beads_processed: u64,
    },
    WorkerExhausted {
        cycle_count: u64,
        last_strand: String,
    },
    WorkerIdle {
        backoff_seconds: u64,
    },
    StateTransition {
        from: WorkerState,
        to: WorkerState,
    },

    // ── Strand evaluation ──
    StrandEvaluated {
        strand_name: String,
        result: String,
        duration_ms: u64,
    },
    StrandSkipped {
        strand_name: String,
        reason: String,
    },
    QueueEmpty,

    // ── Bead claim ──
    ClaimAttempt {
        bead_id: BeadId,
        attempt: u32,
    },
    ClaimSuccess {
        bead_id: BeadId,
    },
    ClaimRaceLost {
        bead_id: BeadId,
    },
    ClaimFailed {
        bead_id: BeadId,
        reason: String,
    },

    // ── Bead lifecycle ──
    BeadReleased {
        bead_id: BeadId,
        reason: String,
    },
    BeadCompleted {
        bead_id: BeadId,
        duration_ms: u64,
    },
    BeadOrphaned {
        bead_id: BeadId,
    },

    // ── Agent dispatch ──
    DispatchStarted {
        bead_id: BeadId,
        agent: String,
        prompt_len: usize,
    },
    DispatchCompleted {
        bead_id: BeadId,
        exit_code: i32,
        duration_ms: u64,
    },

    // ── Outcome ──
    OutcomeClassified {
        bead_id: BeadId,
        outcome: String,
        exit_code: i32,
    },
    OutcomeHandled {
        bead_id: BeadId,
        outcome: String,
        action: String,
    },

    // ── Health ──
    HeartbeatEmitted {
        bead_id: Option<BeadId>,
        state: String,
    },
    StuckDetected {
        bead_id: BeadId,
        age_secs: u64,
    },
    StuckReleased {
        bead_id: BeadId,
        peer_worker: String,
    },
    HealthCheck {
        db_healthy: bool,
        disk_free_mb: u64,
        peer_count: u32,
    },

    // ── Mend strand ──
    MendOrphanedLockRemoved {
        lock_path: String,
        age_secs: u64,
    },
    MendDependencyCleaned {
        bead_id: BeadId,
        blocker_id: BeadId,
    },
    MendDbRepaired {
        warnings: u32,
        fixed: u32,
    },
    MendDbRebuilt,
    MendCycleSummary {
        beads_released: u32,
        locks_removed: u32,
        deps_cleaned: u32,
        db_repaired: bool,
        db_rebuilt: bool,
    },

    // ── Effort tracking ──
    EffortRecorded {
        bead_id: BeadId,
        elapsed_ms: u64,
        agent_name: String,
        model: Option<String>,
        tokens_in: Option<u64>,
        tokens_out: Option<u64>,
        estimated_cost_usd: Option<f64>,
    },
    BudgetWarning {
        daily_cost: f64,
        threshold: f64,
    },
    BudgetStop {
        daily_cost: f64,
        threshold: f64,
    },

    // ── Rate limiting ──
    RateLimitWait {
        provider: String,
        model: Option<String>,
        reason: String,
    },
    RateLimitAllowed {
        provider: String,
        model: Option<String>,
    },

    // ── Mitosis ──
    MitosisEvaluated {
        bead_id: BeadId,
        splittable: bool,
        proposed_children: u32,
    },
    MitosisSplit {
        parent_id: BeadId,
        children_created: u32,
        children_skipped: u32,
        child_ids: Vec<BeadId>,
    },
    MitosisSkipped {
        parent_id: BeadId,
        existing_children: u32,
    },

    // ── Internal ──
    SinkError {
        message: String,
    },
}

impl EventKind {
    /// Return the dotted event type string.
    pub fn event_type(&self) -> &'static str {
        match self {
            EventKind::WorkerStarted { .. } => "worker.started",
            EventKind::WorkerStopped { .. } => "worker.stopped",
            EventKind::WorkerErrored { .. } => "worker.errored",
            EventKind::WorkerExhausted { .. } => "worker.exhausted",
            EventKind::WorkerIdle { .. } => "worker.idle",
            EventKind::StateTransition { .. } => "worker.state_transition",
            EventKind::StrandEvaluated { .. } => "strand.evaluated",
            EventKind::StrandSkipped { .. } => "strand.skipped",
            EventKind::QueueEmpty => "worker.queue_empty",
            EventKind::ClaimAttempt { .. } => "bead.claim.attempted",
            EventKind::ClaimSuccess { .. } => "bead.claim.succeeded",
            EventKind::ClaimRaceLost { .. } => "bead.claim.race_lost",
            EventKind::ClaimFailed { .. } => "bead.claim.failed",
            EventKind::BeadReleased { .. } => "bead.released",
            EventKind::BeadCompleted { .. } => "bead.completed",
            EventKind::BeadOrphaned { .. } => "bead.orphaned",
            EventKind::DispatchStarted { .. } => "agent.dispatched",
            EventKind::DispatchCompleted { .. } => "agent.completed",
            EventKind::OutcomeClassified { .. } => "outcome.classified",
            EventKind::OutcomeHandled { .. } => "outcome.handled",
            EventKind::HeartbeatEmitted { .. } => "heartbeat.emitted",
            EventKind::StuckDetected { .. } => "peer.stale",
            EventKind::StuckReleased { .. } => "peer.crashed",
            EventKind::HealthCheck { .. } => "health.check",
            EventKind::MendOrphanedLockRemoved { .. } => "mend.orphaned_lock_removed",
            EventKind::MendDependencyCleaned { .. } => "mend.dependency_cleaned",
            EventKind::MendDbRepaired { .. } => "mend.db_repaired",
            EventKind::MendDbRebuilt => "mend.db_rebuilt",
            EventKind::MendCycleSummary { .. } => "mend.cycle_summary",
            EventKind::EffortRecorded { .. } => "effort.recorded",
            EventKind::BudgetWarning { .. } => "budget.warning",
            EventKind::BudgetStop { .. } => "budget.stop",
            EventKind::RateLimitWait { .. } => "rate_limit.wait",
            EventKind::RateLimitAllowed { .. } => "rate_limit.allowed",
            EventKind::MitosisEvaluated { .. } => "bead.mitosis.evaluated",
            EventKind::MitosisSplit { .. } => "bead.mitosis.split",
            EventKind::MitosisSkipped { .. } => "bead.mitosis.skipped",
            EventKind::SinkError { .. } => "telemetry.sink_error",
        }
    }

    /// Extract bead_id context from this event (if any).
    pub fn bead_id(&self) -> Option<BeadId> {
        match self {
            EventKind::ClaimAttempt { bead_id, .. }
            | EventKind::ClaimSuccess { bead_id }
            | EventKind::ClaimRaceLost { bead_id }
            | EventKind::ClaimFailed { bead_id, .. }
            | EventKind::BeadReleased { bead_id, .. }
            | EventKind::BeadCompleted { bead_id, .. }
            | EventKind::BeadOrphaned { bead_id }
            | EventKind::DispatchStarted { bead_id, .. }
            | EventKind::DispatchCompleted { bead_id, .. }
            | EventKind::OutcomeClassified { bead_id, .. }
            | EventKind::OutcomeHandled { bead_id, .. }
            | EventKind::StuckDetected { bead_id, .. }
            | EventKind::StuckReleased { bead_id, .. }
            | EventKind::MendDependencyCleaned { bead_id, .. }
            | EventKind::EffortRecorded { bead_id, .. }
            | EventKind::MitosisEvaluated { bead_id, .. } => Some(bead_id.clone()),
            EventKind::MitosisSplit { parent_id, .. }
            | EventKind::MitosisSkipped { parent_id, .. } => Some(parent_id.clone()),
            EventKind::HeartbeatEmitted { bead_id, .. } => bead_id.clone(),
            EventKind::WorkerStarted { .. }
            | EventKind::WorkerStopped { .. }
            | EventKind::WorkerErrored { .. }
            | EventKind::WorkerExhausted { .. }
            | EventKind::WorkerIdle { .. }
            | EventKind::StateTransition { .. }
            | EventKind::StrandEvaluated { .. }
            | EventKind::StrandSkipped { .. }
            | EventKind::QueueEmpty
            | EventKind::HealthCheck { .. }
            | EventKind::MendOrphanedLockRemoved { .. }
            | EventKind::MendDbRepaired { .. }
            | EventKind::MendDbRebuilt
            | EventKind::MendCycleSummary { .. }
            | EventKind::BudgetWarning { .. }
            | EventKind::BudgetStop { .. }
            | EventKind::RateLimitWait { .. }
            | EventKind::RateLimitAllowed { .. }
            | EventKind::SinkError { .. } => None,
        }
    }

    /// Serialize event-specific payload to a JSON value.
    pub fn to_data(&self) -> serde_json::Value {
        match self {
            EventKind::WorkerStarted {
                worker_name,
                version,
            } => {
                serde_json::json!({ "worker_name": worker_name, "version": version })
            }
            EventKind::WorkerStopped {
                reason,
                beads_processed,
                uptime_secs,
            } => {
                serde_json::json!({
                    "reason": reason,
                    "beads_processed": beads_processed,
                    "uptime_secs": uptime_secs,
                })
            }
            EventKind::WorkerErrored {
                error_type,
                error_message,
                beads_processed,
            } => {
                serde_json::json!({
                    "error_type": error_type,
                    "error_message": error_message,
                    "beads_processed": beads_processed,
                })
            }
            EventKind::WorkerExhausted {
                cycle_count,
                last_strand,
            } => {
                serde_json::json!({
                    "cycle_count": cycle_count,
                    "last_strand_evaluated": last_strand,
                })
            }
            EventKind::WorkerIdle { backoff_seconds } => {
                serde_json::json!({ "backoff_seconds": backoff_seconds })
            }
            EventKind::StateTransition { from, to } => {
                serde_json::json!({ "from": format!("{from}"), "to": format!("{to}") })
            }
            EventKind::StrandEvaluated {
                strand_name,
                result,
                duration_ms,
            } => {
                serde_json::json!({
                    "strand_name": strand_name,
                    "result": result,
                    "duration_ms": duration_ms,
                })
            }
            EventKind::StrandSkipped {
                strand_name,
                reason,
            } => {
                serde_json::json!({ "strand_name": strand_name, "reason": reason })
            }
            EventKind::QueueEmpty => serde_json::json!({}),
            EventKind::ClaimAttempt { bead_id, attempt } => {
                serde_json::json!({ "bead_id": bead_id.as_ref(), "attempt": attempt })
            }
            EventKind::ClaimSuccess { bead_id } => {
                serde_json::json!({ "bead_id": bead_id.as_ref() })
            }
            EventKind::ClaimRaceLost { bead_id } => {
                serde_json::json!({ "bead_id": bead_id.as_ref() })
            }
            EventKind::ClaimFailed { bead_id, reason } => {
                serde_json::json!({ "bead_id": bead_id.as_ref(), "reason": reason })
            }
            EventKind::BeadReleased { bead_id, reason } => {
                serde_json::json!({ "bead_id": bead_id.as_ref(), "reason": reason })
            }
            EventKind::BeadCompleted {
                bead_id,
                duration_ms,
            } => {
                serde_json::json!({ "bead_id": bead_id.as_ref(), "duration_ms": duration_ms })
            }
            EventKind::BeadOrphaned { bead_id } => {
                serde_json::json!({ "bead_id": bead_id.as_ref() })
            }
            EventKind::DispatchStarted {
                bead_id,
                agent,
                prompt_len,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "agent": agent,
                    "prompt_len": prompt_len,
                })
            }
            EventKind::DispatchCompleted {
                bead_id,
                exit_code,
                duration_ms,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "exit_code": exit_code,
                    "duration_ms": duration_ms,
                })
            }
            EventKind::OutcomeClassified {
                bead_id,
                outcome,
                exit_code,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "outcome": outcome,
                    "exit_code": exit_code,
                })
            }
            EventKind::OutcomeHandled {
                bead_id,
                outcome,
                action,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "outcome": outcome,
                    "action": action,
                })
            }
            EventKind::HeartbeatEmitted { bead_id, state } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref().map(|b| b.as_ref()),
                    "state": state,
                })
            }
            EventKind::StuckDetected { bead_id, age_secs } => {
                serde_json::json!({ "bead_id": bead_id.as_ref(), "age_secs": age_secs })
            }
            EventKind::StuckReleased {
                bead_id,
                peer_worker,
            } => {
                serde_json::json!({ "bead_id": bead_id.as_ref(), "peer_worker": peer_worker })
            }
            EventKind::HealthCheck {
                db_healthy,
                disk_free_mb,
                peer_count,
            } => {
                serde_json::json!({
                    "db_healthy": db_healthy,
                    "disk_free_mb": disk_free_mb,
                    "peer_count": peer_count,
                })
            }
            EventKind::MendOrphanedLockRemoved {
                lock_path,
                age_secs,
            } => {
                serde_json::json!({ "lock_path": lock_path, "age_secs": age_secs })
            }
            EventKind::MendDependencyCleaned {
                bead_id,
                blocker_id,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "blocker_id": blocker_id.as_ref(),
                })
            }
            EventKind::MendDbRepaired { warnings, fixed } => {
                serde_json::json!({ "warnings": warnings, "fixed": fixed })
            }
            EventKind::MendDbRebuilt => serde_json::json!({}),
            EventKind::MendCycleSummary {
                beads_released,
                locks_removed,
                deps_cleaned,
                db_repaired,
                db_rebuilt,
            } => {
                serde_json::json!({
                    "beads_released": beads_released,
                    "locks_removed": locks_removed,
                    "deps_cleaned": deps_cleaned,
                    "db_repaired": db_repaired,
                    "db_rebuilt": db_rebuilt,
                })
            }
            EventKind::EffortRecorded {
                bead_id,
                elapsed_ms,
                agent_name,
                model,
                tokens_in,
                tokens_out,
                estimated_cost_usd,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "elapsed_ms": elapsed_ms,
                    "agent_name": agent_name,
                    "model": model,
                    "tokens_in": tokens_in,
                    "tokens_out": tokens_out,
                    "estimated_cost_usd": estimated_cost_usd,
                })
            }
            EventKind::BudgetWarning {
                daily_cost,
                threshold,
            } => {
                serde_json::json!({
                    "daily_cost": daily_cost,
                    "threshold": threshold,
                })
            }
            EventKind::BudgetStop {
                daily_cost,
                threshold,
            } => {
                serde_json::json!({
                    "daily_cost": daily_cost,
                    "threshold": threshold,
                })
            }
            EventKind::RateLimitWait {
                provider,
                model,
                reason,
            } => {
                serde_json::json!({
                    "provider": provider,
                    "model": model,
                    "reason": reason,
                })
            }
            EventKind::RateLimitAllowed { provider, model } => {
                serde_json::json!({
                    "provider": provider,
                    "model": model,
                })
            }
            EventKind::MitosisEvaluated {
                bead_id,
                splittable,
                proposed_children,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "splittable": splittable,
                    "proposed_children": proposed_children,
                })
            }
            EventKind::MitosisSplit {
                parent_id,
                children_created,
                children_skipped,
                child_ids,
            } => {
                let ids: Vec<&str> = child_ids.iter().map(|id| id.as_ref()).collect();
                serde_json::json!({
                    "parent_id": parent_id.as_ref(),
                    "children_created": children_created,
                    "children_skipped": children_skipped,
                    "child_ids": ids,
                })
            }
            EventKind::MitosisSkipped {
                parent_id,
                existing_children,
            } => {
                serde_json::json!({
                    "parent_id": parent_id.as_ref(),
                    "existing_children": existing_children,
                })
            }
            EventKind::SinkError { message } => serde_json::json!({ "message": message }),
        }
    }

    /// Extract duration_ms from events that carry it.
    pub fn duration_ms(&self) -> Option<u64> {
        match self {
            EventKind::DispatchCompleted { duration_ms, .. }
            | EventKind::BeadCompleted { duration_ms, .. }
            | EventKind::StrandEvaluated { duration_ms, .. }
            | EventKind::EffortRecorded {
                elapsed_ms: duration_ms,
                ..
            } => Some(*duration_ms),
            EventKind::WorkerStarted { .. }
            | EventKind::WorkerStopped { .. }
            | EventKind::WorkerErrored { .. }
            | EventKind::WorkerExhausted { .. }
            | EventKind::WorkerIdle { .. }
            | EventKind::StateTransition { .. }
            | EventKind::StrandSkipped { .. }
            | EventKind::QueueEmpty
            | EventKind::ClaimAttempt { .. }
            | EventKind::ClaimSuccess { .. }
            | EventKind::ClaimRaceLost { .. }
            | EventKind::ClaimFailed { .. }
            | EventKind::BeadReleased { .. }
            | EventKind::BeadOrphaned { .. }
            | EventKind::DispatchStarted { .. }
            | EventKind::OutcomeClassified { .. }
            | EventKind::OutcomeHandled { .. }
            | EventKind::HeartbeatEmitted { .. }
            | EventKind::StuckDetected { .. }
            | EventKind::StuckReleased { .. }
            | EventKind::HealthCheck { .. }
            | EventKind::MendOrphanedLockRemoved { .. }
            | EventKind::MendDependencyCleaned { .. }
            | EventKind::MendDbRepaired { .. }
            | EventKind::MendDbRebuilt
            | EventKind::MendCycleSummary { .. }
            | EventKind::BudgetWarning { .. }
            | EventKind::BudgetStop { .. }
            | EventKind::RateLimitWait { .. }
            | EventKind::RateLimitAllowed { .. }
            | EventKind::MitosisEvaluated { .. }
            | EventKind::MitosisSplit { .. }
            | EventKind::MitosisSkipped { .. }
            | EventKind::SinkError { .. } => None,
        }
    }
}

// ─── TelemetrySink trait ─────────────────────────────────────────────────────

/// Pluggable output backend for telemetry events.
///
/// Phase 2 adds stdout sink and hook sink. All backends implement this trait.
pub trait TelemetrySink: Send + Sync {
    /// Write a single event. Must not block indefinitely.
    fn write(&self, event: &TelemetryEvent) -> Result<()>;

    /// Flush any buffered events. Called on shutdown.
    fn flush(&self) -> Result<()>;
}

// ─── FileSink ────────────────────────────────────────────────────────────────

/// Writes JSONL telemetry to `<log_dir>/<worker>-<session>.jsonl`.
///
/// Append-only, one line per event. The log directory is created if it
/// does not exist.
pub struct FileSink {
    path: PathBuf,
    writer: std::sync::Mutex<std::io::BufWriter<std::fs::File>>,
}

impl FileSink {
    /// Construct a sink using the default log directory (`~/.needle/logs/`).
    pub fn new(worker_id: &str, session_id: &str) -> Result<Self> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let log_dir = PathBuf::from(&home).join(".needle").join("logs");
        Self::with_dir(&log_dir, worker_id, session_id)
    }

    /// Construct a sink writing to a specific directory.
    ///
    /// Creates the directory (and parents) if it does not exist.
    pub fn with_dir(log_dir: &Path, worker_id: &str, session_id: &str) -> Result<Self> {
        std::fs::create_dir_all(log_dir)
            .with_context(|| format!("failed to create log directory: {}", log_dir.display()))?;
        let filename = format!("{worker_id}-{session_id}.jsonl");
        let path = log_dir.join(filename);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open log file: {}", path.display()))?;
        Ok(FileSink {
            path,
            writer: std::sync::Mutex::new(std::io::BufWriter::new(file)),
        })
    }

    /// Return the path to the log file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl TelemetrySink for FileSink {
    fn write(&self, event: &TelemetryEvent) -> Result<()> {
        use std::io::Write;
        let line = serde_json::to_string(event)?;
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {}", e))?;
        writeln!(writer, "{line}")?;
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        use std::io::Write;
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {}", e))?;
        writer.flush()?;
        Ok(())
    }
}

// ─── StdoutSink ───────────────────────────────────────────────────────────────

/// Human-readable, color-coded telemetry sink for interactive monitoring.
///
/// Format: `HH:MM:SS [worker] EVENT detail`
pub struct StdoutSink {
    format: StdoutFormat,
    use_color: bool,
}

impl StdoutSink {
    /// Create a new stdout sink from config.
    pub fn new(config: &StdoutSinkConfig) -> Self {
        let use_color = match config.color {
            ColorMode::Always => true,
            ColorMode::Never => false,
            ColorMode::Auto => atty_stdout(),
        };
        StdoutSink {
            format: config.format,
            use_color,
        }
    }

    /// Format a single event as a human-readable line.
    pub fn format_event(&self, event: &TelemetryEvent) -> String {
        let time = event.timestamp.format("%H:%M:%S");
        let worker = &event.worker_id;
        let etype = &event.event_type;

        // Build the summary portion based on event type.
        let summary = self.event_summary(event);

        match self.format {
            StdoutFormat::Minimal => {
                let line = format!("{time} [{worker}] {etype}");
                if self.use_color {
                    self.colorize(etype, &line)
                } else {
                    line
                }
            }
            StdoutFormat::Normal => {
                let bead_part = event
                    .bead_id
                    .as_ref()
                    .map(|b| format!(" {}", b.as_ref()))
                    .unwrap_or_default();
                let summary_part = if summary.is_empty() {
                    String::new()
                } else {
                    format!(" ({summary})")
                };
                let line = format!(
                    "{time} [{worker}] {}{bead_part}{summary_part}",
                    Self::short_type(etype)
                );
                if self.use_color {
                    self.colorize(etype, &line)
                } else {
                    line
                }
            }
            StdoutFormat::Verbose => {
                let bead_part = event
                    .bead_id
                    .as_ref()
                    .map(|b| format!(" {}", b.as_ref()))
                    .unwrap_or_default();
                let data_part = if event.data.is_object()
                    && event.data.as_object().map_or(true, |m| m.is_empty())
                {
                    String::new()
                } else {
                    format!(" {}", event.data)
                };
                let dur = event
                    .duration_ms
                    .map(|d| format!(" {}ms", d))
                    .unwrap_or_default();
                let line = format!(
                    "{time} [{worker}] {}{bead_part}{dur}{data_part}",
                    Self::short_type(etype)
                );
                if self.use_color {
                    self.colorize(etype, &line)
                } else {
                    line
                }
            }
        }
    }

    /// Shorten the dotted event type to an uppercase action word.
    fn short_type(event_type: &str) -> &str {
        match event_type {
            "worker.started" => "STARTED",
            "worker.stopped" => "STOPPED",
            "worker.errored" => "ERROR",
            "worker.exhausted" => "EXHAUSTED",
            "worker.idle" => "IDLE",
            "worker.state_transition" => "STATE",
            "worker.queue_empty" => "QUEUE_EMPTY",
            "strand.evaluated" => "STRAND",
            "strand.skipped" => "STRAND_SKIP",
            "bead.claim.attempted" => "CLAIMING",
            "bead.claim.succeeded" => "CLAIMED",
            "bead.claim.race_lost" => "RACE_LOST",
            "bead.claim.failed" => "CLAIM_FAIL",
            "bead.released" => "RELEASED",
            "bead.completed" => "COMPLETED",
            "bead.orphaned" => "ORPHANED",
            "agent.dispatched" => "DISPATCHED",
            "agent.completed" => "AGENT_DONE",
            "outcome.classified" => "OUTCOME",
            "outcome.handled" => "HANDLED",
            "heartbeat.emitted" => "HEARTBEAT",
            "peer.stale" => "PEER_STALE",
            "peer.crashed" => "PEER_CRASHED",
            "health.check" => "HEALTH",
            "effort.recorded" => "EFFORT",
            "budget.warning" => "BUDGET_WARN",
            "budget.stop" => "BUDGET_STOP",
            "rate_limit.wait" => "RATE_WAIT",
            "rate_limit.allowed" => "RATE_OK",
            "bead.mitosis.evaluated" => "MITOSIS_EVAL",
            "bead.mitosis.split" => "MITOSIS",
            "bead.mitosis.skipped" => "MITOSIS_SKIP",
            "mend.orphaned_lock_removed" => "MEND_LOCK",
            "mend.dependency_cleaned" => "MEND_DEP",
            "mend.db_repaired" => "MEND_REPAIR",
            "mend.db_rebuilt" => "MEND_REBUILD",
            "mend.cycle_summary" => "MEND_DONE",
            "telemetry.sink_error" => "SINK_ERR",
            other => other,
        }
    }

    /// Produce a brief summary from the event data.
    fn event_summary(&self, event: &TelemetryEvent) -> String {
        let d = &event.data;
        match event.event_type.as_str() {
            "bead.claim.succeeded" => String::new(),
            "agent.dispatched" => d["agent"]
                .as_str()
                .map(|a| format!("agent={a}"))
                .unwrap_or_default(),
            "agent.completed" => {
                let exit = d["exit_code"].as_i64().unwrap_or(-1);
                let dur = d["duration_ms"].as_u64().unwrap_or(0);
                format!("exit={exit}, {}", format_duration_ms(dur))
            }
            "outcome.classified" => d["outcome"].as_str().unwrap_or("unknown").to_string(),
            "outcome.handled" => {
                let outcome = d["outcome"].as_str().unwrap_or("?");
                let action = d["action"].as_str().unwrap_or("?");
                format!("{outcome} → {action}")
            }
            "effort.recorded" => {
                let agent = d["agent_name"].as_str().unwrap_or("?");
                let cost = d["estimated_cost_usd"].as_f64();
                let dur = d["elapsed_ms"].as_u64().unwrap_or(0);
                match cost {
                    Some(c) => format!("{agent}, {}, ${c:.4}", format_duration_ms(dur)),
                    None => format!("{agent}, {}", format_duration_ms(dur)),
                }
            }
            "worker.stopped" => {
                let reason = d["reason"].as_str().unwrap_or("?");
                let beads = d["beads_processed"].as_u64().unwrap_or(0);
                format!("{reason}, {beads} beads")
            }
            "worker.errored" => d["error_message"].as_str().unwrap_or("unknown").to_string(),
            "worker.idle" => {
                let secs = d["backoff_seconds"].as_u64().unwrap_or(0);
                format!("backoff {secs}s")
            }
            "bead.mitosis.split" => {
                let created = d["children_created"].as_u64().unwrap_or(0);
                format!("{created} children")
            }
            "budget.warning" | "budget.stop" => {
                let cost = d["daily_cost"].as_f64().unwrap_or(0.0);
                let thresh = d["threshold"].as_f64().unwrap_or(0.0);
                format!("${cost:.2}/${thresh:.2}")
            }
            _ => String::new(),
        }
    }

    /// Wrap text in ANSI color based on event type category.
    fn colorize(&self, event_type: &str, text: &str) -> String {
        let code = match event_type {
            t if t.starts_with("worker.errored") || t.starts_with("budget.stop") => "\x1b[31m", // red
            t if t.starts_with("bead.claim.succeeded") || t == "bead.completed" => "\x1b[32m", // green
            t if t.starts_with("agent.") || t.starts_with("outcome.") => "\x1b[36m", // cyan
            t if t.starts_with("bead.claim.race_lost")
                || t.starts_with("bead.claim.failed")
                || t == "bead.released"
                || t == "bead.orphaned" =>
            {
                "\x1b[33m"
            } // yellow
            t if t.starts_with("budget.warning") || t.starts_with("rate_limit") => "\x1b[33m", // yellow
            t if t.starts_with("mend.") || t.starts_with("peer.") => "\x1b[35m", // magenta
            t if t.starts_with("heartbeat") || t.starts_with("health") => "\x1b[90m", // dim
            _ => "\x1b[0m",                                                      // reset / default
        };
        format!("{code}{text}\x1b[0m")
    }
}

impl TelemetrySink for StdoutSink {
    fn write(&self, event: &TelemetryEvent) -> Result<()> {
        let line = self.format_event(event);
        println!("{line}");
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        use std::io::Write;
        std::io::stdout().flush()?;
        Ok(())
    }
}

/// Check if stdout is a terminal (for auto color detection).
fn atty_stdout() -> bool {
    unsafe { libc_isatty(1) != 0 }
}

extern "C" {
    #[link_name = "isatty"]
    fn libc_isatty(fd: i32) -> i32;
}

/// Public wrapper for formatting milliseconds as human-readable duration.
pub fn format_duration_ms_public(ms: u64) -> String {
    format_duration_ms(ms)
}

/// Format milliseconds as human-readable duration.
fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) / 1000;
        format!("{mins}m{secs}s")
    }
}

// ─── Session ID generation ───────────────────────────────────────────────────

/// Generate an 8-hex-char session ID.
///
/// Uses `/dev/urandom` when available, falls back to PID XOR timestamp.
pub fn generate_session_id() -> String {
    // Try /dev/urandom (Linux/macOS)
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let mut buf = [0u8; 4];
        if f.read_exact(&mut buf).is_ok() {
            return format!("{:02x}{:02x}{:02x}{:02x}", buf[0], buf[1], buf[2], buf[3]);
        }
    }
    // Fallback: PID XOR nanosecond timestamp
    let pid = std::process::id();
    let ts = Utc::now().timestamp_millis() as u64;
    let hash = pid as u64 ^ ts;
    format!("{:08x}", hash & 0xffff_ffff)
}

// ─── Telemetry emitter ───────────────────────────────────────────────────────

/// Non-blocking telemetry emitter.
///
/// Cloning a `Telemetry` handle is cheap — it shares the same background
/// writer and sequence counter.
#[derive(Clone)]
pub struct Telemetry {
    worker_id: WorkerId,
    session_id: String,
    sequence: Arc<AtomicU64>,
    sender: mpsc::UnboundedSender<TelemetryEvent>,
}

impl Telemetry {
    /// Create a telemetry emitter that writes to a `FileSink`.
    ///
    /// Spawns a background tokio task that drains events to the sink.
    pub fn new(worker_id: WorkerId) -> Self {
        let session_id = generate_session_id();
        let (sender, receiver) = mpsc::unbounded_channel();

        // Try to create a file sink; fall back to no-op on error.
        let sink: Option<FileSink> = FileSink::new(&worker_id, &session_id)
            .map_err(|e| {
                tracing::warn!(error = %e, "failed to create telemetry file sink");
            })
            .ok();

        let sequence = Arc::new(AtomicU64::new(0));
        Self::spawn_writer(receiver, sink, None);

        Telemetry {
            worker_id,
            session_id,
            sequence,
            sender,
        }
    }

    /// Create a telemetry emitter with both file and stdout sinks.
    pub fn with_stdout(worker_id: WorkerId, stdout_config: &StdoutSinkConfig) -> Self {
        let session_id = generate_session_id();
        let (sender, receiver) = mpsc::unbounded_channel();

        let file_sink: Option<FileSink> = FileSink::new(&worker_id, &session_id)
            .map_err(|e| {
                tracing::warn!(error = %e, "failed to create telemetry file sink");
            })
            .ok();

        let stdout_sink = if stdout_config.enabled {
            Some(StdoutSink::new(stdout_config))
        } else {
            None
        };

        let sequence = Arc::new(AtomicU64::new(0));
        Self::spawn_writer(receiver, file_sink, stdout_sink);

        Telemetry {
            worker_id,
            session_id,
            sequence,
            sender,
        }
    }

    /// Create a telemetry emitter with a custom sink (for testing).
    #[cfg(test)]
    pub fn with_sink(worker_id: WorkerId, sink: impl TelemetrySink + 'static) -> Self {
        let session_id = "test0000".to_string();
        let (sender, receiver) = mpsc::unbounded_channel::<TelemetryEvent>();
        let sequence = Arc::new(AtomicU64::new(0));

        tokio::spawn(async move {
            let mut rx = receiver;
            while let Some(event) = rx.recv().await {
                if let Err(e) = sink.write(&event) {
                    tracing::warn!(error = %e, "test sink write failed");
                }
            }
        });

        Telemetry {
            worker_id,
            session_id,
            sequence,
            sender,
        }
    }

    /// Emit an event. Non-blocking — returns immediately.
    ///
    /// Returns `Err` only if the channel is disconnected (background task died).
    pub fn emit(&self, kind: EventKind) -> Result<()> {
        let seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: kind.event_type().to_string(),
            worker_id: self.worker_id.clone(),
            session_id: self.session_id.clone(),
            sequence: seq,
            bead_id: kind.bead_id(),
            workspace: None,
            duration_ms: kind.duration_ms(),
            data: kind.to_data(),
        };
        tracing::debug!(event_type = %event.event_type, seq, "telemetry event");
        self.sender.send(event).ok(); // ok() — never block, never panic
        Ok(())
    }

    /// Return a reference to the session ID for log path discovery.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Return a reference to the worker ID.
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// Create a telemetry emitter writing to a specific log directory.
    ///
    /// Use this when the config specifies a custom log path.
    pub fn with_log_dir(worker_id: WorkerId, log_dir: &Path) -> Self {
        let session_id = generate_session_id();
        let (sender, receiver) = mpsc::unbounded_channel();

        let sink: Option<FileSink> = FileSink::with_dir(log_dir, &worker_id, &session_id)
            .map_err(|e| {
                tracing::warn!(error = %e, "failed to create telemetry file sink");
            })
            .ok();

        let sequence = Arc::new(AtomicU64::new(0));
        Self::spawn_writer(receiver, sink, None);

        Telemetry {
            worker_id,
            session_id,
            sequence,
            sender,
        }
    }

    /// Spawn background writer task draining the channel to the sinks.
    fn spawn_writer(
        mut receiver: mpsc::UnboundedReceiver<TelemetryEvent>,
        file_sink: Option<FileSink>,
        stdout_sink: Option<StdoutSink>,
    ) {
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                if let Some(ref s) = file_sink {
                    if let Err(e) = s.write(&event) {
                        tracing::warn!(error = %e, "telemetry file sink write failed");
                    }
                }
                if let Some(ref s) = stdout_sink {
                    if let Err(e) = s.write(&event) {
                        tracing::warn!(error = %e, "telemetry stdout sink write failed");
                    }
                }
            }
            if let Some(ref s) = file_sink {
                let _ = s.flush();
            }
            if let Some(ref s) = stdout_sink {
                let _ = s.flush();
            }
        });
    }
}

// ─── Log querying ────────────────────────────────────────────────────────────

/// Parse a `--since` value into a `DateTime<Utc>`.
///
/// Accepts relative durations like `1h`, `30m`, `24h`, `7d` or absolute
/// ISO 8601 / date strings like `2026-03-20` or `2026-03-20T15:00:00Z`.
pub fn parse_since(input: &str) -> Result<DateTime<Utc>> {
    let trimmed = input.trim();

    // Try relative duration: Nh, Nm, Nd, Ns
    if let Some(rest) = trimmed.strip_suffix('h') {
        let hours: i64 = rest.parse().context("invalid hours in --since")?;
        return Ok(Utc::now() - chrono::Duration::hours(hours));
    }
    if let Some(rest) = trimmed.strip_suffix('m') {
        let mins: i64 = rest.parse().context("invalid minutes in --since")?;
        return Ok(Utc::now() - chrono::Duration::minutes(mins));
    }
    if let Some(rest) = trimmed.strip_suffix('d') {
        let days: i64 = rest.parse().context("invalid days in --since")?;
        return Ok(Utc::now() - chrono::Duration::days(days));
    }
    if let Some(rest) = trimmed.strip_suffix('s') {
        if rest.chars().all(|c| c.is_ascii_digit()) {
            let secs: i64 = rest.parse().context("invalid seconds in --since")?;
            return Ok(Utc::now() - chrono::Duration::seconds(secs));
        }
    }

    // Try ISO 8601 with time
    if let Ok(dt) = trimmed.parse::<DateTime<Utc>>() {
        return Ok(dt);
    }

    // Try date-only (YYYY-MM-DD) → midnight UTC
    if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let dt = date.and_hms_opt(0, 0, 0).context("invalid date")?;
        return Ok(DateTime::from_naive_utc_and_offset(dt, Utc));
    }

    anyhow::bail!(
        "unrecognized --since format: '{}'. Use relative (1h, 30m, 7d) or ISO date (2026-03-20)",
        input
    )
}

/// Convert a glob-style filter pattern to a regex.
///
/// Supports `*` as wildcard. E.g., `bead.claim.*` → `^bead\.claim\..*$`
pub fn glob_to_regex(pattern: &str) -> Result<regex::Regex> {
    let mut re = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '.' => re.push_str("\\."),
            '?' => re.push('.'),
            c => re.push(c),
        }
    }
    re.push('$');
    regex::Regex::new(&re).context("invalid filter pattern")
}

/// Read and parse JSONL log files from a directory.
///
/// Returns events sorted by timestamp. Optionally filters by `since` and
/// event type `filter` (glob pattern).
pub fn read_logs(
    log_dir: &Path,
    since: Option<DateTime<Utc>>,
    filter: Option<&regex::Regex>,
) -> Result<Vec<TelemetryEvent>> {
    let mut events = Vec::new();

    if !log_dir.is_dir() {
        return Ok(events);
    }

    let entries = std::fs::read_dir(log_dir)
        .with_context(|| format!("cannot read log directory: {}", log_dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read log file: {}", path.display()))?;
        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let event: TelemetryEvent = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(_) => continue, // Skip malformed lines
            };
            if let Some(ref cutoff) = since {
                if event.timestamp < *cutoff {
                    continue;
                }
            }
            if let Some(re) = filter {
                if !re.is_match(&event.event_type) {
                    continue;
                }
            }
            events.push(event);
        }
    }

    events.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(events)
}

/// Compute cost summary from effort events.
pub fn compute_cost_summary(events: &[TelemetryEvent]) -> CostSummary {
    let mut summary = CostSummary::default();
    for event in events {
        if event.event_type == "effort.recorded" {
            summary.total_events += 1;
            if let Some(cost) = event.data["estimated_cost_usd"].as_f64() {
                summary.total_cost_usd += cost;
            }
            if let Some(tokens_in) = event.data["tokens_in"].as_u64() {
                summary.total_tokens_in += tokens_in;
            }
            if let Some(tokens_out) = event.data["tokens_out"].as_u64() {
                summary.total_tokens_out += tokens_out;
            }
            if let Some(elapsed) = event.data["elapsed_ms"].as_u64() {
                summary.total_elapsed_ms += elapsed;
            }
        }
    }
    summary
}

/// Aggregated cost data from effort events.
#[derive(Debug, Default)]
pub struct CostSummary {
    pub total_events: u64,
    pub total_cost_usd: f64,
    pub total_tokens_in: u64,
    pub total_tokens_out: u64,
    pub total_elapsed_ms: u64,
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory sink for testing — collects events via a shared Vec.
    struct MemorySink {
        events: Arc<std::sync::Mutex<Vec<TelemetryEvent>>>,
    }

    impl MemorySink {
        fn new() -> (Self, Arc<std::sync::Mutex<Vec<TelemetryEvent>>>) {
            let events = Arc::new(std::sync::Mutex::new(Vec::new()));
            (
                MemorySink {
                    events: events.clone(),
                },
                events,
            )
        }
    }

    impl TelemetrySink for MemorySink {
        fn write(&self, event: &TelemetryEvent) -> Result<()> {
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
        fn flush(&self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn event_kind_types_are_dotted_strings() {
        assert_eq!(EventKind::QueueEmpty.event_type(), "worker.queue_empty");
        assert_eq!(
            EventKind::StateTransition {
                from: WorkerState::Booting,
                to: WorkerState::Selecting,
            }
            .event_type(),
            "worker.state_transition"
        );
        assert_eq!(
            EventKind::ClaimAttempt {
                bead_id: BeadId::from("needle-abc"),
                attempt: 1,
            }
            .event_type(),
            "bead.claim.attempted"
        );
        assert_eq!(
            EventKind::DispatchStarted {
                bead_id: BeadId::from("nd-x"),
                agent: "claude".to_string(),
                prompt_len: 100,
            }
            .event_type(),
            "agent.dispatched"
        );
        assert_eq!(
            EventKind::OutcomeHandled {
                bead_id: BeadId::from("nd-x"),
                outcome: "success".to_string(),
                action: "none".to_string(),
            }
            .event_type(),
            "outcome.handled"
        );
    }

    #[test]
    fn event_kind_bead_id_extracted_correctly() {
        let id = BeadId::from("needle-xyz");
        let kind = EventKind::ClaimSuccess {
            bead_id: id.clone(),
        };
        assert_eq!(kind.bead_id(), Some(id));

        let kind = EventKind::QueueEmpty;
        assert_eq!(kind.bead_id(), None);

        let kind = EventKind::BeadReleased {
            bead_id: BeadId::from("nd-r"),
            reason: "failure".to_string(),
        };
        assert!(kind.bead_id().is_some());

        let kind = EventKind::HeartbeatEmitted {
            bead_id: None,
            state: "SELECTING".to_string(),
        };
        assert!(kind.bead_id().is_none());
    }

    #[test]
    fn event_kind_to_data_is_valid_json() {
        let kind = EventKind::WorkerStarted {
            worker_name: "needle-alpha".to_string(),
            version: "0.1.0".to_string(),
        };
        let data = kind.to_data();
        assert_eq!(data["worker_name"], "needle-alpha");
        assert_eq!(data["version"], "0.1.0");
    }

    #[test]
    fn event_kind_duration_ms_extracted() {
        let kind = EventKind::DispatchCompleted {
            bead_id: BeadId::from("nd-x"),
            exit_code: 0,
            duration_ms: 1234,
        };
        assert_eq!(kind.duration_ms(), Some(1234));

        let kind = EventKind::BeadCompleted {
            bead_id: BeadId::from("nd-x"),
            duration_ms: 5000,
        };
        assert_eq!(kind.duration_ms(), Some(5000));

        let kind = EventKind::QueueEmpty;
        assert_eq!(kind.duration_ms(), None);
    }

    #[test]
    fn telemetry_event_serializes_to_valid_json() {
        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "test_event".to_string(),
            worker_id: "needle-test".to_string(),
            session_id: "abcd1234".to_string(),
            sequence: 0,
            bead_id: Some(BeadId::from("needle-abc")),
            workspace: None,
            data: serde_json::json!({ "key": "value" }),
            duration_ms: Some(42),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse back");
        assert_eq!(parsed["event_type"], "test_event");
        assert_eq!(parsed["sequence"], 0);
        assert_eq!(parsed["duration_ms"], 42);
    }

    #[test]
    fn telemetry_event_json_roundtrip() {
        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "bead.claim.attempted".to_string(),
            worker_id: "needle-01".to_string(),
            session_id: "deadbeef".to_string(),
            sequence: 42,
            bead_id: Some(BeadId::from("nd-abc")),
            workspace: Some(PathBuf::from("/home/coder/project")),
            data: serde_json::json!({ "retry_number": 2 }),
            duration_ms: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: TelemetryEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.event_type, event.event_type);
        assert_eq!(parsed.worker_id, event.worker_id);
        assert_eq!(parsed.session_id, event.session_id);
        assert_eq!(parsed.sequence, event.sequence);
        assert_eq!(parsed.bead_id, event.bead_id);
        assert_eq!(parsed.data["retry_number"], 2);
    }

    #[test]
    fn telemetry_event_optional_fields_omitted_when_none() {
        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "worker.started".to_string(),
            worker_id: "needle-01".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({}),
            duration_ms: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            !json.contains("bead_id"),
            "bead_id should be omitted: {}",
            json
        );
        assert!(
            !json.contains("duration_ms"),
            "duration_ms should be omitted: {}",
            json
        );
    }

    #[test]
    fn session_id_is_8_hex_chars() {
        let id = generate_session_id();
        assert_eq!(id.len(), 8, "session ID should be 8 chars: {}", id);
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "session ID should be hex: {}",
            id
        );
    }

    #[test]
    fn session_ids_are_unique() {
        let ids: Vec<String> = (0..10).map(|_| generate_session_id()).collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert!(unique.len() > 1, "session IDs should vary: {:?}", ids);
    }

    #[test]
    fn sequence_numbers_are_monotonic() {
        let seq = Arc::new(AtomicU64::new(0));
        let a = seq.fetch_add(1, Ordering::Relaxed);
        let b = seq.fetch_add(1, Ordering::Relaxed);
        let c = seq.fetch_add(1, Ordering::Relaxed);
        assert!(a < b);
        assert!(b < c);
    }

    #[tokio::test]
    async fn emit_does_not_block() {
        let telemetry = Telemetry::new("needle-test".to_string());
        for i in 0..100u32 {
            telemetry
                .emit(EventKind::ClaimAttempt {
                    bead_id: BeadId::from("needle-abc"),
                    attempt: i,
                })
                .expect("emit should not fail");
        }
    }

    #[tokio::test]
    async fn emitter_delivers_events_to_sink() {
        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-worker".to_string(), sink);

        telemetry
            .emit(EventKind::WorkerStarted {
                worker_name: "test-worker".to_string(),
                version: "0.1.0".to_string(),
            })
            .unwrap();
        telemetry
            .emit(EventKind::ClaimAttempt {
                bead_id: BeadId::from("nd-test"),
                attempt: 1,
            })
            .unwrap();

        // Drop to close channel and drain
        drop(telemetry);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let collected = events.lock().unwrap();
        assert_eq!(
            collected.len(),
            2,
            "expected 2 events, got {}",
            collected.len()
        );
        assert_eq!(collected[0].event_type, "worker.started");
        assert_eq!(collected[0].sequence, 0);
        assert_eq!(collected[1].event_type, "bead.claim.attempted");
        assert_eq!(collected[1].sequence, 1);
        assert_eq!(collected[1].bead_id, Some(BeadId::from("nd-test")));
    }

    #[tokio::test]
    async fn sequence_numbers_monotonic_in_emitter() {
        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-seq".to_string(), sink);

        for _ in 0..10 {
            telemetry.emit(EventKind::QueueEmpty).unwrap();
        }

        drop(telemetry);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 10);
        for (i, event) in collected.iter().enumerate() {
            assert_eq!(event.sequence, i as u64, "sequence mismatch at index {}", i);
        }
    }

    #[tokio::test]
    async fn broken_sink_does_not_crash_emitter() {
        struct BrokenSink;
        impl TelemetrySink for BrokenSink {
            fn write(&self, _: &TelemetryEvent) -> Result<()> {
                anyhow::bail!("sink is broken")
            }
            fn flush(&self) -> Result<()> {
                Ok(())
            }
        }

        let telemetry = Telemetry::with_sink("test-broken".to_string(), BrokenSink);
        telemetry
            .emit(EventKind::WorkerStarted {
                worker_name: "test".to_string(),
                version: "0.1.0".to_string(),
            })
            .unwrap();
        telemetry.emit(EventKind::QueueEmpty).unwrap();
        // No panic, no block
        drop(telemetry);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn file_sink_writes_jsonl() {
        let dir = std::env::temp_dir().join("needle-test-telem-file");
        let _ = std::fs::remove_dir_all(&dir);

        let sink =
            FileSink::with_dir(&dir, "test-worker", "deadbeef").expect("should create file sink");

        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "test.file".to_string(),
            worker_id: "test-worker".to_string(),
            session_id: "deadbeef".to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({"hello": "world"}),
            duration_ms: None,
        };

        sink.write(&event).expect("write should succeed");
        sink.flush().expect("flush should succeed");

        let path = dir.join("test-worker-deadbeef.jsonl");
        let contents = std::fs::read_to_string(&path).expect("should read file");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1, "expected 1 line");

        let parsed: TelemetryEvent =
            serde_json::from_str(lines[0]).expect("line should be valid JSON");
        assert_eq!(parsed.event_type, "test.file");
        assert_eq!(parsed.data["hello"], "world");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn file_sink_creates_nested_directory() {
        let dir = std::env::temp_dir()
            .join("needle-test-mkdir")
            .join("nested");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());

        let sink = FileSink::with_dir(&dir, "worker", "abcd1234");
        assert!(sink.is_ok(), "should create nested directory");

        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[tokio::test]
    async fn timestamps_are_utc_iso8601() {
        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-utc".to_string(), sink);

        telemetry.emit(EventKind::QueueEmpty).unwrap();
        drop(telemetry);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let collected = events.lock().unwrap();
        let json = serde_json::to_string(&collected[0]).unwrap();
        // ISO 8601 UTC timestamps contain Z or +00:00
        assert!(
            json.contains('Z') || json.contains("+00:00"),
            "timestamp should be UTC: {}",
            json
        );
    }

    #[tokio::test]
    async fn all_event_kinds_produce_valid_events() {
        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-all".to_string(), sink);
        let id = BeadId::from("nd-test");

        let kinds = vec![
            EventKind::WorkerStarted {
                worker_name: "w".to_string(),
                version: "0.1".to_string(),
            },
            EventKind::WorkerStopped {
                reason: "done".to_string(),
                beads_processed: 5,
                uptime_secs: 60,
            },
            EventKind::WorkerErrored {
                error_type: "config".to_string(),
                error_message: "bad".to_string(),
                beads_processed: 0,
            },
            EventKind::WorkerExhausted {
                cycle_count: 3,
                last_strand: "pluck".to_string(),
            },
            EventKind::WorkerIdle {
                backoff_seconds: 30,
            },
            EventKind::StateTransition {
                from: WorkerState::Booting,
                to: WorkerState::Selecting,
            },
            EventKind::StrandEvaluated {
                strand_name: "pluck".to_string(),
                result: "bead_found".to_string(),
                duration_ms: 50,
            },
            EventKind::StrandSkipped {
                strand_name: "mend".to_string(),
                reason: "disabled".to_string(),
            },
            EventKind::QueueEmpty,
            EventKind::ClaimAttempt {
                bead_id: id.clone(),
                attempt: 1,
            },
            EventKind::ClaimSuccess {
                bead_id: id.clone(),
            },
            EventKind::ClaimRaceLost {
                bead_id: id.clone(),
            },
            EventKind::ClaimFailed {
                bead_id: id.clone(),
                reason: "not open".to_string(),
            },
            EventKind::BeadReleased {
                bead_id: id.clone(),
                reason: "failure".to_string(),
            },
            EventKind::BeadCompleted {
                bead_id: id.clone(),
                duration_ms: 5000,
            },
            EventKind::BeadOrphaned {
                bead_id: id.clone(),
            },
            EventKind::DispatchStarted {
                bead_id: id.clone(),
                agent: "claude".to_string(),
                prompt_len: 100,
            },
            EventKind::DispatchCompleted {
                bead_id: id.clone(),
                exit_code: 0,
                duration_ms: 3000,
            },
            EventKind::OutcomeClassified {
                bead_id: id.clone(),
                outcome: "success".to_string(),
                exit_code: 0,
            },
            EventKind::OutcomeHandled {
                bead_id: id.clone(),
                outcome: "success".to_string(),
                action: "none".to_string(),
            },
            EventKind::HeartbeatEmitted {
                bead_id: Some(id.clone()),
                state: "EXECUTING".to_string(),
            },
            EventKind::StuckDetected {
                bead_id: id.clone(),
                age_secs: 600,
            },
            EventKind::StuckReleased {
                bead_id: id.clone(),
                peer_worker: "other".to_string(),
            },
            EventKind::HealthCheck {
                db_healthy: true,
                disk_free_mb: 10240,
                peer_count: 2,
            },
            EventKind::EffortRecorded {
                bead_id: id.clone(),
                elapsed_ms: 45000,
                agent_name: "claude-sonnet".to_string(),
                model: Some("claude-sonnet-4-6".to_string()),
                tokens_in: Some(10000),
                tokens_out: Some(2000),
                estimated_cost_usd: Some(0.06),
            },
            EventKind::BudgetWarning {
                daily_cost: 8.50,
                threshold: 5.0,
            },
            EventKind::BudgetStop {
                daily_cost: 55.0,
                threshold: 50.0,
            },
            EventKind::SinkError {
                message: "test".to_string(),
            },
        ];

        for kind in &kinds {
            telemetry.emit(kind.clone()).unwrap();
        }

        drop(telemetry);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let collected = events.lock().unwrap();
        assert_eq!(
            collected.len(),
            kinds.len(),
            "expected {} events, got {}",
            kinds.len(),
            collected.len()
        );

        // Every event should roundtrip through JSON
        for event in collected.iter() {
            let json = serde_json::to_string(event).expect("serialize");
            let parsed: TelemetryEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(parsed.event_type, event.event_type);
            assert_eq!(parsed.sequence, event.sequence);
        }
    }

    // ── StdoutSink tests ──

    #[test]
    fn stdout_sink_format_minimal() {
        let sink = StdoutSink {
            format: StdoutFormat::Minimal,
            use_color: false,
        };
        let event = TelemetryEvent {
            timestamp: chrono::NaiveDate::from_ymd_opt(2026, 3, 20)
                .unwrap()
                .and_hms_opt(15, 30, 0)
                .unwrap()
                .and_utc(),
            event_type: "bead.claim.succeeded".to_string(),
            worker_id: "alpha".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 0,
            bead_id: Some(BeadId::from("nd-a3f8")),
            workspace: None,
            data: serde_json::json!({"bead_id": "nd-a3f8"}),
            duration_ms: None,
        };
        let line = sink.format_event(&event);
        assert!(line.contains("15:30:00"));
        assert!(line.contains("[alpha]"));
        assert!(line.contains("bead.claim.succeeded"));
    }

    #[test]
    fn stdout_sink_format_normal_includes_bead_id() {
        let sink = StdoutSink {
            format: StdoutFormat::Normal,
            use_color: false,
        };
        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "bead.claim.succeeded".to_string(),
            worker_id: "alpha".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 0,
            bead_id: Some(BeadId::from("nd-a3f8")),
            workspace: None,
            data: serde_json::json!({"bead_id": "nd-a3f8"}),
            duration_ms: None,
        };
        let line = sink.format_event(&event);
        assert!(line.contains("CLAIMED"), "should use short type: {}", line);
        assert!(line.contains("nd-a3f8"), "should include bead id: {}", line);
    }

    #[test]
    fn stdout_sink_format_verbose_includes_data() {
        let sink = StdoutSink {
            format: StdoutFormat::Verbose,
            use_color: false,
        };
        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "agent.dispatched".to_string(),
            worker_id: "bravo".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 5,
            bead_id: Some(BeadId::from("nd-xyz")),
            workspace: None,
            data: serde_json::json!({"bead_id": "nd-xyz", "agent": "claude", "prompt_len": 500}),
            duration_ms: Some(3000),
        };
        let line = sink.format_event(&event);
        assert!(
            line.contains("DISPATCHED"),
            "should use short type: {}",
            line
        );
        assert!(line.contains("3000ms"), "should include duration: {}", line);
        assert!(line.contains("claude"), "should include data: {}", line);
    }

    #[test]
    fn stdout_sink_colorize_returns_ansi_codes() {
        let sink = StdoutSink {
            format: StdoutFormat::Normal,
            use_color: true,
        };
        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "worker.errored".to_string(),
            worker_id: "alpha".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({"error_type": "config", "error_message": "bad", "beads_processed": 0}),
            duration_ms: None,
        };
        let line = sink.format_event(&event);
        assert!(line.contains("\x1b[31m"), "errors should be red: {}", line);
        assert!(line.contains("\x1b[0m"), "should have reset: {}", line);
    }

    // ── parse_since tests ──

    #[test]
    fn parse_since_relative_hours() {
        let now = Utc::now();
        let dt = parse_since("1h").unwrap();
        let diff = now.signed_duration_since(dt).num_minutes();
        assert!((58..=62).contains(&diff), "should be ~60 min ago: {diff}");
    }

    #[test]
    fn parse_since_relative_minutes() {
        let now = Utc::now();
        let dt = parse_since("30m").unwrap();
        let diff = now.signed_duration_since(dt).num_minutes();
        assert!((28..=32).contains(&diff), "should be ~30 min ago: {diff}");
    }

    #[test]
    fn parse_since_relative_days() {
        let now = Utc::now();
        let dt = parse_since("7d").unwrap();
        let diff = now.signed_duration_since(dt).num_days();
        assert!((6..=8).contains(&diff), "should be ~7 days ago: {diff}");
    }

    #[test]
    fn parse_since_absolute_date() {
        let dt = parse_since("2026-03-20").unwrap();
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2026-03-20");
    }

    #[test]
    fn parse_since_invalid_fails() {
        assert!(parse_since("not-a-date").is_err());
    }

    // ── glob_to_regex tests ──

    #[test]
    fn glob_to_regex_wildcard() {
        let re = glob_to_regex("bead.claim.*").unwrap();
        assert!(re.is_match("bead.claim.succeeded"));
        assert!(re.is_match("bead.claim.failed"));
        assert!(!re.is_match("worker.started"));
    }

    #[test]
    fn glob_to_regex_exact() {
        let re = glob_to_regex("worker.started").unwrap();
        assert!(re.is_match("worker.started"));
        assert!(!re.is_match("worker.stopped"));
    }

    #[test]
    fn glob_to_regex_double_wildcard() {
        let re = glob_to_regex("*.*").unwrap();
        assert!(re.is_match("bead.claim.succeeded"));
        assert!(re.is_match("worker.started"));
    }

    // ── read_logs and cost summary tests ──

    #[test]
    fn read_logs_empty_dir() {
        let dir = std::env::temp_dir().join("needle-test-logs-empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let events = read_logs(&dir, None, None).unwrap();
        assert!(events.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_logs_with_filter() {
        let dir = std::env::temp_dir().join("needle-test-logs-filter");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let event1 = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "bead.claim.succeeded".to_string(),
            worker_id: "alpha".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 0,
            bead_id: Some(BeadId::from("nd-abc")),
            workspace: None,
            data: serde_json::json!({}),
            duration_ms: None,
        };
        let event2 = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "worker.started".to_string(),
            worker_id: "alpha".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 1,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({}),
            duration_ms: None,
        };

        let log_file = dir.join("test-aabb0011.jsonl");
        let content = format!(
            "{}\n{}\n",
            serde_json::to_string(&event1).unwrap(),
            serde_json::to_string(&event2).unwrap()
        );
        std::fs::write(&log_file, content).unwrap();

        let re = glob_to_regex("bead.claim.*").unwrap();
        let events = read_logs(&dir, None, Some(&re)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "bead.claim.succeeded");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compute_cost_summary_aggregates() {
        let events = vec![
            TelemetryEvent {
                timestamp: Utc::now(),
                event_type: "effort.recorded".to_string(),
                worker_id: "alpha".to_string(),
                session_id: "aabb0011".to_string(),
                sequence: 0,
                bead_id: Some(BeadId::from("nd-1")),
                workspace: None,
                data: serde_json::json!({
                    "bead_id": "nd-1",
                    "elapsed_ms": 10000,
                    "agent_name": "claude",
                    "tokens_in": 5000,
                    "tokens_out": 1000,
                    "estimated_cost_usd": 0.05,
                }),
                duration_ms: Some(10000),
            },
            TelemetryEvent {
                timestamp: Utc::now(),
                event_type: "effort.recorded".to_string(),
                worker_id: "alpha".to_string(),
                session_id: "aabb0011".to_string(),
                sequence: 1,
                bead_id: Some(BeadId::from("nd-2")),
                workspace: None,
                data: serde_json::json!({
                    "bead_id": "nd-2",
                    "elapsed_ms": 20000,
                    "agent_name": "claude",
                    "tokens_in": 8000,
                    "tokens_out": 2000,
                    "estimated_cost_usd": 0.08,
                }),
                duration_ms: Some(20000),
            },
            TelemetryEvent {
                timestamp: Utc::now(),
                event_type: "bead.claim.succeeded".to_string(),
                worker_id: "alpha".to_string(),
                session_id: "aabb0011".to_string(),
                sequence: 2,
                bead_id: Some(BeadId::from("nd-1")),
                workspace: None,
                data: serde_json::json!({}),
                duration_ms: None,
            },
        ];

        let summary = compute_cost_summary(&events);
        assert_eq!(summary.total_events, 2);
        assert!((summary.total_cost_usd - 0.13).abs() < 0.001);
        assert_eq!(summary.total_tokens_in, 13000);
        assert_eq!(summary.total_tokens_out, 3000);
        assert_eq!(summary.total_elapsed_ms, 30000);
    }

    // ── format_duration_ms tests ──

    #[test]
    fn format_duration_ms_milliseconds() {
        assert_eq!(format_duration_ms(500), "500ms");
    }

    #[test]
    fn format_duration_ms_seconds() {
        assert_eq!(format_duration_ms(3500), "3.5s");
    }

    #[test]
    fn format_duration_ms_minutes() {
        assert_eq!(format_duration_ms(125_000), "2m5s");
    }

    #[test]
    fn short_type_mappings() {
        assert_eq!(StdoutSink::short_type("bead.claim.succeeded"), "CLAIMED");
        assert_eq!(StdoutSink::short_type("worker.started"), "STARTED");
        assert_eq!(StdoutSink::short_type("agent.dispatched"), "DISPATCHED");
        assert_eq!(StdoutSink::short_type("outcome.handled"), "HANDLED");
        assert_eq!(StdoutSink::short_type("effort.recorded"), "EFFORT");
        assert_eq!(StdoutSink::short_type("bead.mitosis.split"), "MITOSIS");
        assert_eq!(StdoutSink::short_type("unknown.event"), "unknown.event");
    }
}
