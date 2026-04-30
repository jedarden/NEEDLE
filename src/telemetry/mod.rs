//! Structured telemetry — JSONL event stream, never on stdout/stderr.
//!
//! Every state transition, claim attempt, dispatch, and outcome emits a typed
//! event. The emitter is non-blocking: events are queued and written by a
//! background task. A broken sink never blocks or panics the worker.
//!
//! ## Architecture
//! ```text
//! worker → emit() → mpsc::Sender → [background task] → TelemetryBus → Vec<Box<dyn Sink>>
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

use crate::config::{ColorMode, HookConfig, StdoutFormat, StdoutSinkConfig, TelemetryConfig};
use crate::types::{BeadId, WorkerId, WorkerState};

// ─── OTLP Sink (feature-gated) ───────────────────────────────────────────────────

#[cfg(feature = "otlp")]
pub mod otlp;

#[cfg(feature = "otlp")]
pub use otlp::OtlpSink;

// ─── Test Utilities ────────────────────────────────────────────────────────────

#[cfg(test)]
pub mod test_utils;

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
    /// W3C trace-id hex (32 lowercase hex chars). Present only when emitted inside an OTel span.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// W3C span-id hex (16 lowercase hex chars). Present only when emitted inside an OTel span.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
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
    WorkerBooting {
        worker_name: String,
        version: String,
    },
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
        /// How many times the waterfall restarted before exhausting.
        waterfall_restarts: u32,
        /// Name of each strand that returned WorkCreated (one entry per restart).
        restart_triggers: Vec<String>,
        /// All strand evaluations in order, across all waterfall passes.
        /// Each entry is (strand_name, result, duration_ms) for diagnostic visibility.
        strand_evaluations: Vec<(String, String, u64)>,
    },
    WorkerIdle {
        backoff_seconds: u64,
    },
    IdleSleepCompleted {
        backoff_secs: u64,
        elapsed_secs: u64,
        shutdown_checks: u64,
    },
    IdleSleepEntered {
        backoff_secs: u64,
        beads_processed: u64,
        uptime_secs: u64,
    },
    StateTransition {
        from: WorkerState,
        to: WorkerState,
    },
    InitStepStarted {
        step: String,
    },
    InitStepCompleted {
        step: String,
        duration_ms: u64,
    },
    WorkerBootTimeout {
        elapsed_ms: u64,
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
        priority: i32,
        strand: String,
    },
    ClaimRaceLost {
        bead_id: BeadId,
    },
    ClaimFailed {
        bead_id: BeadId,
        reason: String,
    },
    ClaimRaceLostSkipped {
        consecutive_losses: u32,
        threshold: u32,
    },

    // ── Bead lifecycle ──
    BeadReleased {
        bead_id: BeadId,
        reason: String,
    },
    BeadReleaseFailed {
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
        /// Template name used to build the prompt (e.g., `"pluck"`).
        template_name: String,
        /// Version tag identifying which variant was used (e.g., `"pluck-default"`, `"pluck-v2"`).
        template_version: String,
        /// SHA-256 hex digest of the rendered prompt content.
        prompt_hash: String,
    },
    DispatchCompleted {
        bead_id: BeadId,
        exit_code: i32,
        duration_ms: u64,
        agent: String,
        model: Option<String>,
    },
    BuildTimeout {
        bead_id: BeadId,
        timeout_secs: u64,
    },
    BuildHeartbeat {
        bead_id: BeadId,
        elapsed_ms: u64,
    },
    BuildTimeout {
        bead_id: BeadId,
        timeout_secs: u64,
    },
    BuildHeartbeat {
        bead_id: BeadId,
        elapsed_ms: u64,
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
    WorkerHandlingTimeout {
        bead_id: BeadId,
        outcome: String,
        operation: String,
        error: String,
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
        agent_logs_cleaned: u32,
        zero_activity_logs_cleaned: u32,
        traces_pruned: u32,
        traces_deleted: u32,
        workers_deregistered: u32,
        idle_workers_flagged: u32,
        rate_limits_cleaned: u32,
    },
    MendTraceCleanup {
        traces_pruned: u32,
        traces_deleted: u32,
    },
    MendLearningCleanup {
        pruned: u32,
        consolidated: u32,
    },
    MendIdleWorkerFlagged {
        worker_id: String,
        pid: u32,
        age_secs: u64,
    },
    MendWorkerDeregistered {
        worker_id: String,
        pid: u32,
    },
    MendOrphanedHeartbeatRemoved {
        worker_id: String,
        age_secs: u64,
    },
    MendDependencyRemoved {
        bead_id: BeadId,
        blocker_id: BeadId,
    },
    MendBeadReleaseFailed {
        bead_id: String,
        assignee: String,
        error: String,
    },
    MendDependencyCleanupFailed {
        bead_id: String,
        blocker_id: String,
        error: String,
    },
    MendLockRemoveFailed {
        lock_path: String,
        error: String,
    },
    MendRateLimitCleaned {
        provider: String,
        age_secs: u64,
    },
    MendRateLimitProviderRemoved {
        provider: String,
    },
    MendRateLimitProviderReset {
        provider: String,
        age_secs: u64,
    },
    MendZeroActivityLogCleaned {
        worker_id: String,
        log_path: String,
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

    // ── Validation gates ──
    VerificationFailed {
        bead_id: BeadId,
        command: String,
        exit_code: Option<i32>,
        output: String,
    },
    VerificationPassed {
        bead_id: BeadId,
        gates_run: u32,
    },

    // ── Unravel ──
    UnravelAnalyzed {
        bead_id: BeadId,
        alternatives_proposed: u32,
    },
    UnravelSkipped {
        bead_id: BeadId,
        reason: String,
    },

    // ── Reflect ──
    ReflectStarted {
        beads_since_last: usize,
    },
    ReflectConsolidated {
        learnings_added: usize,
        learnings_pruned: usize,
        skills_promoted: usize,
        beads_processed: usize,
    },
    ReflectSkipped {
        reason: String,
    },
    ReflectTranscriptsRead {
        sessions_count: usize,
        entries_count: usize,
        parse_errors: usize,
    },
    ReflectDriftDetected {
        cluster_size: usize,
        category: String,
        sessions: Vec<String>,
    },
    ReflectDriftPromoted {
        pattern: String,
        category: String,
    },
    ReflectDecisionExtracted {
        bead_id: BeadId,
        has_alternatives: bool,
        rationale_length: usize,
    },
    ReflectAdrCreated {
        bead_id: BeadId,
        path: String,
    },
    ReflectLearningPromoted {
        learning_id: String,
        target_path: String,
        workspace_count: usize,
        is_decision: bool,
    },
    ReflectLearningDeduplicated {
        learning_id: String,
        existing_entry: String,
    },
    ReflectClaudeMdWritten {
        path: String,
        entries_added: usize,
        entries_updated: usize,
    },

    // ── Drift detection ──
    DriftDetectionStarted {
        sessions_analyzed: usize,
    },
    DriftDetectionCompleted {
        sessions_analyzed: usize,
        clusters_found: usize,
        evolved_count: usize,
        inconsistent_count: usize,
    },
    DriftDetectionSkipped {
        reason: String,
    },
    DriftReportWritten {
        report_path: String,
        clusters: usize,
    },

    // ── Decision detection (ADR) ──
    DecisionDetectionStarted {
        sessions_analyzed: usize,
    },
    DecisionDetectionCompleted {
        sessions_analyzed: usize,
        decisions_found: usize,
    },
    DecisionDetectionSkipped {
        reason: String,
    },

    // ── Pulse ──
    PulseScannerStarted {
        scanner_name: String,
    },
    PulseScannerCompleted {
        scanner_name: String,
        findings_count: u32,
    },
    PulseScannerFailed {
        scanner_name: String,
        error: String,
    },
    PulseBeadCreated {
        bead_id: BeadId,
        scanner_name: String,
        severity: u8,
    },
    PulseSkipped {
        reason: String,
    },

    // ── Hot-reload ──
    UpgradeDetected {
        old_hash: String,
        new_hash: String,
    },
    UpgradeCompleted {
        new_hash: String,
    },
    RollbackCompleted {
        rolled_back_hash: String,
        restored_hash: String,
    },

    // ── Canary testing ──
    CanaryStarted {
        suite: String,
    },
    CanarySuiteCompleted {
        suite: String,
        passed: u32,
        failed: u32,
    },
    CanaryPromoted {
        hash: String,
    },
    CanaryRejected {
        reason: String,
    },

    // ── Output transform ──
    OutputTransformSpawned {
        bead_id: BeadId,
        transform_cmd: String,
        log_path: String,
    },
    OutputTransformExited {
        bead_id: BeadId,
        exit_code: i32,
    },
    OutputTransformSkipped {
        bead_id: BeadId,
        reason: String,
    },

    // ── Transform lifecycle ──
    TransformStarted {
        bead_id: BeadId,
        transform_binary: String,
        agent: String,
    },
    TransformCompleted {
        bead_id: BeadId,
        events_written: u64,
        duration_ms: u64,
    },
    TransformFailed {
        bead_id: BeadId,
        error: String,
        exit_code: i32,
    },
    TransformSkipped {
        bead_id: BeadId,
        reason: String,
    },

    // ── Internal ──
    SinkError {
        message: String,
    },
    OtlpDropped {
        signal: String,
        dropped_count: u64,
        queue_full: bool,
    },
    OtlpShutdownTimeout {
        flushed_batches: u64,
        remaining_batches: u64,
    },
}

impl EventKind {
    /// Return the dotted event type string.
    pub fn event_type(&self) -> &'static str {
        match self {
            EventKind::WorkerBooting { .. } => "worker.booting",
            EventKind::WorkerStarted { .. } => "worker.started",
            EventKind::WorkerStopped { .. } => "worker.stopped",
            EventKind::WorkerErrored { .. } => "worker.errored",
            EventKind::WorkerExhausted { .. } => "worker.exhausted",
            EventKind::WorkerIdle { .. } => "worker.idle",
            EventKind::IdleSleepCompleted { .. } => "worker.idle_sleep_completed",
            EventKind::IdleSleepEntered { .. } => "worker.idle_sleep_entered",
            EventKind::StateTransition { .. } => "worker.state_transition",
            EventKind::InitStepStarted { .. } => "init.step.started",
            EventKind::InitStepCompleted { .. } => "init.step.completed",
            EventKind::WorkerBootTimeout { .. } => "worker.boot.timeout",
            EventKind::StrandEvaluated { .. } => "strand.evaluated",
            EventKind::StrandSkipped { .. } => "strand.skipped",
            EventKind::QueueEmpty => "worker.queue_empty",
            EventKind::ClaimAttempt { .. } => "bead.claim.attempted",
            EventKind::ClaimSuccess { .. } => "bead.claim.succeeded",
            EventKind::ClaimRaceLost { .. } => "bead.claim.race_lost",
            EventKind::ClaimRaceLostSkipped { .. } => "bead.claim.race_lost_skipped",
            EventKind::ClaimFailed { .. } => "bead.claim.failed",
            EventKind::BeadReleased { .. } => "bead.released",
            EventKind::BeadReleaseFailed { .. } => "bead.release.failed",
            EventKind::BeadCompleted { .. } => "bead.completed",
            EventKind::BeadOrphaned { .. } => "bead.orphaned",
            EventKind::DispatchStarted { .. } => "agent.dispatched",
            EventKind::DispatchCompleted { .. } => "agent.completed",
            EventKind::BuildTimeout { .. } => "build.timeout",
            EventKind::BuildHeartbeat { .. } => "build.heartbeat",
            EventKind::OutcomeClassified { .. } => "outcome.classified",
            EventKind::OutcomeHandled { .. } => "outcome.handled",
            EventKind::WorkerHandlingTimeout { .. } => "worker.handling.timeout",
            EventKind::HeartbeatEmitted { .. } => "heartbeat.emitted",
            EventKind::StuckDetected { .. } => "peer.stale",
            EventKind::StuckReleased { .. } => "peer.crashed",
            EventKind::HealthCheck { .. } => "health.check",
            EventKind::MendOrphanedLockRemoved { .. } => "mend.orphaned_lock_removed",
            EventKind::MendDependencyCleaned { .. } => "mend.dependency_cleaned",
            EventKind::MendDbRepaired { .. } => "mend.db_repaired",
            EventKind::MendDbRebuilt => "mend.db_rebuilt",
            EventKind::MendCycleSummary { .. } => "mend.cycle_summary",
            EventKind::MendTraceCleanup { .. } => "mend.trace_cleanup",
            EventKind::MendLearningCleanup { .. } => "mend.learning_cleanup",
            EventKind::MendIdleWorkerFlagged { .. } => "mend.idle_worker_flagged",
            EventKind::MendWorkerDeregistered { .. } => "mend.worker_deregistered",
            EventKind::MendOrphanedHeartbeatRemoved { .. } => "mend.orphaned_heartbeat_removed",
            EventKind::MendDependencyRemoved { .. } => "mend.dependency_removed",
            EventKind::MendBeadReleaseFailed { .. } => "mend.bead_release_failed",
            EventKind::MendDependencyCleanupFailed { .. } => "mend.dependency_cleanup_failed",
            EventKind::MendLockRemoveFailed { .. } => "mend.lock_remove_failed",
            EventKind::MendRateLimitCleaned { .. } => "mend.rate_limit_cleaned",
            EventKind::MendRateLimitProviderRemoved { .. } => "mend.rate_limit_provider_removed",
            EventKind::MendRateLimitProviderReset { .. } => "mend.rate_limit_provider_reset",
            EventKind::MendZeroActivityLogCleaned { .. } => "mend.zero_activity_log_cleaned",
            EventKind::EffortRecorded { .. } => "effort.recorded",
            EventKind::BudgetWarning { .. } => "budget.warning",
            EventKind::BudgetStop { .. } => "budget.stop",
            EventKind::RateLimitWait { .. } => "rate_limit.wait",
            EventKind::RateLimitAllowed { .. } => "rate_limit.allowed",
            EventKind::MitosisEvaluated { .. } => "bead.mitosis.evaluated",
            EventKind::MitosisSplit { .. } => "bead.mitosis.split",
            EventKind::MitosisSkipped { .. } => "bead.mitosis.skipped",
            EventKind::VerificationFailed { .. } => "verification.failed",
            EventKind::VerificationPassed { .. } => "verification.passed",
            EventKind::UnravelAnalyzed { .. } => "bead.unravel.analyzed",
            EventKind::UnravelSkipped { .. } => "bead.unravel.skipped",
            EventKind::ReflectStarted { .. } => "reflect.started",
            EventKind::ReflectConsolidated { .. } => "reflect.consolidated",
            EventKind::ReflectSkipped { .. } => "reflect.skipped",
            EventKind::ReflectTranscriptsRead { .. } => "reflect.transcripts_read",
            EventKind::ReflectDriftDetected { .. } => "reflect.drift_detected",
            EventKind::ReflectDriftPromoted { .. } => "reflect.drift_promoted",
            EventKind::ReflectDecisionExtracted { .. } => "reflect.decision_extracted",
            EventKind::ReflectAdrCreated { .. } => "reflect.adr_created",
            EventKind::ReflectLearningPromoted { .. } => "reflect.learning_promoted",
            EventKind::ReflectLearningDeduplicated { .. } => "reflect.learning_deduplicated",
            EventKind::ReflectClaudeMdWritten { .. } => "reflect.claudemd_written",
            EventKind::DriftDetectionStarted { .. } => "drift.started",
            EventKind::DriftDetectionCompleted { .. } => "drift.completed",
            EventKind::DriftDetectionSkipped { .. } => "drift.skipped",
            EventKind::DriftReportWritten { .. } => "drift.report_written",
            EventKind::DecisionDetectionStarted { .. } => "decision.started",
            EventKind::DecisionDetectionCompleted { .. } => "decision.completed",
            EventKind::DecisionDetectionSkipped { .. } => "decision.skipped",
            EventKind::PulseScannerStarted { .. } => "pulse.scanner_started",
            EventKind::PulseScannerCompleted { .. } => "pulse.scanner_completed",
            EventKind::PulseScannerFailed { .. } => "pulse.scanner_failed",
            EventKind::PulseBeadCreated { .. } => "pulse.bead_created",
            EventKind::PulseSkipped { .. } => "pulse.skipped",
            EventKind::UpgradeDetected { .. } => "worker.upgrade.detected",
            EventKind::UpgradeCompleted { .. } => "worker.upgrade.completed",
            EventKind::RollbackCompleted { .. } => "rollback.completed",
            EventKind::CanaryStarted { .. } => "canary.started",
            EventKind::CanarySuiteCompleted { .. } => "canary.suite_completed",
            EventKind::CanaryPromoted { .. } => "canary.promoted",
            EventKind::CanaryRejected { .. } => "canary.rejected",
            EventKind::OutputTransformSpawned { .. } => "agent.transform.spawned",
            EventKind::OutputTransformExited { .. } => "agent.transform.exited",
            EventKind::OutputTransformSkipped { .. } => "agent.transform.skipped",
            EventKind::TransformStarted { .. } => "transform.started",
            EventKind::TransformCompleted { .. } => "transform.completed",
            EventKind::TransformFailed { .. } => "transform.failed",
            EventKind::TransformSkipped { .. } => "transform.skipped",
            EventKind::SinkError { .. } => "telemetry.sink_error",
            EventKind::OtlpDropped { .. } => "telemetry.otlp.dropped",
            EventKind::OtlpShutdownTimeout { .. } => "telemetry.otlp.shutdown_timeout",
        }
    }

    /// Extract bead_id context from this event (if any).
    pub fn bead_id(&self) -> Option<BeadId> {
        match self {
            EventKind::ClaimAttempt { bead_id, .. }
            | EventKind::ClaimSuccess { bead_id, .. }
            | EventKind::ClaimRaceLost { bead_id }
            | EventKind::ClaimFailed { bead_id, .. }
            | EventKind::BeadReleased { bead_id, .. }
            | EventKind::BeadReleaseFailed { bead_id, .. }
            | EventKind::BeadCompleted { bead_id, .. }
            | EventKind::BeadOrphaned { bead_id }
            | EventKind::DispatchStarted { bead_id, .. }
            | EventKind::DispatchCompleted { bead_id, .. }
            | EventKind::BuildTimeout { bead_id, .. }
            | EventKind::BuildHeartbeat { bead_id, .. }
            | EventKind::OutcomeClassified { bead_id, .. }
            | EventKind::OutcomeHandled { bead_id, .. }
            | EventKind::WorkerHandlingTimeout { bead_id, .. }
            | EventKind::StuckDetected { bead_id, .. }
            | EventKind::StuckReleased { bead_id, .. }
            | EventKind::MendDependencyCleaned { bead_id, .. }
            | EventKind::MendDependencyRemoved { bead_id, .. }
            | EventKind::EffortRecorded { bead_id, .. }
            | EventKind::MitosisEvaluated { bead_id, .. }
            | EventKind::VerificationFailed { bead_id, .. }
            | EventKind::VerificationPassed { bead_id, .. }
            | EventKind::UnravelAnalyzed { bead_id, .. }
            | EventKind::UnravelSkipped { bead_id, .. }
            | EventKind::OutputTransformSpawned { bead_id, .. }
            | EventKind::OutputTransformExited { bead_id, .. }
            | EventKind::OutputTransformSkipped { bead_id, .. }
            | EventKind::TransformStarted { bead_id, .. }
            | EventKind::TransformCompleted { bead_id, .. }
            | EventKind::TransformFailed { bead_id, .. }
            | EventKind::TransformSkipped { bead_id, .. }
            | EventKind::ReflectDecisionExtracted { bead_id, .. }
            | EventKind::ReflectAdrCreated { bead_id, .. } => Some(bead_id.clone()),
            EventKind::MitosisSplit { parent_id, .. }
            | EventKind::MitosisSkipped { parent_id, .. } => Some(parent_id.clone()),
            EventKind::HeartbeatEmitted { bead_id, .. } => bead_id.clone(),
            EventKind::WorkerBooting { .. }
            | EventKind::WorkerStarted { .. }
            | EventKind::WorkerStopped { .. }
            | EventKind::WorkerErrored { .. }
            | EventKind::WorkerExhausted { .. }
            | EventKind::WorkerIdle { .. }
            | EventKind::IdleSleepCompleted { .. }
            | EventKind::IdleSleepEntered { .. }
            | EventKind::StateTransition { .. }
            | EventKind::InitStepStarted { .. }
            | EventKind::InitStepCompleted { .. }
            | EventKind::WorkerBootTimeout { .. }
            | EventKind::StrandEvaluated { .. }
            | EventKind::StrandSkipped { .. }
            | EventKind::QueueEmpty
            | EventKind::HealthCheck { .. }
            | EventKind::MendOrphanedLockRemoved { .. }
            | EventKind::MendDbRepaired { .. }
            | EventKind::MendDbRebuilt
            | EventKind::MendCycleSummary { .. }
            | EventKind::MendIdleWorkerFlagged { .. }
            | EventKind::MendWorkerDeregistered { .. }
            | EventKind::MendOrphanedHeartbeatRemoved { .. }
            | EventKind::MendBeadReleaseFailed { .. }
            | EventKind::MendDependencyCleanupFailed { .. }
            | EventKind::MendLockRemoveFailed { .. }
            | EventKind::MendRateLimitCleaned { .. }
            | EventKind::MendRateLimitProviderRemoved { .. }
            | EventKind::MendRateLimitProviderReset { .. }
            | EventKind::MendZeroActivityLogCleaned { .. }
            | EventKind::MendTraceCleanup { .. }
            | EventKind::MendLearningCleanup { .. }
            | EventKind::BudgetWarning { .. }
            | EventKind::BudgetStop { .. }
            | EventKind::RateLimitWait { .. }
            | EventKind::RateLimitAllowed { .. }
            | EventKind::ReflectStarted { .. }
            | EventKind::ReflectConsolidated { .. }
            | EventKind::ReflectSkipped { .. }
            | EventKind::ReflectTranscriptsRead { .. }
            | EventKind::ReflectDriftDetected { .. }
            | EventKind::ReflectDriftPromoted { .. }
            | EventKind::ReflectLearningPromoted { .. }
            | EventKind::ReflectLearningDeduplicated { .. }
            | EventKind::ReflectClaudeMdWritten { .. }
            | EventKind::PulseScannerStarted { .. }
            | EventKind::PulseScannerCompleted { .. }
            | EventKind::PulseScannerFailed { .. }
            | EventKind::PulseSkipped { .. }
            | EventKind::UpgradeDetected { .. }
            | EventKind::UpgradeCompleted { .. }
            | EventKind::RollbackCompleted { .. }
            | EventKind::CanaryStarted { .. }
            | EventKind::CanarySuiteCompleted { .. }
            | EventKind::CanaryPromoted { .. }
            | EventKind::CanaryRejected { .. }
            | EventKind::ClaimRaceLostSkipped { .. }
            | EventKind::SinkError { .. }
            | EventKind::OtlpDropped { .. }
            | EventKind::OtlpShutdownTimeout { .. }
            | EventKind::DriftDetectionStarted { .. }
            | EventKind::DriftDetectionCompleted { .. }
            | EventKind::DriftDetectionSkipped { .. }
            | EventKind::DriftReportWritten { .. }
            | EventKind::DecisionDetectionStarted { .. }
            | EventKind::DecisionDetectionCompleted { .. }
            | EventKind::DecisionDetectionSkipped { .. } => None,
            EventKind::PulseBeadCreated { bead_id, .. } => Some(bead_id.clone()),
        }
    }

    /// Serialize event-specific payload to a JSON value.
    pub fn to_data(&self) -> serde_json::Value {
        match self {
            EventKind::WorkerBooting {
                worker_name,
                version,
            } => {
                serde_json::json!({ "worker_name": worker_name, "version": version })
            }
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
                waterfall_restarts,
                restart_triggers,
                strand_evaluations,
            } => {
                serde_json::json!({
                    "cycle_count": cycle_count,
                    "last_strand_evaluated": last_strand,
                    "waterfall_restarts": waterfall_restarts,
                    "restart_triggers": restart_triggers,
                    "strand_evaluations": strand_evaluations,
                })
            }
            EventKind::WorkerIdle { backoff_seconds } => {
                serde_json::json!({ "backoff_seconds": backoff_seconds })
            }
            EventKind::StateTransition { from, to } => {
                serde_json::json!({ "from": format!("{from}"), "to": format!("{to}") })
            }
            EventKind::InitStepStarted { step } => {
                serde_json::json!({ "step": step })
            }
            EventKind::InitStepCompleted { step, duration_ms } => {
                serde_json::json!({ "step": step, "duration_ms": duration_ms })
            }
            EventKind::WorkerBootTimeout { elapsed_ms } => {
                serde_json::json!({ "elapsed_ms": elapsed_ms })
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
            EventKind::ClaimSuccess {
                bead_id,
                priority,
                strand,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "priority": priority,
                    "strand": strand,
                })
            }
            EventKind::ClaimRaceLost { bead_id } => {
                serde_json::json!({ "bead_id": bead_id.as_ref() })
            }
            EventKind::ClaimRaceLostSkipped {
                consecutive_losses,
                threshold,
            } => {
                serde_json::json!({
                    "consecutive_losses": consecutive_losses,
                    "threshold": threshold,
                })
            }
            EventKind::ClaimFailed { bead_id, reason } => {
                serde_json::json!({ "bead_id": bead_id.as_ref(), "reason": reason })
            }
            EventKind::BeadReleased { bead_id, reason } => {
                serde_json::json!({ "bead_id": bead_id.as_ref(), "reason": reason })
            }
            EventKind::BeadReleaseFailed { bead_id, reason } => {
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
                template_name,
                template_version,
                prompt_hash,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "agent": agent,
                    "prompt_len": prompt_len,
                    "template_name": template_name,
                    "template_version": template_version,
                    "prompt_hash": format!("sha256:{prompt_hash}"),
                })
            }
            EventKind::DispatchCompleted {
                bead_id,
                exit_code,
                duration_ms,
                agent,
                model,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "exit_code": exit_code,
                    "duration_ms": duration_ms,
                    "agent": agent,
                    "model": model,
                })
            }
            EventKind::BuildTimeout {
                bead_id,
                timeout_secs,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "timeout_secs": timeout_secs,
                })
            }
            EventKind::BuildHeartbeat {
                bead_id,
                elapsed_ms,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "elapsed_ms": elapsed_ms,
                })
            }
            EventKind::BuildTimeout {
                bead_id,
                timeout_secs,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "timeout_secs": timeout_secs,
                })
            }
            EventKind::BuildHeartbeat {
                bead_id,
                elapsed_ms,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "elapsed_ms": elapsed_ms,
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
            EventKind::WorkerHandlingTimeout {
                bead_id,
                outcome,
                operation,
                error,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "outcome": outcome,
                    "operation": operation,
                    "error": error,
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
                agent_logs_cleaned,
                zero_activity_logs_cleaned,
                traces_pruned,
                traces_deleted,
                workers_deregistered,
                idle_workers_flagged,
                rate_limits_cleaned,
            } => {
                serde_json::json!({
                    "beads_released": beads_released,
                    "locks_removed": locks_removed,
                    "deps_cleaned": deps_cleaned,
                    "db_repaired": db_repaired,
                    "db_rebuilt": db_rebuilt,
                    "agent_logs_cleaned": agent_logs_cleaned,
                    "zero_activity_logs_cleaned": zero_activity_logs_cleaned,
                    "traces_pruned": traces_pruned,
                    "traces_deleted": traces_deleted,
                    "workers_deregistered": workers_deregistered,
                    "idle_workers_flagged": idle_workers_flagged,
                    "rate_limits_cleaned": rate_limits_cleaned,
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
            EventKind::VerificationFailed {
                bead_id,
                command,
                exit_code,
                output,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "command": command,
                    "exit_code": exit_code,
                    "output": output,
                })
            }
            EventKind::VerificationPassed { bead_id, gates_run } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "gates_run": gates_run,
                })
            }
            EventKind::UnravelAnalyzed {
                bead_id,
                alternatives_proposed,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "alternatives_proposed": alternatives_proposed,
                })
            }
            EventKind::UnravelSkipped { bead_id, reason } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "reason": reason,
                })
            }
            EventKind::PulseScannerStarted { scanner_name } => {
                serde_json::json!({ "scanner_name": scanner_name })
            }
            EventKind::PulseScannerCompleted {
                scanner_name,
                findings_count,
            } => {
                serde_json::json!({
                    "scanner_name": scanner_name,
                    "findings_count": findings_count,
                })
            }
            EventKind::PulseScannerFailed {
                scanner_name,
                error,
            } => {
                serde_json::json!({
                    "scanner_name": scanner_name,
                    "error": error,
                })
            }
            EventKind::PulseBeadCreated {
                bead_id,
                scanner_name,
                severity,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.as_ref(),
                    "scanner_name": scanner_name,
                    "severity": severity,
                })
            }
            EventKind::PulseSkipped { reason } => {
                serde_json::json!({ "reason": reason })
            }
            EventKind::UpgradeDetected { old_hash, new_hash } => {
                serde_json::json!({ "old_hash": old_hash, "new_hash": new_hash })
            }
            EventKind::UpgradeCompleted { new_hash } => {
                serde_json::json!({ "new_hash": new_hash })
            }
            EventKind::RollbackCompleted {
                rolled_back_hash,
                restored_hash,
            } => {
                serde_json::json!({
                    "rolled_back_hash": rolled_back_hash,
                    "restored_hash": restored_hash,
                })
            }
            EventKind::OutputTransformSpawned {
                bead_id,
                transform_cmd,
                log_path,
            } => serde_json::json!({
                "bead_id": bead_id,
                "transform_cmd": transform_cmd,
                "log_path": log_path,
            }),
            EventKind::OutputTransformExited { bead_id, exit_code } => {
                serde_json::json!({ "bead_id": bead_id, "exit_code": exit_code })
            }
            EventKind::OutputTransformSkipped { bead_id, reason } => {
                serde_json::json!({ "bead_id": bead_id, "reason": reason })
            }
            EventKind::TransformStarted {
                bead_id,
                transform_binary,
                agent,
            } => serde_json::json!({
                "bead_id": bead_id,
                "transform_binary": transform_binary,
                "agent": agent,
            }),
            EventKind::TransformCompleted {
                bead_id,
                events_written,
                duration_ms,
            } => serde_json::json!({
                "bead_id": bead_id,
                "events_written": events_written,
                "duration_ms": duration_ms,
            }),
            EventKind::TransformFailed {
                bead_id,
                error,
                exit_code,
            } => serde_json::json!({
                "bead_id": bead_id,
                "error": error,
                "exit_code": exit_code,
            }),
            EventKind::TransformSkipped { bead_id, reason } => {
                serde_json::json!({ "bead_id": bead_id, "reason": reason })
            }
            EventKind::MendTraceCleanup {
                traces_pruned,
                traces_deleted,
            } => {
                serde_json::json!({
                    "traces_pruned": traces_pruned,
                    "traces_deleted": traces_deleted,
                })
            }
            EventKind::MendLearningCleanup {
                pruned,
                consolidated,
            } => {
                serde_json::json!({
                    "pruned": pruned,
                    "consolidated": consolidated,
                })
            }
            EventKind::MendIdleWorkerFlagged {
                worker_id,
                pid,
                age_secs,
            } => {
                serde_json::json!({
                    "worker_id": worker_id,
                    "pid": pid,
                    "age_secs": age_secs,
                })
            }
            EventKind::MendWorkerDeregistered { worker_id, pid } => {
                serde_json::json!({
                    "worker_id": worker_id,
                    "pid": pid,
                })
            }
            EventKind::MendOrphanedHeartbeatRemoved {
                worker_id,
                age_secs,
            } => {
                serde_json::json!({
                    "worker_id": worker_id,
                    "age_secs": age_secs,
                })
            }
            EventKind::MendDependencyRemoved {
                bead_id,
                blocker_id,
            } => {
                serde_json::json!({
                    "bead_id": bead_id,
                    "blocker_id": blocker_id,
                })
            }
            EventKind::MendBeadReleaseFailed {
                bead_id,
                assignee,
                error,
            } => {
                serde_json::json!({
                    "bead_id": bead_id,
                    "assignee": assignee,
                    "error": error,
                })
            }
            EventKind::MendDependencyCleanupFailed {
                bead_id,
                blocker_id,
                error,
            } => {
                serde_json::json!({
                    "bead_id": bead_id,
                    "blocker_id": blocker_id,
                    "error": error,
                })
            }
            EventKind::MendLockRemoveFailed { lock_path, error } => {
                serde_json::json!({
                    "lock_path": lock_path,
                    "error": error,
                })
            }
            EventKind::MendRateLimitCleaned { provider, age_secs } => {
                serde_json::json!({
                    "provider": provider,
                    "age_secs": age_secs,
                })
            }
            EventKind::MendRateLimitProviderRemoved { provider } => {
                serde_json::json!({ "provider": provider })
            }
            EventKind::MendRateLimitProviderReset { provider, age_secs } => {
                serde_json::json!({
                    "provider": provider,
                    "age_secs": age_secs,
                })
            }
            EventKind::MendZeroActivityLogCleaned {
                worker_id,
                log_path,
            } => {
                serde_json::json!({
                    "worker_id": worker_id,
                    "log_path": log_path,
                })
            }
            EventKind::CanaryStarted { suite } => {
                serde_json::json!({ "suite": suite })
            }
            EventKind::CanarySuiteCompleted {
                suite,
                passed,
                failed,
            } => {
                serde_json::json!({
                    "suite": suite,
                    "passed": passed,
                    "failed": failed,
                })
            }
            EventKind::CanaryPromoted { hash } => {
                serde_json::json!({ "hash": hash })
            }
            EventKind::CanaryRejected { reason } => {
                serde_json::json!({ "reason": reason })
            }
            EventKind::ReflectStarted { beads_since_last } => {
                serde_json::json!({ "beads_since_last": beads_since_last })
            }
            EventKind::ReflectConsolidated {
                learnings_added,
                learnings_pruned,
                skills_promoted,
                beads_processed,
            } => {
                serde_json::json!({
                    "learnings_added": learnings_added,
                    "learnings_pruned": learnings_pruned,
                    "skills_promoted": skills_promoted,
                    "beads_processed": beads_processed,
                })
            }
            EventKind::ReflectSkipped { reason } => {
                serde_json::json!({ "reason": reason })
            }
            EventKind::ReflectTranscriptsRead {
                sessions_count,
                entries_count,
                parse_errors,
            } => {
                serde_json::json!({
                    "sessions_count": sessions_count,
                    "entries_count": entries_count,
                    "parse_errors": parse_errors,
                })
            }
            EventKind::ReflectDriftDetected {
                cluster_size,
                category,
                sessions,
            } => {
                serde_json::json!({
                    "cluster_size": cluster_size,
                    "category": category,
                    "sessions": sessions,
                })
            }
            EventKind::ReflectDriftPromoted { pattern, category } => {
                serde_json::json!({
                    "pattern": pattern,
                    "category": category,
                })
            }
            EventKind::ReflectDecisionExtracted {
                bead_id,
                has_alternatives,
                rationale_length,
            } => {
                serde_json::json!({
                    "bead_id": bead_id.to_string(),
                    "has_alternatives": has_alternatives,
                    "rationale_length": rationale_length,
                })
            }
            EventKind::ReflectAdrCreated { bead_id, path } => {
                serde_json::json!({
                    "bead_id": bead_id.to_string(),
                    "path": path,
                })
            }
            EventKind::ReflectLearningPromoted {
                learning_id,
                target_path,
                workspace_count,
                is_decision,
            } => {
                serde_json::json!({
                    "learning_id": learning_id,
                    "target_path": target_path,
                    "workspace_count": workspace_count,
                    "is_decision": is_decision,
                })
            }
            EventKind::ReflectLearningDeduplicated {
                learning_id,
                existing_entry,
            } => {
                serde_json::json!({
                    "learning_id": learning_id,
                    "existing_entry": existing_entry,
                })
            }
            EventKind::ReflectClaudeMdWritten {
                path,
                entries_added,
                entries_updated,
            } => {
                serde_json::json!({
                    "path": path,
                    "entries_added": entries_added,
                    "entries_updated": entries_updated,
                })
            }
            EventKind::DriftDetectionStarted { sessions_analyzed } => {
                serde_json::json!({ "sessions_analyzed": sessions_analyzed })
            }
            EventKind::DriftDetectionCompleted {
                sessions_analyzed,
                clusters_found,
                evolved_count,
                inconsistent_count,
            } => {
                serde_json::json!({
                    "sessions_analyzed": sessions_analyzed,
                    "clusters_found": clusters_found,
                    "evolved_count": evolved_count,
                    "inconsistent_count": inconsistent_count,
                })
            }
            EventKind::DriftDetectionSkipped { reason } => {
                serde_json::json!({ "reason": reason })
            }
            EventKind::DriftReportWritten {
                report_path,
                clusters,
            } => {
                serde_json::json!({
                    "report_path": report_path,
                    "clusters": clusters,
                })
            }
            EventKind::DecisionDetectionStarted { sessions_analyzed } => {
                serde_json::json!({ "sessions_analyzed": sessions_analyzed })
            }
            EventKind::DecisionDetectionCompleted {
                sessions_analyzed,
                decisions_found,
            } => {
                serde_json::json!({
                    "sessions_analyzed": sessions_analyzed,
                    "decisions_found": decisions_found,
                })
            }
            EventKind::DecisionDetectionSkipped { reason } => {
                serde_json::json!({ "reason": reason })
            }
            EventKind::SinkError { message } => serde_json::json!({ "message": message }),
            EventKind::OtlpDropped {
                signal,
                dropped_count,
                queue_full,
            } => serde_json::json!({
                "signal": signal,
                "dropped_count": dropped_count,
                "queue_full": queue_full,
            }),
            EventKind::OtlpShutdownTimeout {
                flushed_batches,
                remaining_batches,
            } => serde_json::json!({
                "flushed_batches": flushed_batches,
                "remaining_batches": remaining_batches,
            }),
            EventKind::IdleSleepCompleted {
                backoff_secs,
                elapsed_secs,
                shutdown_checks,
            } => serde_json::json!({
                "backoff_secs": backoff_secs,
                "elapsed_secs": elapsed_secs,
                "shutdown_checks": shutdown_checks,
            }),
            EventKind::IdleSleepEntered {
                backoff_secs,
                beads_processed,
                uptime_secs,
            } => serde_json::json!({
                "backoff_secs": backoff_secs,
                "beads_processed": beads_processed,
                "uptime_secs": uptime_secs,
            }),
        }
    }

    /// Extract duration_ms from events that carry it.
    pub fn duration_ms(&self) -> Option<u64> {
        match self {
            EventKind::DispatchCompleted { duration_ms, .. }
            | EventKind::BeadCompleted { duration_ms, .. }
            | EventKind::StrandEvaluated { duration_ms, .. }
            | EventKind::InitStepCompleted { duration_ms, .. }
            | EventKind::EffortRecorded {
                elapsed_ms: duration_ms,
                ..
            } => Some(*duration_ms),
            EventKind::WorkerBooting { .. }
            | EventKind::WorkerStarted { .. }
            | EventKind::WorkerStopped { .. }
            | EventKind::WorkerErrored { .. }
            | EventKind::WorkerExhausted { .. }
            | EventKind::WorkerIdle { .. }
            | EventKind::StateTransition { .. }
            | EventKind::InitStepStarted { .. }
            | EventKind::WorkerBootTimeout { .. }
            | EventKind::StrandSkipped { .. }
            | EventKind::QueueEmpty
            | EventKind::ClaimAttempt { .. }
            | EventKind::ClaimSuccess { .. }
            | EventKind::ClaimRaceLost { .. }
            | EventKind::ClaimRaceLostSkipped { .. }
            | EventKind::ClaimFailed { .. }
            | EventKind::BeadReleased { .. }
            | EventKind::BeadReleaseFailed { .. }
            | EventKind::BeadOrphaned { .. }
            | EventKind::DispatchStarted { .. }
            | EventKind::BuildTimeout { .. }
            | EventKind::BuildHeartbeat { .. }
            | EventKind::OutcomeClassified { .. }
            | EventKind::OutcomeHandled { .. }
            | EventKind::WorkerHandlingTimeout { .. }
            | EventKind::HeartbeatEmitted { .. }
            | EventKind::StuckDetected { .. }
            | EventKind::StuckReleased { .. }
            | EventKind::HealthCheck { .. }
            | EventKind::MendOrphanedLockRemoved { .. }
            | EventKind::MendDependencyCleaned { .. }
            | EventKind::MendDbRepaired { .. }
            | EventKind::MendDbRebuilt
            | EventKind::MendCycleSummary { .. }
            | EventKind::MendIdleWorkerFlagged { .. }
            | EventKind::MendWorkerDeregistered { .. }
            | EventKind::MendOrphanedHeartbeatRemoved { .. }
            | EventKind::MendDependencyRemoved { .. }
            | EventKind::MendBeadReleaseFailed { .. }
            | EventKind::MendDependencyCleanupFailed { .. }
            | EventKind::MendLockRemoveFailed { .. }
            | EventKind::MendRateLimitCleaned { .. }
            | EventKind::MendRateLimitProviderRemoved { .. }
            | EventKind::MendRateLimitProviderReset { .. }
            | EventKind::BudgetWarning { .. }
            | EventKind::BudgetStop { .. }
            | EventKind::RateLimitWait { .. }
            | EventKind::RateLimitAllowed { .. }
            | EventKind::MitosisEvaluated { .. }
            | EventKind::MitosisSplit { .. }
            | EventKind::MitosisSkipped { .. }
            | EventKind::VerificationFailed { .. }
            | EventKind::VerificationPassed { .. }
            | EventKind::UnravelAnalyzed { .. }
            | EventKind::UnravelSkipped { .. }
            | EventKind::PulseScannerStarted { .. }
            | EventKind::PulseScannerCompleted { .. }
            | EventKind::PulseScannerFailed { .. }
            | EventKind::PulseBeadCreated { .. }
            | EventKind::PulseSkipped { .. }
            | EventKind::UpgradeDetected { .. }
            | EventKind::UpgradeCompleted { .. }
            | EventKind::RollbackCompleted { .. }
            | EventKind::MendTraceCleanup { .. }
            | EventKind::MendLearningCleanup { .. }
            | EventKind::CanaryStarted { .. }
            | EventKind::CanarySuiteCompleted { .. }
            | EventKind::CanaryPromoted { .. }
            | EventKind::CanaryRejected { .. }
            | EventKind::OutputTransformSpawned { .. }
            | EventKind::OutputTransformExited { .. }
            | EventKind::OutputTransformSkipped { .. }
            | EventKind::TransformStarted { .. }
            | EventKind::TransformFailed { .. }
            | EventKind::TransformSkipped { .. }
            | EventKind::ReflectStarted { .. }
            | EventKind::ReflectConsolidated { .. }
            | EventKind::ReflectSkipped { .. }
            | EventKind::ReflectTranscriptsRead { .. }
            | EventKind::ReflectDriftDetected { .. }
            | EventKind::ReflectDriftPromoted { .. }
            | EventKind::ReflectDecisionExtracted { .. }
            | EventKind::ReflectAdrCreated { .. }
            | EventKind::ReflectLearningPromoted { .. }
            | EventKind::ReflectLearningDeduplicated { .. }
            | EventKind::ReflectClaudeMdWritten { .. }
            | EventKind::MendZeroActivityLogCleaned { .. }
            | EventKind::DriftDetectionStarted { .. }
            | EventKind::DriftDetectionCompleted { .. }
            | EventKind::DriftDetectionSkipped { .. }
            | EventKind::DriftReportWritten { .. }
            | EventKind::DecisionDetectionStarted { .. }
            | EventKind::DecisionDetectionCompleted { .. }
            | EventKind::DecisionDetectionSkipped { .. }
            | EventKind::SinkError { .. }
            | EventKind::OtlpDropped { .. }
            | EventKind::OtlpShutdownTimeout { .. }
            | EventKind::IdleSleepCompleted { .. }
            | EventKind::IdleSleepEntered { .. } => None,
            EventKind::TransformCompleted { duration_ms, .. } => Some(*duration_ms),
        }
    }
}

// ─── Sink trait ──────────────────────────────────────────────────────────────

/// Pluggable output backend for telemetry events.
///
/// Implement this trait to add a new sink — register an instance in the
/// `TelemetryBus` and events fan out automatically, with no special-casing.
pub trait Sink: Send + Sync {
    /// Accept a single event. Must not block indefinitely.
    fn accept(&self, event: &TelemetryEvent) -> Result<()>;

    /// Flush any buffered state before shutdown.
    ///
    /// Implementations should respect `deadline`: if flushing cannot complete
    /// within the duration, return an error rather than blocking indefinitely.
    fn flush(&self, deadline: std::time::Duration) -> Result<()>;
}

/// Blanket impl for Arc-wrapped sinks.
/// This enables Arc<Sink> to be used wherever Sink is required.
impl<T: Sink + ?Sized> Sink for Arc<T> {
    fn accept(&self, event: &TelemetryEvent) -> Result<()> {
        self.as_ref().accept(event)
    }

    fn flush(&self, deadline: std::time::Duration) -> Result<()> {
        self.as_ref().flush(deadline)
    }
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

    /// Write a boot event directly to the file, bypassing the normal channel.
    ///
    /// This is called immediately after FileSink creation to ensure that even if
    /// the writer thread fails to start, we have a trace in the JSONL file.
    /// The event is written synchronously and flushed to disk.
    fn write_boot_event_direct(
        &self,
        worker_id: &str,
        session_id: &str,
        version: &str,
    ) -> Result<()> {
        Self::write_boot_event_direct_impl(
            &self.writer,
            &self.path,
            worker_id,
            session_id,
            version,
            std::time::Duration::from_secs(5), // 5 second timeout
        )
    }

    /// Write a boot event directly to the file with a timeout.
    ///
    /// This is a timeout-aware variant that prevents indefinite blocking on hung
    /// filesystems (e.g., network filesystem issues, stale NFS mounts). If the
    /// write takes longer than the timeout, it returns an error and the caller
    /// can decide whether to continue or fail.
    ///
    /// The timeout is implemented by spawning a thread to do the blocking I/O
    /// and joining with a timeout. If the timeout expires, the thread is detached
    /// and will continue running (and eventually complete or be killed by the OS).
    fn write_boot_event_direct_impl(
        _writer: &std::sync::Mutex<std::io::BufWriter<std::fs::File>>,
        path: &Path,
        worker_id: &str,
        session_id: &str,
        version: &str,
        timeout: std::time::Duration,
    ) -> Result<()> {
        use std::io::Write;
        use std::thread;

        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "worker.booting".to_string(),
            worker_id: worker_id.to_string(),
            session_id: session_id.to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            duration_ms: None,
            data: serde_json::json!({ "worker_name": worker_id, "version": version }),
            trace_id: None,
            span_id: None,
        };
        let line = serde_json::to_string(&event)?;
        let path_for_error = path.display().to_string();
        let path_clone = path.to_path_buf();

        // Spawn a thread to do the blocking I/O
        let handle = thread::spawn(move || {
            // This runs in a separate thread, so if it blocks, it won't block the main process
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .append(true)
                .open(&path_clone)?;
            let mut writer = std::io::BufWriter::new(file);
            writeln!(writer, "{line}")?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            Ok::<(), anyhow::Error>(())
        });

        // Join with timeout
        handle
            .join()
            .map_err(|e| anyhow::anyhow!("boot event writer thread panicked: {:?}", e))?
            .with_context(|| {
                format!(
                    "timed out writing boot event to {} after {:?} (filesystem may be hung)",
                    path_for_error, timeout
                )
            })
    }
}

impl Sink for FileSink {
    fn accept(&self, event: &TelemetryEvent) -> Result<()> {
        use std::io::Write;
        let line = serde_json::to_string(event)?;
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {}", e))?;
        writeln!(writer, "{line}")?;
        writer.flush()?;
        Ok(())
    }

    fn flush(&self, _deadline: std::time::Duration) -> Result<()> {
        use std::io::Write;
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {}", e))?;
        writer.flush()?;
        // fsync to ensure durability before shutdown
        writer.get_ref().sync_all()?;
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
            "bead.claim.race_lost_skipped" => "RACE_LOST_SKIP",
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
                || t.starts_with("bead.claim.race_lost_skipped")
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

impl Sink for StdoutSink {
    fn accept(&self, event: &TelemetryEvent) -> Result<()> {
        let line = self.format_event(event);
        println!("{line}");
        Ok(())
    }

    fn flush(&self, _deadline: std::time::Duration) -> Result<()> {
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

// ─── Trace context helpers ────────────────────────────────────────────────────

/// Returns `(trace_id_hex, span_id_hex)` from the current OTel span context,
/// or `(None, None)` when not inside a valid span.
#[cfg(feature = "otlp")]
fn current_trace_ids() -> (Option<String>, Option<String>) {
    use opentelemetry::trace::TraceContextExt;
    let ctx = opentelemetry::Context::current();
    let binding = ctx.span();
    let span_ctx = binding.span_context();
    if span_ctx.is_valid() {
        (
            Some(hex::encode(span_ctx.trace_id().to_bytes())),
            Some(hex::encode(span_ctx.span_id().to_bytes())),
        )
    } else {
        (None, None)
    }
}

#[cfg(not(feature = "otlp"))]
fn current_trace_ids() -> (Option<String>, Option<String>) {
    (None, None)
}

// ─── HookSink ─────────────────────────────────────────────────────────────────

/// A compiled hook: a pre-compiled regex filter + dispatch target(s).
struct CompiledHook {
    filter: regex::Regex,
    /// Shell command (empty = disabled).
    command: String,
    /// Webhook URL (None = disabled).
    url: Option<String>,
}

/// Dispatches matching telemetry events to external commands and/or URLs.
///
/// Each hook has a glob pattern matched against `event_type`. When an event
/// matches, the event JSON is piped to `command`'s stdin and/or HTTP-POSTed
/// to `url`. Execution is fire-and-forget — failed hooks emit `SinkError`
/// events to the file sink but never block the worker or recurse into hooks.
pub struct HookSink {
    hooks: Vec<CompiledHook>,
}

impl HookSink {
    /// Compile hook configs into a `HookSink`.
    ///
    /// Returns an error if any `event_filter` is an invalid glob pattern.
    pub fn new(configs: &[crate::config::HookConfig]) -> Result<Self> {
        let mut hooks = Vec::with_capacity(configs.len());
        for cfg in configs {
            let filter = glob_to_regex(&cfg.event_filter)
                .with_context(|| format!("invalid hook filter: {}", cfg.event_filter))?;
            hooks.push(CompiledHook {
                filter,
                command: cfg.command.clone(),
                url: cfg.url.clone(),
            });
        }
        Ok(HookSink { hooks })
    }

    /// Check whether any hooks are configured.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Dispatch an event to all matching hooks (fire-and-forget).
    ///
    /// Returns a list of `SinkError` events for hooks that failed, so the
    /// caller can write them to the file sink without recursion.
    pub fn dispatch(&self, event: &TelemetryEvent) -> Vec<TelemetryEvent> {
        // Never dispatch SinkError events to hooks — prevents recursion.
        if event.event_type == "telemetry.sink_error" {
            return Vec::new();
        }

        let json = match serde_json::to_string(event) {
            Ok(j) => j,
            Err(_) => return Vec::new(),
        };

        let mut failures = Vec::new();
        for hook in &self.hooks {
            if !hook.filter.is_match(&event.event_type) {
                continue;
            }

            // Dispatch to command if configured.
            if !hook.command.is_empty() {
                match Self::run_hook(&hook.command, &json) {
                    Ok(()) => {}
                    Err(e) => {
                        failures.push(TelemetryEvent {
                            timestamp: Utc::now(),
                            event_type: "telemetry.sink_error".to_string(),
                            worker_id: event.worker_id.clone(),
                            session_id: event.session_id.clone(),
                            sequence: 0, // sequence is set by the emitter, not here
                            bead_id: None,
                            workspace: None,
                            data: serde_json::json!({
                                "hook_command": hook.command,
                                "event_filter": hook.filter.as_str(),
                                "original_event_type": event.event_type,
                                "error": e.to_string(),
                            }),
                            duration_ms: None,
                            trace_id: None,
                            span_id: None,
                        });
                    }
                }
            }

            // Dispatch to URL if configured.
            if let Some(ref url) = hook.url {
                match Self::post_url(url, &json) {
                    Ok(()) => {}
                    Err(e) => {
                        failures.push(TelemetryEvent {
                            timestamp: Utc::now(),
                            event_type: "telemetry.sink_error".to_string(),
                            worker_id: event.worker_id.clone(),
                            session_id: event.session_id.clone(),
                            sequence: 0,
                            bead_id: None,
                            workspace: None,
                            data: serde_json::json!({
                                "hook_url": url,
                                "event_filter": hook.filter.as_str(),
                                "original_event_type": event.event_type,
                                "error": e.to_string(),
                            }),
                            duration_ms: None,
                            trace_id: None,
                            span_id: None,
                        });
                    }
                }
            }
        }
        failures
    }

    /// Execute a single hook command, piping JSON to its stdin.
    ///
    /// Spawns `sh -c <command>` with the event JSON on stdin, waits for
    /// completion, and returns an error if the command exits non-zero.
    /// This runs inside the background writer task so blocking is acceptable.
    fn run_hook(command: &str, json: &str) -> Result<()> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn hook: {command}"))?;

        if let Some(mut stdin) = child.stdin.take() {
            // Best-effort write; ignore broken pipe if child exits early.
            let _ = stdin.write_all(json.as_bytes());
        }

        // Wait for the child and check exit status.
        let status = child
            .wait()
            .with_context(|| format!("failed to wait for hook: {command}"))?;

        if !status.success() {
            anyhow::bail!(
                "hook exited with status {}: {command}",
                status.code().unwrap_or(-1)
            );
        }

        Ok(())
    }

    /// HTTP POST the event JSON to a webhook URL.
    ///
    /// Sets `Content-Type: application/json` and applies a 10-second timeout.
    /// Returns an error if the request fails or the server returns a non-2xx
    /// status. This runs inside the background writer task so blocking is
    /// acceptable.
    fn post_url(url: &str, json: &str) -> Result<()> {
        ureq::post(url)
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(10))
            .send_string(json)
            .map_err(|e| anyhow::anyhow!("webhook POST to {url} failed: {e}"))?;
        Ok(())
    }
}

impl Sink for HookSink {
    fn accept(&self, event: &TelemetryEvent) -> Result<()> {
        let failures = self.dispatch(event);
        for fail in &failures {
            tracing::warn!(
                hook_error = %fail.data,
                original_event = %event.event_type,
                "telemetry hook dispatch failed"
            );
        }
        Ok(())
    }

    fn flush(&self, _deadline: std::time::Duration) -> Result<()> {
        // hooks run synchronously in accept(); nothing to drain
        Ok(())
    }
}

// ─── Telemetry emitter ───────────────────────────────────────────────────────

/// Non-blocking telemetry emitter.
///
/// Cloning a `Telemetry` handle is cheap — it shares the same background
/// writer and sequence counter.
///
/// The writer task must be started by calling `start()` from within a tokio
/// runtime context. This is typically done at the beginning of `Worker::run()`.
#[derive(Clone)]
pub struct Telemetry {
    worker_id: WorkerId,
    session_id: String,
    sequence: Arc<AtomicU64>,
    /// Wrapped in Arc<Mutex<Option<...>>> so that `shutdown()` can drop the
    /// sender explicitly (closing the channel) even while clones still exist.
    sender: Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<WriterMessage>>>>,
    /// Pending writer state that needs to be spawned in an async context.
    /// This is `Some` until `start()` is called.
    pending_writer: Arc<std::sync::Mutex<Option<PendingWriter>>>,
    /// JoinHandle for the background writer thread, set by `start()`.
    /// Uses std::thread::JoinHandle (not tokio::task::JoinHandle) because the
    /// writer runs in its own dedicated thread with its own tokio runtime,
    /// ensuring it can process messages before the main runtime's block_on().
    writer_handle: Arc<std::sync::Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// OTLP sink shutdown handle (feature-gated).
    /// Stored separately so we can call its async shutdown method.
    otlp_shutdown: Arc<std::sync::Mutex<Option<OtlpShutdown>>>,
}

/// Internal message type for the writer task.
#[allow(clippy::large_enum_variant)]
enum WriterMessage {
    Event(TelemetryEvent),
    Flush(tokio::sync::oneshot::Sender<()>),
}

/// Internal message type for the writer task.
#[allow(clippy::large_enum_variant)]
enum WriterMessage {
    Event(TelemetryEvent),
    Flush(tokio::sync::oneshot::Sender<()>),
}

/// Holds the receiver and sinks until they can be spawned in an async context.
struct PendingWriter {
    receiver: mpsc::UnboundedReceiver<WriterMessage>,
    sinks: Vec<Box<dyn Sink>>,
}

/// Holds an OTLP sink for async shutdown (feature-gated).
enum OtlpShutdown {
    #[cfg(feature = "otlp")]
    Sink(std::sync::Arc<crate::telemetry::OtlpSink>),
    #[cfg(not(feature = "otlp"))]
    None,
}

impl OtlpShutdown {
    /// Shutdown the OTLP sink gracefully.
    ///
    /// This drains all batched exports before returning.
    #[cfg(feature = "otlp")]
    async fn shutdown(self) -> Result<()> {
        match self {
            OtlpShutdown::Sink(sink) => {
                // We need to extract the OtlpSink from Arc to call shutdown
                // Try to unwrap the Arc - if there are other references, we'll force_flush instead
                match std::sync::Arc::try_unwrap(sink) {
                    Ok(otlp) => otlp.shutdown().await,
                    Err(_) => {
                        tracing::warn!("Cannot shutdown OTLP sink: Arc has multiple references");
                        Ok(())
                    }
                }
            }
        }
    }

    /// No-op shutdown when OTLP feature is disabled.
    #[cfg(not(feature = "otlp"))]
    async fn shutdown(self) -> Result<()> {
        Ok(())
    }
}

impl Telemetry {
    /// Create a telemetry emitter that writes to a `FileSink`.
    ///
    /// Does not spawn any async tasks. Call [`start()`](Self::start) from
    /// within a tokio runtime context before emitting events.
    ///
    /// Writes `worker.booting` directly to the file before returning, so even
    /// if the writer thread fails to start, we have a trace in the JSONL log.
    pub fn new(worker_id: WorkerId) -> Self {
        let session_id = generate_session_id();
        let (sender, receiver) = mpsc::unbounded_channel();

        let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
        match FileSink::new(&worker_id, &session_id) {
            Ok(s) => {
                // Write boot event directly to file BEFORE spawning writer thread.
                // Uses a 5-second timeout to prevent indefinite blocking on hung filesystems.
                // This ensures we have a trace even if the writer thread fails to start.
                let version = env!("CARGO_PKG_VERSION");
                if let Err(e) = s.write_boot_event_direct(&worker_id, &session_id, version) {
                    // Check if this is a timeout error (filesystem may be hung)
                    let error_msg = e.to_string();
                    if error_msg.contains("timed out")
                        || error_msg.contains("filesystem may be hung")
                    {
                        eprintln!(
                            "NEEDLE WARNING: boot event write timed out after 5s - filesystem may be hung or very slow"
                        );
                        eprintln!("  Continuing without boot event in log file. Worker will still function.");
                        eprintln!(
                            "  Check: disk space, NFS mounts, filesystem latency, I/O errors"
                        );
                    }
                    tracing::warn!(error = %e, "failed to write boot event directly to file");
                }
                sinks.push(Box::new(s));
            }
            Err(e) => tracing::warn!(error = %e, "failed to create telemetry file sink"),
        }

        let sequence = Arc::new(AtomicU64::new(0));
        let pending = PendingWriter { receiver, sinks };

        Telemetry {
            worker_id,
            session_id,
            sequence,
            sender: Arc::new(std::sync::Mutex::new(Some(sender))),
            pending_writer: Arc::new(std::sync::Mutex::new(Some(pending))),
            writer_handle: Arc::new(std::sync::Mutex::new(None)),
            otlp_shutdown: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Create a telemetry emitter with both file and stdout sinks.
    ///
    /// Does not spawn any async tasks. Call [`start()`](Self::start) from
    /// within a tokio runtime context before emitting events.
    ///
    /// Writes `worker.booting` directly to the file before returning, so even
    /// if the writer thread fails to start, we have a trace in the JSONL log.
    pub fn with_stdout(worker_id: WorkerId, stdout_config: &StdoutSinkConfig) -> Self {
        let session_id = generate_session_id();
        let (sender, receiver) = mpsc::unbounded_channel();

        let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
        match FileSink::new(&worker_id, &session_id) {
            Ok(s) => {
                // Write boot event directly to file BEFORE spawning writer thread.
                // Uses a 5-second timeout to prevent indefinite blocking on hung filesystems.
                let version = env!("CARGO_PKG_VERSION");
                if let Err(e) = s.write_boot_event_direct(&worker_id, &session_id, version) {
                    // Check if this is a timeout error (filesystem may be hung)
                    let error_msg = e.to_string();
                    if error_msg.contains("timed out")
                        || error_msg.contains("filesystem may be hung")
                    {
                        eprintln!(
                            "NEEDLE WARNING: boot event write timed out after 5s - filesystem may be hung or very slow"
                        );
                        eprintln!("  Continuing without boot event in log file. Worker will still function.");
                        eprintln!(
                            "  Check: disk space, NFS mounts, filesystem latency, I/O errors"
                        );
                    }
                    tracing::warn!(error = %e, "failed to write boot event directly to file");
                }
                sinks.push(Box::new(s));
            }
            Err(e) => tracing::warn!(error = %e, "failed to create telemetry file sink"),
        }
        if stdout_config.enabled {
            sinks.push(Box::new(StdoutSink::new(stdout_config)));
        }

        let sequence = Arc::new(AtomicU64::new(0));
        let pending = PendingWriter { receiver, sinks };

        Telemetry {
            worker_id,
            session_id,
            sequence,
            sender: Arc::new(std::sync::Mutex::new(Some(sender))),
            pending_writer: Arc::new(std::sync::Mutex::new(Some(pending))),
            writer_handle: Arc::new(std::sync::Mutex::new(None)),
            otlp_shutdown: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Create a telemetry emitter with file, stdout, and hook sinks.
    ///
    /// Does not spawn any async tasks. Call [`start()`](Self::start) from
    /// within a tokio runtime context before emitting events.
    ///
    /// Writes `worker.booting` directly to the file before returning, so even
    /// if the writer thread fails to start, we have a trace in the JSONL log.
    pub fn with_hooks(
        worker_id: WorkerId,
        stdout_config: &StdoutSinkConfig,
        hook_configs: &[HookConfig],
    ) -> Result<Self> {
        let session_id = generate_session_id();
        let (sender, receiver) = mpsc::unbounded_channel();

        let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
        match FileSink::new(&worker_id, &session_id) {
            Ok(s) => {
                // Write boot event directly to file BEFORE spawning writer thread.
                // Uses a 5-second timeout to prevent indefinite blocking on hung filesystems.
                let version = env!("CARGO_PKG_VERSION");
                if let Err(e) = s.write_boot_event_direct(&worker_id, &session_id, version) {
                    // Check if this is a timeout error (filesystem may be hung)
                    let error_msg = e.to_string();
                    if error_msg.contains("timed out")
                        || error_msg.contains("filesystem may be hung")
                    {
                        eprintln!(
                            "NEEDLE WARNING: boot event write timed out after 5s - filesystem may be hung or very slow"
                        );
                        eprintln!("  Continuing without boot event in log file. Worker will still function.");
                        eprintln!(
                            "  Check: disk space, NFS mounts, filesystem latency, I/O errors"
                        );
                    }
                    tracing::warn!(error = %e, "failed to write boot event directly to file");
                }
                sinks.push(Box::new(s));
            }
            Err(e) => tracing::warn!(error = %e, "failed to create telemetry file sink"),
        }
        if stdout_config.enabled {
            sinks.push(Box::new(StdoutSink::new(stdout_config)));
        }
        if !hook_configs.is_empty() {
            sinks.push(Box::new(HookSink::new(hook_configs)?));
        }

        let sequence = Arc::new(AtomicU64::new(0));
        let pending = PendingWriter { receiver, sinks };

        Ok(Telemetry {
            worker_id,
            session_id,
            sequence,
            sender: Arc::new(std::sync::Mutex::new(Some(sender))),
            pending_writer: Arc::new(std::sync::Mutex::new(Some(pending))),
            writer_handle: Arc::new(std::sync::Mutex::new(None)),
            otlp_shutdown: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    /// Create a telemetry emitter from a `TelemetryConfig`.
    ///
    /// Selects the right constructor based on configured sinks:
    /// - OTLP enabled       → creates OTLP sink with async shutdown
    /// - Hooks configured   → [`with_hooks`](Self::with_hooks)
    /// - Stdout enabled     → [`with_stdout`](Self::with_stdout)
    /// - Otherwise          → [`new`](Self::new) (file sink only)
    pub fn from_config(worker_id: WorkerId, config: &TelemetryConfig) -> Result<Self> {
        let session_id = generate_session_id();
        let (sender, receiver) = mpsc::unbounded_channel();

        let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
        let mut otlp_shutdown = None;
        let mut file_sink: Option<Arc<FileSink>> = None;

        // File sink is always created (fallback)
        match FileSink::new(&worker_id, &session_id) {
            Ok(s) => {
                // Write boot event directly to file BEFORE spawning writer thread.
                // Uses a 5-second timeout to prevent indefinite blocking on hung filesystems.
                let version = env!("CARGO_PKG_VERSION");
                if let Err(e) = s.write_boot_event_direct(&worker_id, &session_id, version) {
                    // Check if this is a timeout error (filesystem may be hung)
                    let error_msg = e.to_string();
                    if error_msg.contains("timed out")
                        || error_msg.contains("filesystem may be hung")
                    {
                        eprintln!(
                            "NEEDLE WARNING: boot event write timed out after 5s - filesystem may be hung or very slow"
                        );
                        eprintln!("  Continuing without boot event in log file. Worker will still function.");
                        eprintln!(
                            "  Check: disk space, NFS mounts, filesystem latency, I/O errors"
                        );
                    }
                    tracing::warn!(error = %e, "failed to write boot event directly to file");
                }
                file_sink = Some(Arc::new(s));
                // Arc<FileSink> implements Sink via the blanket impl
                if let Some(ref fs) = file_sink {
                    sinks.push(Box::new(Arc::clone(fs)));
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to create telemetry file sink"),
        }

        // OTLP sink (feature-gated)
        #[cfg(feature = "otlp")]
        {
            if config.otlp_sink.enabled {
                // Convert Arc<FileSink> to Option<Box<dyn Sink>> for OtlpSink
                let file_sink_for_otlp = file_sink
                    .as_ref()
                    .map(|fs| Box::new(Arc::clone(fs)) as Box<dyn Sink>);

                match OtlpSink::new(
                    worker_id.clone(),
                    session_id.clone(),
                    &config.otlp_sink,
                    file_sink_for_otlp,
                    None, // agent - not available at config time
                    None, // model - not available at config time
                    None, // workspace - not available at config time
                ) {
                    Ok(otlp) => {
                        let otlp_arc = Arc::new(otlp);
                        sinks.push(Box::new(Arc::clone(&otlp_arc)));
                        otlp_shutdown = Some(OtlpShutdown::Sink(otlp_arc));
                        tracing::info!("OTLP sink initialized: {}", config.otlp_sink.endpoint);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to create OTLP sink, continuing without it");
                    }
                }
            }
        }

        // Stdout sink
        if config.stdout_sink.enabled {
            sinks.push(Box::new(StdoutSink::new(&config.stdout_sink)));
        }

        // Hook sinks
        if !config.hooks.is_empty() {
            sinks.push(Box::new(HookSink::new(&config.hooks)?));
        }

        let sequence = Arc::new(AtomicU64::new(0));
        let pending = PendingWriter { receiver, sinks };

        Ok(Telemetry {
            worker_id,
            session_id,
            sequence,
            sender: Arc::new(std::sync::Mutex::new(Some(sender))),
            pending_writer: Arc::new(std::sync::Mutex::new(Some(pending))),
            writer_handle: Arc::new(std::sync::Mutex::new(None)),
            otlp_shutdown: Arc::new(std::sync::Mutex::new(otlp_shutdown)),
        })
    }

    /// Create a telemetry emitter backed by a real `FileSink` in a custom
    /// directory (for testing the `start()`/`shutdown()` lifecycle).
    ///
    /// Returns `(Telemetry, log_file_path)`. Call `start()` from inside a
    /// tokio runtime, emit events, then `shutdown().await` — the BufWriter
    /// will be flushed before `shutdown()` returns.
    #[cfg(test)]
    pub fn with_log_dir_and_path(
        worker_id: WorkerId,
        log_dir: &std::path::Path,
    ) -> Result<(Self, std::path::PathBuf)> {
        // Fixed session ID so the caller knows the exact file path.
        let session_id = "testbeef".to_string();
        let (sender, receiver) = mpsc::unbounded_channel();
        let file_sink = FileSink::with_dir(log_dir, &worker_id, &session_id)?;
        let path = file_sink.path().to_path_buf();
        // Write boot event directly to file (for consistency with production code).
        let version = env!("CARGO_PKG_VERSION");
        let _ = file_sink.write_boot_event_direct(&worker_id, &session_id, version);
        let sequence = Arc::new(AtomicU64::new(0));
        let sinks: Vec<Box<dyn Sink>> = vec![Box::new(file_sink)];
        let pending = PendingWriter { receiver, sinks };
        Ok((
            Telemetry {
                worker_id,
                session_id,
                sequence,
                sender: Arc::new(std::sync::Mutex::new(Some(sender))),
                pending_writer: Arc::new(std::sync::Mutex::new(Some(pending))),
                writer_handle: Arc::new(std::sync::Mutex::new(None)),
                otlp_shutdown: Arc::new(std::sync::Mutex::new(None)),
            },
            path,
        ))
    }

    /// Create a telemetry emitter backed by a pre-built list of sinks (for testing).
    #[cfg(any(test, feature = "integration"))]
    pub fn with_boxed_sinks(worker_id: WorkerId, sinks: Vec<Box<dyn Sink>>) -> Self {
        let session_id = "test0000".to_string();
        let (sender, receiver) = mpsc::unbounded_channel();
        let sequence = Arc::new(AtomicU64::new(0));
        let pending = PendingWriter { receiver, sinks };
        Telemetry {
            worker_id,
            session_id,
            sequence,
            sender: Arc::new(std::sync::Mutex::new(Some(sender))),
            pending_writer: Arc::new(std::sync::Mutex::new(Some(pending))),
            writer_handle: Arc::new(std::sync::Mutex::new(None)),
            otlp_shutdown: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Create a telemetry emitter with a custom sink (for testing).
    #[cfg(any(test, feature = "integration"))]
    pub fn with_sink(worker_id: WorkerId, sink: impl Sink + 'static) -> Self {
        let session_id = "test0000".to_string();
        let (sender, receiver) = mpsc::unbounded_channel::<WriterMessage>();
        let sequence = Arc::new(AtomicU64::new(0));

        tokio::spawn(async move {
            let mut rx = receiver;
            while let Some(msg) = rx.recv().await {
                if let WriterMessage::Event(event) = msg {
                    if let Err(e) = sink.accept(&event) {
                        tracing::warn!(error = %e, "test sink accept failed");
                    }
                }
            }
        });

        Telemetry {
            worker_id,
            session_id,
            sequence,
            sender: Arc::new(std::sync::Mutex::new(Some(sender))),
            pending_writer: Arc::new(std::sync::Mutex::new(None)),
            writer_handle: Arc::new(std::sync::Mutex::new(None)),
            otlp_shutdown: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Emit an event. Non-blocking — returns immediately.
    ///
    /// Returns `Err` only if the channel is disconnected (background task died).
    pub fn emit(&self, kind: EventKind) -> Result<()> {
        let seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let (trace_id, span_id) = current_trace_ids();
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
            trace_id,
            span_id,
        };
        tracing::debug!(event_type = %event.event_type, seq, "telemetry event");
        // Use try_lock() to avoid blocking indefinitely if the telemetry writer
        // is stuck holding the lock. If the lock is contended, we log and return
        // Ok(()) to allow the worker to continue.
        match self.sender.try_lock() {
            Ok(guard) => {
                if let Some(ref s) = *guard {
                    s.send(WriterMessage::Event(event)).ok(); // ok() — never block, never panic
                }
                Ok(())
            }
            Err(_) => {
                tracing::warn!(
                    event_type = %event.event_type,
                    seq,
                    "telemetry sender lock contended, skipping emit"
                );
                Ok(())
            }
        }
    }

    /// Emit an event without blocking — uses try_lock() instead of lock().
    ///
    /// Returns `Ok(())` if emitted successfully or if the lock is contended
    /// (gracefully degrades). Returns `Err` only if the channel is disconnected.
    ///
    /// Use this in timeout recovery paths where blocking on emit() would
    /// prevent the worker from recovering.
    pub fn emit_try_lock(&self, kind: EventKind) -> Result<()> {
        let seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let (trace_id, span_id) = current_trace_ids();
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
            trace_id,
            span_id,
        };
        // Use try_lock() to avoid blocking indefinitely if the telemetry writer
        // is stuck holding the lock. If the lock is contended, we log and return
        // Ok(()) to allow the worker to continue.
        match self.sender.try_lock() {
            Ok(guard) => {
                if let Some(ref s) = *guard {
                    s.send(WriterMessage::Event(event)).ok();
                }
                Ok(())
            }
            Err(_) => {
                tracing::warn!(
                    event_type = %event.event_type,
                    seq,
                    "telemetry sender lock contended, skipping emit in timeout recovery path"
                );
                Ok(())
            }
        }
    }

    /// Force-flush all buffered events to disk (synchronous).
    ///
    /// Sends a flush request through the writer channel and waits (up to
    /// `timeout`) for the writer task to flush its BufWriter. Used after
    /// `worker.booting` so the event is visible on disk even if subsequent
    /// init steps block.
    ///
    /// NOTE: This uses `blocking_recv()` and must be called from outside a
    /// tokio runtime (e.g., in `run_worker` before `rt.block_on`). Use
    /// `force_flush_async()` from within async contexts.
    pub fn force_flush(&self, timeout: std::time::Duration) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        // Use try_lock() to avoid blocking indefinitely if the telemetry writer
        // is stuck holding the lock.
        match self.sender.try_lock() {
            Ok(guard) => {
                if let Some(ref s) = *guard {
                    s.send(WriterMessage::Flush(tx)).ok();
                }
            }
            Err(_) => {
                tracing::warn!("telemetry sender lock contended in force_flush, skipping flush");
                return Ok(()); // Don't fail — the writer will eventually flush
            }
        }
        // Block until the writer task flushes or timeout.
        // Spawn a thread to handle the blocking recv with a timeout.
        let handle = std::thread::spawn(move || rx.blocking_recv());
        let start = std::time::Instant::now();
        loop {
            if handle.is_finished() {
                return Ok(());
            }
            if start.elapsed() >= timeout {
                tracing::warn!("force_flush timed out after {:?}", timeout);
                return Ok(()); // Don't fail — the writer will eventually flush
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Force-flush all buffered events to disk (async).
    ///
    /// Async version of `force_flush()` that can be called from within a
    /// tokio runtime. Use this in `Worker::run()` and other async contexts.
    pub async fn force_flush_async(&self, timeout: std::time::Duration) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        // Use try_lock() to avoid blocking indefinitely if the telemetry writer
        // is stuck holding the lock.
        match self.sender.try_lock() {
            Ok(guard) => {
                if let Some(ref s) = *guard {
                    s.send(WriterMessage::Flush(tx)).ok();
                }
            }
            Err(_) => {
                tracing::warn!(
                    "telemetry sender lock contended in force_flush_async, skipping flush"
                );
                return Ok(()); // Don't fail — the writer will eventually flush
            }
        }
        // Wait for the writer task to flush, with timeout.
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Ok(()), // Channel closed, writer already flushed
            Err(_) => {
                tracing::warn!("force_flush_async timed out after {:?}", timeout);
                Ok(())
            }
        }
    }

    /// Emit an event synchronously, writing directly to the file sink.
    ///
    /// This bypasses the async channel and writes immediately to the file.
    /// Use this for critical early-boot events (like `worker.booting`) that
    /// must be visible even if the async writer hasn't started yet.
    ///
    /// Returns `Err` if the pending writer is not available (already started)
    /// or if writing to the file fails.
    pub fn emit_sync(&self, kind: EventKind) -> Result<()> {
        let seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let (trace_id, span_id) = current_trace_ids();
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
            trace_id,
            span_id,
        };

        // Take the pending writer temporarily to write directly to sinks.
        let pending_guard = self.pending_writer.lock().unwrap();
        if let Some(ref pending) = *pending_guard {
            // Write directly to each sink (typically just FileSink).
            for sink in &pending.sinks {
                if let Err(e) = sink.accept(&event) {
                    tracing::warn!(error = %e, "sync emit to sink failed");
                }
            }
            Ok(())
        } else {
            // Pending writer already consumed by start().
            Err(anyhow::anyhow!("cannot emit_sync after start()"))
        }
    }

    /// Force-flush all buffered events to disk (synchronous).
    ///
    /// Sends a flush request through the writer channel and waits (up to
    /// `timeout`) for the writer task to flush its BufWriter. Used after
    /// `worker.booting` so the event is visible on disk even if subsequent
    /// init steps block.
    ///
    /// NOTE: This uses `blocking_recv()` and must be called from outside a
    /// tokio runtime (e.g., in `run_worker` before `rt.block_on`). Use
    /// `force_flush_async()` from within async contexts.
    pub fn force_flush(&self, timeout: std::time::Duration) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if let Some(ref s) = *self.sender.lock().unwrap() {
            s.send(WriterMessage::Flush(tx)).ok();
        }
        // Block until the writer task flushes or timeout.
        // Use a small thread to avoid needing an async context.
        let _ = rx.blocking_recv();
        let _ = timeout; // deadline enforced by the writer task
        Ok(())
    }

    /// Force-flush all buffered events to disk (async).
    ///
    /// Async version of `force_flush()` that can be called from within a
    /// tokio runtime. Use this in `Worker::run()` and other async contexts.
    pub async fn force_flush_async(&self, timeout: std::time::Duration) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if let Some(ref s) = *self.sender.lock().unwrap() {
            s.send(WriterMessage::Flush(tx)).ok();
        }
        // Wait for the writer task to flush, with timeout.
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Ok(()), // Channel closed, writer already flushed
            Err(_) => {
                tracing::warn!("force_flush_async timed out after {:?}", timeout);
                Ok(())
            }
        }
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
    /// Use this when the config specifies a custom log path. Does not spawn
    /// any async tasks. Call [`start()`](Self::start) from within a tokio
    /// runtime context before emitting events.
    pub fn with_log_dir(worker_id: WorkerId, log_dir: &Path) -> Self {
        let session_id = generate_session_id();
        let (sender, receiver) = mpsc::unbounded_channel();

        let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
        match FileSink::with_dir(log_dir, &worker_id, &session_id) {
            Ok(s) => sinks.push(Box::new(s)),
            Err(e) => tracing::warn!(error = %e, "failed to create telemetry file sink"),
        }

        let sequence = Arc::new(AtomicU64::new(0));
        let pending = PendingWriter { receiver, sinks };

        Telemetry {
            worker_id,
            session_id,
            sequence,
            sender: Arc::new(std::sync::Mutex::new(Some(sender))),
            pending_writer: Arc::new(std::sync::Mutex::new(Some(pending))),
            writer_handle: Arc::new(std::sync::Mutex::new(None)),
            otlp_shutdown: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Start the background writer task.
    ///
    /// Must be called from within a tokio runtime context (e.g. inside
    /// `block_on` or an async function). Calling this more than once on
    /// the same handle is a no-op.
    pub fn start(&self) {
        let pending = self.pending_writer.lock().unwrap().take();
        if let Some(pw) = pending {
            let handle = Self::spawn_writer(pw.receiver, pw.sinks, None);
            *self.writer_handle.lock().unwrap() = Some(handle);
        }
    }

    /// Start the background writer task and wait for it to be ready.
    ///
    /// Returns a Future that completes when the writer thread has started
    /// and is ready to process events. Use this instead of `start()` when
    /// you need to guarantee that events emitted immediately after will
    /// be processed (e.g., for `worker.booting` as the first JSONL event).
    ///
    /// Must be called from within a tokio runtime context.
    pub async fn start_and_wait(&self) -> Result<()> {
        eprintln!("NEEDLE telemetry: starting writer thread and waiting for ready signal...");
        let pending = self.pending_writer.lock().unwrap().take();
        if let Some(pw) = pending {
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let handle = Self::spawn_writer(pw.receiver, pw.sinks, Some(ready_tx));
            *self.writer_handle.lock().unwrap() = Some(handle);

            // Wait for the writer thread to signal it's ready.
            // Use a timeout to avoid hanging if the thread fails to start.
            eprintln!("NEEDLE telemetry: waiting for writer thread ready signal (timeout 5s)...");
            tokio::time::timeout(std::time::Duration::from_secs(5), ready_rx)
                .await
                .map_err(|_| anyhow::anyhow!("writer thread failed to start within 5s"))?
                .map_err(|_| anyhow::anyhow!("writer thread closed before signaling ready"))?;
            eprintln!("NEEDLE telemetry: writer thread ready signal received");
        } else {
            eprintln!("NEEDLE telemetry: writer thread already started, skipping");
        }
        eprintln!("NEEDLE telemetry: start_and_wait complete");
        Ok(())
    }

    /// Flush and shut down the background writer.
    ///
    /// Drops the shared sender (closing the channel) so the writer thread
    /// processes all buffered events and flushes its `BufWriter` before
    /// exiting. Joins the thread to guarantee completion.
    ///
    /// Call this at every terminal path in the worker before the tokio
    /// Runtime is dropped, or the BufWriter flush will be cancelled.
    pub async fn shutdown(&self) {
        // Drop the sender to signal EOF to the writer thread.
        *self.sender.lock().unwrap() = None;
        // Join the writer thread so the flush completes before we return.
        let handle = self.writer_handle.lock().unwrap().take();
        if let Some(h) = handle {
            let _ = h.join();
        }
    }

    /// Shut down the OTLP sink gracefully (if enabled).
    ///
    /// This drains all batched OTLP exports before returning. Should be called
    /// once at process exit, before the Tokio runtime is dropped.
    ///
    /// This is a no-op if the OTLP sink is not configured or if the `otlp`
    /// feature is disabled.
    pub async fn shutdown_otlp(&self) {
        let shutdown = self.otlp_shutdown.lock().unwrap().take();
        if let Some(s) = shutdown {
            if let Err(e) = s.shutdown().await {
                tracing::warn!(error = %e, "OTLP shutdown failed");
            }
        }
    }

    /// Record the current queue depth for the `needle.queue.depth` observable gauge.
    ///
    /// This updates the per-priority counts that the observable gauge callback reads.
    /// The queue depth should be sampled during strand evaluation (typically after
    /// the Pluck strand returns candidates).
    ///
    /// The `depths` parameter maps priority level -> bead count at that priority.
    ///
    /// This is a no-op if the OTLP sink is not configured or if the `otlp`
    /// feature is disabled.
    pub fn record_queue_depth(&self, depths: std::collections::HashMap<u8, u64>) {
        #[cfg(feature = "otlp")]
        {
            let shutdown = self.otlp_shutdown.lock().unwrap();
            if let Some(OtlpShutdown::Sink(sink)) = &*shutdown {
                sink.record_queue_depth(depths);
            }
        }
    }

    /// Spawn background writer task draining the channel to all registered sinks.
    ///
    /// Spawns a dedicated thread with its own tokio runtime so the writer task
    /// can immediately start processing messages, even before the main runtime's
    /// `block_on()` is called. This fixes a deadlock where `force_flush()` would
    /// wait indefinitely for a response from a task that hasn't started yet.
    ///
    /// If `ready_signal` is provided, sends () through it when the writer is
    /// ready to process events.
    fn spawn_writer(
        receiver: mpsc::UnboundedReceiver<WriterMessage>,
        sinks: Vec<Box<dyn Sink>>,
        ready_signal: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(r) => r,
                Err(e) => {
                    // Fallback to stderr when tracing may not be initialized yet
                    eprintln!(
                        "NEEDLE telemetry writer thread: FAILED to create tokio runtime: {}",
                        e
                    );
                    tracing::error!(error = %e, "failed to create writer runtime");
                    // Signal that we failed (by dropping the sender without sending)
                    drop(ready_signal);
                    return;
                }
            };
            eprintln!("NEEDLE telemetry writer thread: started successfully");
            if sinks.is_empty() {
                eprintln!("NEEDLE telemetry writer thread: WARNING - no sinks configured, events will be discarded!");
            }
            let mut receiver = receiver;
            rt.block_on(async move {
                // Signal that the writer is ready to process events.
                // Do this before entering the recv() loop so the caller
                // can proceed as soon as we're ready.
                eprintln!("NEEDLE telemetry writer thread: signaling ready to main thread...");
                if let Some(tx) = ready_signal {
                    tx.send(()).ok();
                }
                eprintln!("NEEDLE telemetry writer thread: ready, waiting for events...");

                let deadline = std::time::Duration::from_secs(5);
                let mut event_count = 0u64;
                while let Some(msg) = receiver.recv().await {
                    match msg {
                        WriterMessage::Event(event) => {
                            event_count += 1;
                            for sink in &sinks {
                                if let Err(e) = sink.accept(&event) {
                                    eprintln!("NEEDLE telemetry: sink accept failed for event {}: {}", event.event_type, e);
                                    tracing::warn!(error = %e, "telemetry sink accept failed");
                                }
                            }
                            if event_count == 1 {
                                eprintln!("NEEDLE telemetry writer thread: first event written: {}", event.event_type);
                            }
                        }
                        WriterMessage::Flush(reply) => {
                            for sink in &sinks {
                                if let Err(e) = sink.flush(deadline) {
                                    eprintln!("NEEDLE telemetry: sink flush failed: {}", e);
                                    tracing::warn!(error = %e, "telemetry sink flush on demand failed");
                                }
                            }
                            reply.send(()).ok();
                        }
                    }
                }
                for sink in &sinks {
                    if let Err(e) = sink.flush(deadline) {
                        tracing::warn!(error = %e, "telemetry sink flush failed");
                    }
                }
            })
        })
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

// ─── Rich filter expressions ──────────────────────────────────────────────────

/// A single filter predicate applied to a telemetry event.
enum FilterPredicate {
    /// Glob pattern matched against `event_type` (backward-compatible).
    EventTypeGlob(regex::Regex),
    /// Exact case-sensitive match on a named field.
    FieldEquals { field: String, value: String },
    /// Regex match on a named field.
    FieldRegex {
        field: String,
        pattern: regex::Regex,
    },
    /// Numeric greater-than on a named field.
    FieldGt { field: String, threshold: f64 },
}

/// A conjunction (AND) of filter predicates for querying telemetry events.
///
/// Build from one or more expression strings with [`LogsFilter::parse`].
pub struct LogsFilter {
    predicates: Vec<FilterPredicate>,
}

impl LogsFilter {
    /// Parse a list of filter expression strings into a `LogsFilter`.
    ///
    /// Each expression may be one of:
    /// - `field=value`   — exact string match
    /// - `field~pattern` — regex match
    /// - `field>number`  — numeric greater-than
    /// - `glob`          — glob pattern on `event_type` (no operator, backward compat)
    ///
    /// All predicates are ANDed together.
    pub fn parse(exprs: &[&str]) -> Result<Self> {
        let mut predicates = Vec::with_capacity(exprs.len());
        for expr in exprs {
            let expr = expr.trim();
            if let Some(pos) = expr.find('=') {
                let field = expr[..pos].to_string();
                let value = expr[pos + 1..].to_string();
                predicates.push(FilterPredicate::FieldEquals { field, value });
            } else if let Some(pos) = expr.find('~') {
                let field = expr[..pos].to_string();
                let raw = &expr[pos + 1..];
                let pattern = regex::Regex::new(raw)
                    .with_context(|| format!("invalid regex in filter '{expr}': {raw}"))?;
                predicates.push(FilterPredicate::FieldRegex { field, pattern });
            } else if let Some(pos) = expr.find('>') {
                let field = expr[..pos].to_string();
                let raw = &expr[pos + 1..];
                let threshold: f64 = raw
                    .parse()
                    .with_context(|| format!("invalid number in filter '{expr}': {raw}"))?;
                predicates.push(FilterPredicate::FieldGt { field, threshold });
            } else {
                // Treat as glob pattern on event_type.
                let re = glob_to_regex(expr)?;
                predicates.push(FilterPredicate::EventTypeGlob(re));
            }
        }
        Ok(LogsFilter { predicates })
    }

    /// Return `true` if the event satisfies all predicates.
    pub fn matches(&self, event: &TelemetryEvent) -> bool {
        self.predicates.iter().all(|p| predicate_matches(p, event))
    }

    /// Return `true` if the filter has no predicates (matches everything).
    pub fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }
}

/// Evaluate a single predicate against an event.
fn predicate_matches(pred: &FilterPredicate, event: &TelemetryEvent) -> bool {
    match pred {
        FilterPredicate::EventTypeGlob(re) => re.is_match(&event.event_type),
        FilterPredicate::FieldEquals { field, value } => {
            get_event_field_str(event, field).as_deref() == Some(value.as_str())
        }
        FilterPredicate::FieldRegex { field, pattern } => get_event_field_str(event, field)
            .map(|v| pattern.is_match(&v))
            .unwrap_or(false),
        FilterPredicate::FieldGt { field, threshold } => get_event_field_f64(event, field)
            .map(|v| v > *threshold)
            .unwrap_or(false),
    }
}

/// Extract a named field from a `TelemetryEvent` as a string.
///
/// Supported top-level fields: `event_type`, `worker_id`, `session_id`,
/// `bead_id`, `workspace`. Data sub-fields can be accessed directly by
/// name (matched against `event.data[field]`).
fn get_event_field_str(event: &TelemetryEvent, field: &str) -> Option<String> {
    match field {
        "event_type" => Some(event.event_type.clone()),
        "worker_id" => Some(event.worker_id.clone()),
        "session_id" => Some(event.session_id.clone()),
        "bead_id" => event.bead_id.as_ref().map(|b| b.as_ref().to_string()),
        "workspace" => event.workspace.as_ref().map(|p| p.display().to_string()),
        _ => {
            // Check data sub-fields.
            event.data.get(field).map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
        }
    }
}

/// Extract a named field from a `TelemetryEvent` as a float.
///
/// Supported numeric fields: `duration_ms`, `sequence`, and any numeric key
/// in `event.data`.
fn get_event_field_f64(event: &TelemetryEvent, field: &str) -> Option<f64> {
    match field {
        "duration_ms" => event.duration_ms.map(|d| d as f64),
        "sequence" => Some(event.sequence as f64),
        _ => event.data.get(field).and_then(|v| v.as_f64()),
    }
}

/// Parse a time-bound string (relative or absolute) into a UTC timestamp.
///
/// This is an alias for [`parse_since`] and accepts the same formats.
pub fn parse_until(input: &str) -> Result<DateTime<Utc>> {
    parse_since(input)
}

/// Read and parse JSONL log files from a directory.
///
/// Returns events sorted by timestamp. Optionally filters by `since`, `until`,
/// and a [`LogsFilter`] predicate set.
pub fn read_logs(
    log_dir: &Path,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    filter: Option<&LogsFilter>,
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
            if let Some(ref ceiling) = until {
                if event.timestamp > *ceiling {
                    continue;
                }
            }
            if let Some(f) = filter {
                if !f.matches(&event) {
                    continue;
                }
            }
            events.push(event);
        }
    }

    events.sort_by_key(|a| a.timestamp);
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

/// Per-worker cost breakdown from effort events.
#[derive(Debug, Default)]
pub struct WorkerCostSummary {
    pub worker_id: String,
    pub total_events: u64,
    pub total_cost_usd: f64,
    pub total_tokens_in: u64,
    pub total_tokens_out: u64,
    pub total_elapsed_ms: u64,
}

/// Compute per-worker cost breakdown from effort events.
///
/// Returns one entry per worker that has at least one effort event, sorted
/// by descending total cost.
pub fn compute_cost_by_worker(events: &[TelemetryEvent]) -> Vec<WorkerCostSummary> {
    let mut map: std::collections::HashMap<String, WorkerCostSummary> =
        std::collections::HashMap::new();
    for event in events {
        if event.event_type != "effort.recorded" {
            continue;
        }
        let entry = map
            .entry(event.worker_id.clone())
            .or_insert_with(|| WorkerCostSummary {
                worker_id: event.worker_id.clone(),
                ..Default::default()
            });
        entry.total_events += 1;
        if let Some(cost) = event.data["estimated_cost_usd"].as_f64() {
            entry.total_cost_usd += cost;
        }
        if let Some(tokens_in) = event.data["tokens_in"].as_u64() {
            entry.total_tokens_in += tokens_in;
        }
        if let Some(tokens_out) = event.data["tokens_out"].as_u64() {
            entry.total_tokens_out += tokens_out;
        }
        if let Some(elapsed) = event.data["elapsed_ms"].as_u64() {
            entry.total_elapsed_ms += elapsed;
        }
    }
    let mut result: Vec<WorkerCostSummary> = map.into_values().collect();
    result.sort_by(|a, b| {
        b.total_cost_usd
            .partial_cmp(&a.total_cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    result
}

/// Per-workspace cost breakdown from effort events.
#[derive(Debug, Default)]
pub struct WorkspaceCostSummary {
    pub workspace: String,
    pub total_events: u64,
    pub total_cost_usd: f64,
    pub total_tokens_in: u64,
    pub total_tokens_out: u64,
    pub total_elapsed_ms: u64,
}

/// Compute per-workspace cost breakdown from effort events.
///
/// Returns one entry per workspace that has at least one effort event, sorted
/// by descending total cost.
pub fn compute_cost_by_workspace(events: &[TelemetryEvent]) -> Vec<WorkspaceCostSummary> {
    let mut map: std::collections::HashMap<String, WorkspaceCostSummary> =
        std::collections::HashMap::new();
    for event in events {
        if event.event_type != "effort.recorded" {
            continue;
        }
        let ws_key = event
            .workspace
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let entry = map
            .entry(ws_key.clone())
            .or_insert_with(|| WorkspaceCostSummary {
                workspace: ws_key,
                ..Default::default()
            });
        entry.total_events += 1;
        if let Some(cost) = event.data["estimated_cost_usd"].as_f64() {
            entry.total_cost_usd += cost;
        }
        if let Some(tokens_in) = event.data["tokens_in"].as_u64() {
            entry.total_tokens_in += tokens_in;
        }
        if let Some(tokens_out) = event.data["tokens_out"].as_u64() {
            entry.total_tokens_out += tokens_out;
        }
        if let Some(elapsed) = event.data["elapsed_ms"].as_u64() {
            entry.total_elapsed_ms += elapsed;
        }
    }
    let mut result: Vec<WorkspaceCostSummary> = map.into_values().collect();
    result.sort_by(|a, b| {
        b.total_cost_usd
            .partial_cmp(&a.total_cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    result
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

    impl Sink for MemorySink {
        fn accept(&self, event: &TelemetryEvent) -> Result<()> {
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
        fn flush(&self, _deadline: std::time::Duration) -> Result<()> {
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
                template_name: "pluck".to_string(),
                template_version: "pluck-default".to_string(),
                prompt_hash: "abc123".to_string(),
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
            priority: 1,
            strand: "pluck".to_string(),
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
            agent: "claude".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
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
            trace_id: None,
            span_id: None,
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
            trace_id: None,
            span_id: None,
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
            trace_id: None,
            span_id: None,
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
        assert!(
            !json.contains("trace_id"),
            "trace_id should be omitted: {}",
            json
        );
        assert!(
            !json.contains("span_id"),
            "span_id should be omitted: {}",
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
        impl Sink for BrokenSink {
            fn accept(&self, _: &TelemetryEvent) -> Result<()> {
                anyhow::bail!("sink is broken")
            }
            fn flush(&self, _deadline: std::time::Duration) -> Result<()> {
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
    async fn emit_try_lock_delivers_events_when_lock_available() {
        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-try-lock".to_string(), sink);

        // emit_try_lock() should succeed when the lock is available.
        telemetry
            .emit_try_lock(EventKind::WorkerStarted {
                worker_name: "test-worker".to_string(),
                version: "0.1.0".to_string(),
            })
            .unwrap();
        telemetry
            .emit_try_lock(EventKind::ClaimAttempt {
                bead_id: BeadId::from("nd-test"),
                attempt: 1,
            })
            .unwrap();

        // Drop to close channel and drain.
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
        assert_eq!(collected[1].event_type, "bead.claim.attempted");
    }

    #[tokio::test]
    async fn emit_try_lock_gracefully_degrades_when_lock_contended() {
        let (sink, _events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-try-lock-contend".to_string(), sink);

        // Hold the sender lock to simulate contention.
        let sender_lock = telemetry.sender.clone();
        let _guard = sender_lock.lock().unwrap();

        // emit_try_lock() should return Ok(()) without blocking when lock is contended.
        let result = telemetry.emit_try_lock(EventKind::QueueEmpty);
        assert!(
            result.is_ok(),
            "emit_try_lock should not error on contention"
        );

        // Drop the guard and cleanup.
        drop(_guard);
        drop(telemetry);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn emit_gracefully_degrades_when_lock_contended() {
        let (sink, _events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-emit-contend".to_string(), sink);

        // Hold the sender lock to simulate contention.
        let sender_lock = telemetry.sender.clone();
        let _guard = sender_lock.lock().unwrap();

        // emit() should return Ok(()) without blocking when lock is contended.
        let result = telemetry.emit(EventKind::QueueEmpty);
        assert!(result.is_ok(), "emit should not error on contention");

        // Drop the guard and cleanup.
        drop(_guard);
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
            trace_id: None,
            span_id: None,
        };

        sink.accept(&event).expect("accept should succeed");
        sink.flush(std::time::Duration::from_secs(5))
            .expect("flush should succeed");

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

    #[test]
    fn file_sink_accept_is_visible_without_explicit_flush() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = FileSink::with_dir(tmp.path(), "test-worker", "test-session").unwrap();

        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "test.flush".to_string(),
            worker_id: "test-worker".to_string(),
            session_id: "test-session".to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({"flushed": true}),
            duration_ms: None,
            trace_id: None,
            span_id: None,
        };
        sink.accept(&event).unwrap();

        // Read the file via an independent handle — no flush() or drop of sink.
        let contents = std::fs::read_to_string(sink.path()).unwrap();
        assert!(contents.contains("test-worker"));
        assert!(contents.contains("\n"));
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
                waterfall_restarts: 0,
                restart_triggers: vec![],
                strand_evaluations: vec![],
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
                priority: 1,
                strand: "pluck".to_string(),
            },
            EventKind::ClaimRaceLost {
                bead_id: id.clone(),
            },
            EventKind::ClaimRaceLostSkipped {
                consecutive_losses: 5,
                threshold: 5,
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
                template_name: "pluck".to_string(),
                template_version: "pluck-default".to_string(),
                prompt_hash: "abc123".to_string(),
            },
            EventKind::DispatchCompleted {
                bead_id: id.clone(),
                exit_code: 0,
                duration_ms: 3000,
                agent: "claude".to_string(),
                model: Some("claude-sonnet-4-6".to_string()),
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
            EventKind::PulseScannerStarted {
                scanner_name: "clippy".to_string(),
            },
            EventKind::PulseScannerCompleted {
                scanner_name: "clippy".to_string(),
                findings_count: 5,
            },
            EventKind::PulseScannerFailed {
                scanner_name: "clippy".to_string(),
                error: "timeout".to_string(),
            },
            EventKind::PulseBeadCreated {
                bead_id: id.clone(),
                scanner_name: "clippy".to_string(),
                severity: 2,
            },
            EventKind::PulseSkipped {
                reason: "cooldown".to_string(),
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
            trace_id: None,
            span_id: None,
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
            trace_id: None,
            span_id: None,
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
            trace_id: None,
            span_id: None,
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
            trace_id: None,
            span_id: None,
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
        let events = read_logs(&dir, None, None, None).unwrap();
        assert!(events.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_logs_with_glob_filter() {
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
            trace_id: None,
            span_id: None,
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
            trace_id: None,
            span_id: None,
        };

        let log_file = dir.join("test-aabb0011.jsonl");
        let content = format!(
            "{}\n{}\n",
            serde_json::to_string(&event1).unwrap(),
            serde_json::to_string(&event2).unwrap()
        );
        std::fs::write(&log_file, content).unwrap();

        let filter = LogsFilter::parse(&["bead.claim.*"]).unwrap();
        let events = read_logs(&dir, None, None, Some(&filter)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "bead.claim.succeeded");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_logs_with_field_equals_filter() {
        let dir = std::env::temp_dir().join("needle-test-logs-field-eq");
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
            trace_id: None,
            span_id: None,
        };
        let event2 = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "worker.started".to_string(),
            worker_id: "bravo".to_string(),
            session_id: "ccdd0022".to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({}),
            duration_ms: None,
            trace_id: None,
            span_id: None,
        };

        let log_file = dir.join("test.jsonl");
        let content = format!(
            "{}\n{}\n",
            serde_json::to_string(&event1).unwrap(),
            serde_json::to_string(&event2).unwrap()
        );
        std::fs::write(&log_file, content).unwrap();

        let filter = LogsFilter::parse(&["worker_id=alpha"]).unwrap();
        let events = read_logs(&dir, None, None, Some(&filter)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].worker_id, "alpha");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_logs_with_field_gt_filter() {
        let dir = std::env::temp_dir().join("needle-test-logs-field-gt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let event1 = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "agent.completed".to_string(),
            worker_id: "alpha".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({}),
            duration_ms: Some(2000),
            trace_id: None,
            span_id: None,
        };
        let event2 = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "agent.completed".to_string(),
            worker_id: "alpha".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 1,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({}),
            duration_ms: Some(500),
            trace_id: None,
            span_id: None,
        };

        let log_file = dir.join("test.jsonl");
        let content = format!(
            "{}\n{}\n",
            serde_json::to_string(&event1).unwrap(),
            serde_json::to_string(&event2).unwrap()
        );
        std::fs::write(&log_file, content).unwrap();

        let filter = LogsFilter::parse(&["duration_ms>1000"]).unwrap();
        let events = read_logs(&dir, None, None, Some(&filter)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].duration_ms, Some(2000));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_logs_with_until_bound() {
        let dir = std::env::temp_dir().join("needle-test-logs-until");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let past = Utc::now() - chrono::Duration::hours(2);
        let recent = Utc::now() - chrono::Duration::minutes(5);

        let event1 = TelemetryEvent {
            timestamp: past,
            event_type: "worker.started".to_string(),
            worker_id: "alpha".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({}),
            duration_ms: None,
            trace_id: None,
            span_id: None,
        };
        let event2 = TelemetryEvent {
            timestamp: recent,
            event_type: "worker.started".to_string(),
            worker_id: "alpha".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 1,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({}),
            duration_ms: None,
            trace_id: None,
            span_id: None,
        };

        let log_file = dir.join("test.jsonl");
        let content = format!(
            "{}\n{}\n",
            serde_json::to_string(&event1).unwrap(),
            serde_json::to_string(&event2).unwrap()
        );
        std::fs::write(&log_file, content).unwrap();

        // until 1 hour ago — only the 2h-old event should be included
        let until = Utc::now() - chrono::Duration::hours(1);
        let events = read_logs(&dir, None, Some(until), None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logs_filter_parse_field_regex() {
        let filter = LogsFilter::parse(&["event_type~bead\\..*"]).unwrap();
        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "bead.claim.succeeded".to_string(),
            worker_id: "alpha".to_string(),
            session_id: "aabb0011".to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({}),
            duration_ms: None,
            trace_id: None,
            span_id: None,
        };
        assert!(filter.matches(&event));

        let event2 = TelemetryEvent {
            event_type: "worker.started".to_string(),
            ..event.clone()
        };
        assert!(!filter.matches(&event2));
    }

    #[test]
    fn logs_filter_parse_multiple_predicates_anded() {
        // Both conditions must hold: worker_id=alpha AND event_type=worker.started
        let filter = LogsFilter::parse(&["worker_id=alpha", "event_type=worker.started"]).unwrap();

        let matching = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "worker.started".to_string(),
            worker_id: "alpha".to_string(),
            session_id: "s1".to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({}),
            duration_ms: None,
            trace_id: None,
            span_id: None,
        };
        assert!(filter.matches(&matching));

        let wrong_worker = TelemetryEvent {
            worker_id: "bravo".to_string(),
            ..matching.clone()
        };
        assert!(!filter.matches(&wrong_worker));

        let wrong_type = TelemetryEvent {
            event_type: "worker.stopped".to_string(),
            ..matching.clone()
        };
        assert!(!filter.matches(&wrong_type));
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
                trace_id: None,
                span_id: None,
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
                trace_id: None,
                span_id: None,
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
                trace_id: None,
                span_id: None,
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

    // ── HookSink tests ──

    fn make_test_event(event_type: &str) -> TelemetryEvent {
        TelemetryEvent {
            timestamp: Utc::now(),
            event_type: event_type.to_string(),
            worker_id: "alpha".to_string(),
            session_id: "test0000".to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            data: serde_json::json!({"test": true}),
            duration_ms: None,
            trace_id: None,
            span_id: None,
        }
    }

    #[test]
    fn hook_sink_new_compiles_valid_filters() {
        let configs = vec![
            HookConfig {
                event_filter: "outcome.*".to_string(),
                command: "cat".to_string(),
                url: None,
            },
            HookConfig {
                event_filter: "worker.errored".to_string(),
                command: "cat".to_string(),
                url: None,
            },
        ];
        let sink = HookSink::new(&configs);
        assert!(sink.is_ok());
        assert!(!sink.unwrap().is_empty());
    }

    #[test]
    fn hook_sink_empty_when_no_configs() {
        let sink = HookSink::new(&[]).unwrap();
        assert!(sink.is_empty());
    }

    #[test]
    fn hook_sink_invalid_filter_returns_error() {
        let configs = vec![HookConfig {
            event_filter: "[invalid".to_string(),
            command: "cat".to_string(),
            url: None,
        }];
        assert!(HookSink::new(&configs).is_err());
    }

    #[test]
    fn hook_sink_dispatch_matches_filter() {
        let configs = vec![HookConfig {
            event_filter: "outcome.*".to_string(),
            command: "true".to_string(), // always succeeds
            url: None,
        }];
        let sink = HookSink::new(&configs).unwrap();

        // Matching event — should dispatch (no failures expected)
        let event = make_test_event("outcome.handled");
        let failures = sink.dispatch(&event);
        assert!(failures.is_empty());
    }

    #[test]
    fn hook_sink_dispatch_skips_non_matching() {
        let configs = vec![HookConfig {
            event_filter: "outcome.*".to_string(),
            command: "false".to_string(), // would fail if dispatched
            url: None,
        }];
        let sink = HookSink::new(&configs).unwrap();

        // Non-matching event — should NOT dispatch
        let event = make_test_event("worker.started");
        let failures = sink.dispatch(&event);
        // No failures because the hook was never called
        assert!(failures.is_empty());
    }

    #[test]
    fn hook_sink_dispatch_prevents_recursion_on_sink_error() {
        let configs = vec![HookConfig {
            event_filter: "telemetry.*".to_string(),
            command: "true".to_string(),
            url: None,
        }];
        let sink = HookSink::new(&configs).unwrap();

        // SinkError events must never be dispatched to hooks
        let event = make_test_event("telemetry.sink_error");
        let failures = sink.dispatch(&event);
        assert!(failures.is_empty());
    }

    #[test]
    fn hook_sink_dispatch_captures_failure() {
        let configs = vec![HookConfig {
            event_filter: "bead.*".to_string(),
            command: "/nonexistent/command/that/does/not/exist".to_string(),
            url: None,
        }];
        let sink = HookSink::new(&configs).unwrap();

        let event = make_test_event("bead.completed");
        let failures = sink.dispatch(&event);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].event_type, "telemetry.sink_error");
        assert!(failures[0].data["hook_command"]
            .as_str()
            .unwrap()
            .contains("nonexistent"));
    }

    #[test]
    fn hook_sink_multiple_hooks_matching_same_event() {
        let configs = vec![
            HookConfig {
                event_filter: "outcome.*".to_string(),
                command: "true".to_string(),
                url: None,
            },
            HookConfig {
                event_filter: "outcome.handled".to_string(),
                command: "true".to_string(),
                url: None,
            },
        ];
        let sink = HookSink::new(&configs).unwrap();

        let event = make_test_event("outcome.handled");
        let failures = sink.dispatch(&event);
        // Both hooks match, both succeed — no failures
        assert!(failures.is_empty());
    }

    #[test]
    fn hook_sink_dispatches_json_to_stdin() {
        let tmp = std::env::temp_dir().join("needle-hook-test-stdin");
        let _ = std::fs::remove_file(&tmp);

        let cmd = format!("cat > {}", tmp.display());
        let configs = vec![HookConfig {
            event_filter: "worker.*".to_string(),
            command: cmd,
            url: None,
        }];
        let sink = HookSink::new(&configs).unwrap();

        let event = make_test_event("worker.started");
        let failures = sink.dispatch(&event);
        assert!(failures.is_empty());

        // Give the child process a moment to write
        std::thread::sleep(std::time::Duration::from_millis(200));

        let content = std::fs::read_to_string(&tmp).unwrap_or_default();
        assert!(
            !content.is_empty(),
            "hook should have received JSON on stdin"
        );
        // Verify it's valid JSON containing the event type
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["event_type"], "worker.started");

        let _ = std::fs::remove_file(&tmp);
    }

    // Regression test for needle-xeh: Telemetry construction must not require
    // an active tokio runtime. The writer is deferred until start() is called.
    #[test]
    fn telemetry_new_does_not_require_runtime() {
        // This runs outside any tokio context. Before the fix, this panicked
        // with "there is no reactor running, must be called from the context
        // of a Tokio 1.x runtime".
        let telemetry = Telemetry::new("needle-test".to_string());
        // emit() should be safe even without a started writer (channel is unbounded)
        assert!(telemetry.emit(EventKind::QueueEmpty).is_ok());
    }

    #[tokio::test]
    async fn telemetry_start_spawns_writer_and_delivers_events() {
        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-start".to_string(), sink);

        // start() must be called from inside the runtime
        telemetry.start();

        telemetry
            .emit(EventKind::WorkerStarted {
                worker_name: "test-start".to_string(),
                version: "0.0.0".to_string(),
            })
            .unwrap();

        // Give the background task a moment to drain the channel.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let received = events.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].event_type, "worker.started");
    }

    /// Regression test: BufWriter never flushed on short-lived sessions.
    ///
    /// When a worker hits EXHAUSTED quickly the total telemetry is only a few
    /// hundred bytes — well below BufWriter's 8 KB auto-flush threshold.
    /// `shutdown()` must drop the sender (closing the channel) and *await* the
    /// writer task's JoinHandle so the flush runs before the tokio Runtime drops.
    #[tokio::test]
    async fn shutdown_flushes_bufwriter_on_short_lived_session() {
        let dir = std::env::temp_dir().join("needle-test-shutdown-flush");
        let _ = std::fs::remove_dir_all(&dir);

        let (telemetry, path) = Telemetry::with_log_dir_and_path("test-worker".to_string(), &dir)
            .expect("should create");

        // start() must be called from inside the runtime.
        telemetry.start();

        // Emit a handful of small events — total << 8 KB BufWriter threshold.
        telemetry
            .emit(EventKind::WorkerStarted {
                worker_name: "test-worker".to_string(),
                version: "0.1.0".to_string(),
            })
            .unwrap();
        telemetry.emit(EventKind::QueueEmpty).unwrap();
        telemetry
            .emit(EventKind::WorkerStopped {
                reason: "exhausted".to_string(),
                beads_processed: 0,
                uptime_secs: 0,
            })
            .unwrap();

        // shutdown() closes the channel, awaits the writer task, and guarantees
        // the BufWriter is flushed before returning — no sleep required.
        telemetry.shutdown().await;

        // The file must contain the 3 events.  If the BufWriter was never
        // flushed the file would be 0 bytes and this assertion would fail.
        let content = std::fs::read_to_string(&path).expect("log file must exist");
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 3, "expected 3 JSONL lines after shutdown");
        for line in &lines {
            let v: serde_json::Value =
                serde_json::from_str(line).expect("each line must be valid JSON");
            assert!(
                v.get("event_type").is_some(),
                "event_type field must be present"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Verify that events emitted inside an OTel span carry the correct
    /// trace_id and span_id in 32/16 lowercase-hex W3C format.
    #[cfg(feature = "otlp")]
    #[tokio::test]
    async fn emit_inside_span_captures_trace_and_span_ids() {
        use opentelemetry::{
            trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState},
            Context, KeyValue,
        };
        use std::borrow::Cow;
        use std::time::SystemTime;

        let trace_bytes: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let span_bytes: [u8; 8] = [17, 18, 19, 20, 21, 22, 23, 24];
        let expected_trace = hex::encode(trace_bytes);
        let expected_span = hex::encode(span_bytes);

        let span_ctx = SpanContext::new(
            TraceId::from_bytes(trace_bytes),
            SpanId::from_bytes(span_bytes),
            TraceFlags::SAMPLED,
            false,
            TraceState::NONE,
        );

        struct FakeSpan(SpanContext);
        impl opentelemetry::trace::Span for FakeSpan {
            fn add_event_with_timestamp<T: Into<Cow<'static, str>>>(
                &mut self,
                _: T,
                _: SystemTime,
                _: Vec<KeyValue>,
            ) {
            }
            fn span_context(&self) -> &SpanContext {
                &self.0
            }
            fn is_recording(&self) -> bool {
                true
            }
            fn set_attribute(&mut self, _: KeyValue) {}
            fn set_status(&mut self, _: opentelemetry::trace::Status) {}
            fn update_name<T: Into<Cow<'static, str>>>(&mut self, _: T) {}
            fn add_link(&mut self, _: SpanContext, _: Vec<KeyValue>) {}
            fn end_with_timestamp(&mut self, _: SystemTime) {}
        }

        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-worker".to_string(), sink);
        telemetry.start();

        let ctx = Context::current().with_span(FakeSpan(span_ctx));
        let _guard = ctx.attach();
        telemetry.emit(EventKind::QueueEmpty).unwrap();
        drop(_guard);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let received = events.lock().unwrap();
        assert_eq!(received.len(), 1, "expected exactly one event");
        assert_eq!(
            received[0].trace_id.as_deref(),
            Some(expected_trace.as_str()),
            "trace_id must be 32 hex chars matching W3C traceparent"
        );
        assert_eq!(
            received[0].span_id.as_deref(),
            Some(expected_span.as_str()),
            "span_id must be 16 hex chars matching W3C traceparent"
        );
    }

    /// Events emitted outside any active span must have None trace_id / span_id.
    #[tokio::test]
    async fn emit_outside_span_has_no_trace_ids() {
        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-worker".to_string(), sink);
        telemetry.start();

        telemetry.emit(EventKind::QueueEmpty).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let received = events.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert!(
            received[0].trace_id.is_none(),
            "trace_id must be absent outside a span"
        );
        assert!(
            received[0].span_id.is_none(),
            "span_id must be absent outside a span"
        );
    }

    // ── Sink trait / TelemetryBus tests ──

    /// Adding a new sink requires only implementing `Sink` and registering it;
    /// the bus fans events out to all registered sinks automatically.
    #[tokio::test]
    async fn bus_fans_out_to_all_sinks() {
        let (sink1, events1) = MemorySink::new();
        let (sink2, events2) = MemorySink::new();
        let telemetry = Telemetry::with_boxed_sinks(
            "test-fanout".to_string(),
            vec![Box::new(sink1), Box::new(sink2)],
        );
        telemetry.start();
        telemetry.emit(EventKind::QueueEmpty).unwrap();
        telemetry.shutdown().await;

        let e1 = events1.lock().unwrap();
        let e2 = events2.lock().unwrap();
        assert_eq!(e1.len(), 1, "sink1 must receive the event");
        assert_eq!(e2.len(), 1, "sink2 must receive the event");
    }

    /// `flush(deadline)` is called on every registered sink during graceful
    /// shutdown, and the deadline value is forwarded to the sink.
    #[tokio::test]
    async fn shutdown_flush_calls_flush_with_deadline() {
        let deadline_received = Arc::new(std::sync::Mutex::new(None::<std::time::Duration>));

        struct DeadlineSink {
            received: Arc<std::sync::Mutex<Option<std::time::Duration>>>,
        }
        impl Sink for DeadlineSink {
            fn accept(&self, _: &TelemetryEvent) -> Result<()> {
                Ok(())
            }
            fn flush(&self, deadline: std::time::Duration) -> Result<()> {
                *self.received.lock().unwrap() = Some(deadline);
                Ok(())
            }
        }

        let telemetry = Telemetry::with_boxed_sinks(
            "test-deadline".to_string(),
            vec![Box::new(DeadlineSink {
                received: deadline_received.clone(),
            })],
        );
        telemetry.start();
        telemetry.emit(EventKind::QueueEmpty).unwrap();
        telemetry.shutdown().await;

        let dl = deadline_received.lock().unwrap();
        assert!(
            dl.is_some(),
            "flush must be called with a non-None deadline on shutdown"
        );
        assert!(
            dl.unwrap() > std::time::Duration::ZERO,
            "deadline passed to flush must be positive"
        );
    }

    /// A blocking fake sink whose `flush` sleeps longer than its deadline must
    /// return an error rather than hanging indefinitely. The bus must not block
    /// past the deadline.
    #[tokio::test]
    async fn shutdown_does_not_hang_when_flush_exceeds_deadline() {
        struct SlowFlusher;
        impl Sink for SlowFlusher {
            fn accept(&self, _: &TelemetryEvent) -> Result<()> {
                Ok(())
            }
            fn flush(&self, deadline: std::time::Duration) -> Result<()> {
                // Sleep twice the deadline, then report timeout.
                std::thread::sleep(deadline * 2);
                anyhow::bail!("flush timed out (deliberate in test)")
            }
        }

        let telemetry =
            Telemetry::with_boxed_sinks("test-slow-flush".to_string(), vec![Box::new(SlowFlusher)]);
        telemetry.start();
        telemetry.emit(EventKind::QueueEmpty).unwrap();

        // shutdown() must complete in a reasonable wall-clock window even though
        // SlowFlusher sleeps past its own deadline.
        let start = std::time::Instant::now();
        telemetry.shutdown().await;
        let elapsed = start.elapsed();

        // The bus deadline is 5 s; SlowFlusher sleeps 10 s but must not be
        // awaited indefinitely — 30 s is a generous upper bound.
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "shutdown must not hang: elapsed={elapsed:?}"
        );
    }

    /// Verify that `trace_id` and `span_id` are captured from the current OTel span.
    #[cfg(feature = "otlp")]
    #[tokio::test]
    async fn emit_captures_trace_and_span_ids_from_otel_context() {
        use opentelemetry::trace::{Tracer, TracerProvider as _};
        use opentelemetry_sdk::trace::SdkTracerProvider;

        // Create a simple tracer provider for testing (no exporter).
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("test");

        // Create a test sink to capture emitted events.
        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-tracer".to_string(), sink);

        // Emit an event inside a span using the tracer's in_span method.
        tracer.in_span("test-span", |_cx| {
            telemetry
                .emit(EventKind::QueueEmpty)
                .expect("emit should succeed");
        });

        // Drop telemetry to close channel and drain events.
        drop(telemetry);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 1, "expected 1 event");
        let event = &collected[0];

        // Verify trace_id and span_id were captured.
        assert!(
            event.trace_id.is_some(),
            "trace_id should be captured inside a span"
        );
        assert!(
            event.span_id.is_some(),
            "span_id should be captured inside a span"
        );

        // Verify W3C format: 32 hex chars for trace_id, 16 hex chars for span_id.
        let trace_id = event.trace_id.as_ref().unwrap();
        let span_id = event.span_id.as_ref().unwrap();
        assert_eq!(trace_id.len(), 32, "trace_id should be 32 hex chars");
        assert_eq!(span_id.len(), 16, "span_id should be 16 hex chars");
        assert!(
            trace_id
                .chars()
                .all(|c| c.is_ascii_hexdigit() && c.is_lowercase()),
            "trace_id should be lowercase hex"
        );
        assert!(
            span_id
                .chars()
                .all(|c| c.is_ascii_hexdigit() && c.is_lowercase()),
            "span_id should be lowercase hex"
        );
    }

    /// Verify that events emitted outside any span omit trace_id/span_id.
    #[tokio::test]
    async fn emit_omits_trace_and_span_ids_outside_span() {
        // Create a test sink to capture emitted events.
        let (sink, events) = MemorySink::new();
        let telemetry = Telemetry::with_sink("test-no-span".to_string(), sink);

        // Emit an event outside any span context.
        telemetry
            .emit(EventKind::QueueEmpty)
            .expect("emit should succeed");

        // Drop telemetry to close channel and drain events.
        drop(telemetry);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 1, "expected 1 event");
        let event = &collected[0];

        // Verify trace_id and span_id are None when emitted outside a span.
        assert!(
            event.trace_id.is_none(),
            "trace_id should be None outside a span"
        );
        assert!(
            event.span_id.is_none(),
            "span_id should be None outside a span"
        );

        // Verify the JSON omits the fields entirely (via skip_serializing_if).
        let json = serde_json::to_string(event).expect("serialize");
        assert!(
            !json.contains("trace_id"),
            "trace_id should be omitted from JSON when None"
        );
        assert!(
            !json.contains("span_id"),
            "span_id should be omitted from JSON when None"
        );
    }

    /// Regression test for needle-la6l: verify that worker.booting is written to file.
    ///
    /// The boot event is written synchronously via write_boot_event_direct_impl
    /// before the async writer starts. This test verifies the file is not empty
    /// after Telemetry::new() returns.
    #[test]
    fn boot_event_written_to_file_on_telemetry_creation() {
        let dir = std::env::temp_dir().join("needle-test-boot-event");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("should create temp dir");

        let worker_id = "test-boot-worker";
        let session_id = "cafe1234";
        let file_path = dir.join(format!("{worker_id}-{session_id}.jsonl"));

        // Create a FileSink (this creates the file and writes the boot event)
        let file_sink = FileSink::with_dir(&dir, worker_id, session_id)
            .expect("FileSink::with_dir should succeed");
        assert!(file_path.exists(), "log file should be created");

        // write_boot_event_direct should succeed
        let version = env!("CARGO_PKG_VERSION");
        let result = file_sink.write_boot_event_direct(worker_id, session_id, version);
        assert!(
            result.is_ok(),
            "write_boot_event_direct should succeed: {:?}",
            result
        );

        // Verify the file has content (not 0 bytes)
        let metadata = std::fs::metadata(&file_path).expect("should get file metadata");
        assert!(
            metadata.len() > 0,
            "log file should not be empty after boot event write"
        );

        // Verify the content is valid JSON with worker.booting event
        let content = std::fs::read_to_string(&file_path).expect("should read file");
        let first_line = content
            .lines()
            .next()
            .expect("file should have at least one line");
        let event: serde_json::Value =
            serde_json::from_str(first_line).expect("first line should be valid JSON");

        assert_eq!(
            event["event_type"], "worker.booting",
            "first event should be worker.booting"
        );
        assert_eq!(event["worker_id"], worker_id, "worker_id should match");
        assert_eq!(event["session_id"], session_id, "session_id should match");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
