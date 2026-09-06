//! Outcome routing: map agent exit codes to explicit handlers.
//!
//! Every possible exit code has a named handler. The type system enforces
//! exhaustiveness — if an outcome can happen, it must have a handler.
//!
//! Depends on: `types`, `config`, `bead_store`, `telemetry`, `validation`.

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::bead_store::BeadStore;
use crate::config::Config;
use crate::fingerprint::{
    append_alert_note, build_alert_labels, check_alert_deduplication, AlertDeduplication, AlertKind,
};
use crate::gate_health;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{AgentOutcome, Bead, BeadAction, BeadId, BeadStatus, HandlerResult, Outcome};
use crate::validation::{
    dod_bypass, predispatch, verify_shipped_work, GateConfig, GateReport, GateResult,
    ValidationGate,
};

// ──────────────────────────────────────────────────────────────────────────────
// classify (convenience re-export)
// ──────────────────────────────────────────────────────────────────────────────

/// Classify an agent result into an `Outcome`, with verification and shutdown
/// signal support.
///
/// Interruption takes precedence. Otherwise, failed verification is always a
/// failure; only verified results are delegated to the exit-code classifier.
pub fn classify(exit_code: i32, was_interrupted: bool, verified: bool) -> Outcome {
    classify_with_stream(exit_code, was_interrupted, verified, "")
}

/// Like [`classify`], but also consults the agent stream's final
/// `type="result"` envelope.
///
/// The claude CLI exits 0 even when the session terminated on an API error, so
/// an envelope carrying `is_error` or an error `terminal_reason` is a failure
/// regardless of the exit code. Streams without a result envelope (other trace
/// formats, or a run killed before the envelope was emitted) fall back to the
/// exit-code classifier.
pub fn classify_with_stream(
    exit_code: i32,
    was_interrupted: bool,
    verified: bool,
    stdout: &str,
) -> Outcome {
    if was_interrupted {
        return Outcome::Interrupted;
    }

    if !verified {
        return Outcome::Failure;
    }

    if crate::trace::stream_indicates_failure(stdout) {
        return Outcome::Failure;
    }

    Outcome::classify(exit_code, false)
}

// ──────────────────────────────────────────────────────────────────────────────
// OutcomeHandler
// ──────────────────────────────────────────────────────────────────────────────

/// Routes agent outcomes to their explicit handlers.
pub struct OutcomeHandler {
    config: Config,
    telemetry: Telemetry,
}

impl OutcomeHandler {
    pub fn new(config: Config, telemetry: Telemetry) -> Self {
        OutcomeHandler { config, telemetry }
    }

    /// Run a bead store operation with a 30s timeout.
    ///
    /// Returns `Ok(Some(T))` on success, `Ok(None)` on timeout, and `Err(E)` on
    /// other errors. Callers should treat timeout and error as non-fatal — log
    /// and continue rather than blocking the worker in HANDLING state.
    async fn timeout_op<T, F, Fut>(
        &self,
        op: F,
        operation_name: &str,
    ) -> Result<Option<T>, anyhow::Error>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, anyhow::Error>>,
    {
        match tokio::time::timeout(std::time::Duration::from_secs(30), op()).await {
            Ok(Ok(result)) => Ok(Some(result)),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                tracing::error!(
                    operation = operation_name,
                    "bead store operation timed out after 30s"
                );
                Ok(None)
            }
        }
    }

    /// Prepare telemetry events for a bead release.
    ///
    /// This helper method collects telemetry events that would be emitted during
    /// a bead release operation, but does NOT actually perform the release.
    /// The actual release must be performed by the caller via BeadAction.
    ///
    /// This is part of the structural enforcement: handlers only return BeadAction,
    /// they never directly mutate bead state. The worker's apply_bead_action()
    /// method is the ONLY place that calls store.release().
    ///
    /// Flow:
    /// 1. Flush the configured backend's durable checkpoint.
    /// 2. Return telemetry events that would be emitted during release.
    ///
    /// A flush failure pauses the workspace and leaves the bead untouched.
    async fn prepare_release_events(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<Vec<EventKind>> {
        let events = Vec::new();

        // Flush before apply_bead_action() releases the bead. A heartbeat
        // identifies where an unresponsive backend operation is waiting.
        let _ = self.telemetry.emit_try_lock(
            EventKind::HeartbeatEmitted {
                bead_id: Some(bead.id.clone()),
                state: "HANDLING_FLUSH".to_string(),
            },
            Utc::now(),
        );

        match self.timeout_op(|| store.flush(), "flush").await {
            Ok(Some(())) => {
                tracing::debug!(
                    bead_id = %bead.id,
                    "flushed local changes to JSONL before release"
                );
                // Emit heartbeat after successful flush.
                let _ = self.telemetry.emit_try_lock(
                    EventKind::HeartbeatEmitted {
                        bead_id: Some(bead.id.clone()),
                        state: "HANDLING_FLUSH_DONE".to_string(),
                    },
                    Utc::now(),
                );
            }
            Ok(None) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    "flush timed out before release will be attempted by apply_bead_action()"
                );
                store.pause_workspace("checkpoint flush timed out before release".to_string());
                anyhow::bail!("checkpoint flush timed out; preserving bead state");
            }
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "flush failed before release will be attempted by apply_bead_action()"
                );
                store.pause_workspace(format!("checkpoint flush failed before release: {e:#}"));
                return Err(e.context("checkpoint flush failed; preserving bead state"));
            }
        }

        // Note: The actual release happens in apply_bead_action(), not here.
        // We only prepare telemetry events here.

        Ok(events)
    }

    /// Run verification gates for a bead, returning whether all passed.
    ///
    /// This is extracted as a helper so it can be called BEFORE outcome classification.
    /// Verification must determine Success, not just exit code 0.
    async fn run_verification_gates(&self, bead: &Bead) -> Result<(bool, Option<GateReport>)> {
        // Try pluggable gates first, fall back to legacy verification commands.
        let gate_opt = if !self.config.gates.is_empty() {
            // New pluggable gate system. Fill in each command gate's stderr
            // cap from `validation.stderr_cap_bytes` unless the gate already
            // set its own override — see GitHub issue jedarden/NEEDLE#9.
            let default_stderr_cap = self.config.validation.stderr_cap_bytes;
            let gate_configs: Vec<(String, GateConfig)> = self
                .config
                .gates
                .iter()
                .enumerate()
                .map(|(i, config)| {
                    let mut config = config.clone();
                    let GateConfig::Command {
                        stderr_cap_bytes, ..
                    } = &mut config;
                    if stderr_cap_bytes.is_none() {
                        *stderr_cap_bytes = Some(default_stderr_cap);
                    }
                    (format!("gate_{}", i), config)
                })
                .collect();
            ValidationGate::new(gate_configs, bead.workspace.clone())
        } else if !self.config.verification.is_empty() {
            // Legacy verification command format.
            ValidationGate::from_commands_with_stderr_cap(
                self.config.verification.clone(),
                bead.workspace.clone(),
                self.config.validation.stderr_cap_bytes,
            )
        } else {
            // No gates configured — treat as verified (backward compatible).
            return Ok((true, None));
        };

        // Run the gate and return the result.
        let gate = gate_opt.ok_or_else(|| {
            anyhow::anyhow!("validation gate creation failed - all gates failed to initialize")
        })?;
        let report = gate.run(bead).await?;
        let all_passed = report.all_passed;
        Ok((all_passed, Some(report)))
    }

    /// Handle a process output for the given bead.
    ///
    /// CRITICAL: For exit code 0, verification gates run BEFORE classification.
    /// This ensures Success means verification passed, not just that the agent exited 0.
    /// An agent that exits 0 but fails verification produces Failure, not Success.
    #[tracing::instrument(
        name = "bead.outcome",
        skip(self, store, bead, output),
        fields(
            needle.bead.id = %bead.id,
            needle.outcome = tracing::field::Empty,
            needle.outcome.action = tracing::field::Empty,
        )
    )]
    pub async fn handle(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        output: &AgentOutcome,
        was_interrupted: bool,
    ) -> Result<HandlerResult> {
        // For exit code 0, run verification BEFORE classification.
        // This is the core fix: Success must mean verification passed.
        let (verified, gate_report) = if output.exit_code == 0 && !was_interrupted {
            self.run_verification_gates(bead).await?
        } else {
            // Non-zero exit or interrupted — verification irrelevant.
            (true, None)
        };

        // Classification consults the stream's result envelope in addition to
        // the exit code: a terminal API error exits 0 but is not a success.
        let outcome =
            classify_with_stream(output.exit_code, was_interrupted, verified, &output.stdout);

        // Set outcome as span attribute
        tracing::Span::current().record("needle.outcome", outcome.as_str());

        tracing::info!(
            bead_id = %bead.id,
            exit_code = output.exit_code,
            verified,
            outcome = %outcome,
            "handling agent outcome"
        );

        // Use emit_try_lock() to avoid blocking if telemetry writer is stuck.
        // This prevents worker hang in HANDLING state when telemetry is wedged.
        let _ = self.telemetry.emit_try_lock(
            EventKind::OutcomeClassified {
                bead_id: bead.id.clone(),
                outcome: outcome.as_str().to_string(),
                exit_code: output.exit_code,
            },
            Utc::now(),
        );

        let (bead_action, telemetry_events) = match outcome.clone() {
            Outcome::Success => self.handle_success(store, bead, gate_report).await?,
            Outcome::Failure => {
                // If we have a gate report with failures, check if any gate had execution errors.
                if let Some(report) = gate_report {
                    if !report.all_passed {
                        // Check if any result is an ExecutionError
                        let execution_error =
                            report.results.iter().find(|(_, r)| r.is_execution_error());
                        if let Some((gate_name, result)) = execution_error {
                            if let GateResult::ExecutionError { command, reason } = result {
                                self.handle_gate_error(
                                    store,
                                    bead,
                                    &bead.workspace.display().to_string(),
                                    gate_name,
                                    command,
                                    reason,
                                )
                                .await?
                            } else {
                                unreachable!() // We already checked is_execution_error()
                            }
                        } else {
                            self.handle_gate_failure(store, bead, &report).await?
                        }
                    } else {
                        self.handle_failure(store, bead).await?
                    }
                } else {
                    self.handle_failure(store, bead).await?
                }
            }
            Outcome::Timeout => self.handle_timeout(store, bead).await?,
            Outcome::AgentNotFound => self.handle_agent_not_found(store, bead).await?,
            Outcome::Interrupted => self.handle_interrupted(store, bead).await?,
            Outcome::Crash(code) => self.handle_crash(store, bead, code).await?,
            Outcome::GateError => {
                // This should not be reached - GateError is only produced during outcome handling
                // when gate execution errors are detected. For now, treat as regular failure.
                tracing::error!(
                    bead_id = %bead.id,
                    "unexpected GateError outcome — treating as regular failure"
                );
                self.handle_failure(store, bead).await?
            }
            Outcome::GateUnsatisfiable => {
                // Not reachable yet: no classification path produces GateUnsatisfiable —
                // it is assigned from gate-report analysis (precondition unsatisfiable),
                // which lands separately. Placeholder follows the GateError precedent;
                // the real handling must NOT attribute the failure to the work or retry
                // the bead, since no work can satisfy the gate.
                tracing::error!(
                    bead_id = %bead.id,
                    "unexpected GateUnsatisfiable outcome — treating as regular failure"
                );
                self.handle_failure(store, bead).await?
            }
        };

        // Emit sub-handler events (e.g. BeadCompleted, BeadOrphaned) to the
        // telemetry sink so they appear in the JSONL log.
        // Use emit_try_lock() to avoid blocking if telemetry writer is stuck.
        for event in &telemetry_events {
            let timestamp = Utc::now();
            tracing::debug!(
                event_type = %event.event_type(),
                timestamp = %timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                "captured timestamp for sub-handler telemetry event"
            );
            let _ = self.telemetry.emit_try_lock(event.clone(), timestamp);
        }

        // Use emit_try_lock() to avoid blocking if telemetry writer is stuck.
        let timestamp = Utc::now();
        tracing::debug!(
            event_type = "outcome.handled",
            timestamp = %timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            bead_id = %bead.id,
            outcome = %outcome.as_str(),
            action = %bead_action.to_string(),
            "captured timestamp for outcome handled telemetry event"
        );
        let _ = self.telemetry.emit_try_lock(
            EventKind::OutcomeHandled {
                bead_id: bead.id.clone(),
                outcome: outcome.as_str().to_string(),
                action: bead_action.to_string(),
            },
            timestamp,
        );

        // Set action as span attribute
        tracing::Span::current().record("needle.outcome.action", bead_action.to_string());

        // Set span status: Ok for success, Error otherwise
        if !matches!(outcome, Outcome::Success) {
            tracing::Span::current().record("otel.status_code", 2u64);
            tracing::Span::current().record("otel.status_description", outcome.as_str());
        }

        Ok(HandlerResult {
            outcome,
            bead_action,
            telemetry_events,
        })
    }

    /// Handle a process output with cancellation support.
    ///
    /// Checks the cancellation flag before starting the handler and returns
    /// early if the handler has been cancelled (e.g., due to a timeout in
    /// the worker). This prevents the handler from making further br calls
    /// after a timeout has occurred.
    pub async fn handle_with_cancellation(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        output: &AgentOutcome,
        was_interrupted: bool,
        cancelled: Arc<AtomicBool>,
    ) -> Result<HandlerResult> {
        // Check if we've been cancelled before starting.
        if cancelled.load(Ordering::Acquire) {
            tracing::warn!(
                bead_id = %bead.id,
                "outcome handler cancelled before starting, returning early"
            );
            // Return an explicit error action so the worker's action applier
            // performs release recovery. Verification never ran, so
            // conservatively treat the outcome as unverified.
            return Ok(HandlerResult {
                outcome: classify(output.exit_code, was_interrupted, false),
                bead_action: BeadAction::Errored,
                telemetry_events: vec![],
            });
        }

        // Wrap the handler in a timeout to prevent indefinite hangs.
        // This is a safety net in case the internal br call timeouts don't work.
        // Configurable via `validation.outcome_timeout_seconds` (default 50) —
        // see GitHub issue jedarden/NEEDLE#8: a gate running a real verification
        // workload (container test suite, secret scan, fresh-model diff verifier)
        // needs minutes, not seconds.
        let bead_id = bead.id.clone();
        let telemetry = self.telemetry.clone();
        let timeout_secs = self.config.validation.outcome_timeout_seconds;

        match tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.handle(store, bead, output, was_interrupted),
        )
        .await
        {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => {
                // Handler returned an error.
                tracing::error!(
                    bead_id = %bead_id,
                    error = %e,
                    "outcome handler returned error"
                );
                Err(e)
            }
            Err(_) => {
                // Timeout after `timeout_secs` seconds.
                tracing::error!(
                    bead_id = %bead_id,
                    timeout_secs,
                    "outcome handler timed out, returning early to allow worker recovery"
                );
                // Emit a timeout event for observability.
                let _ = telemetry.emit(
                    EventKind::WorkerHandlingTimeout {
                        bead_id: bead_id.clone(),
                        outcome: classify(output.exit_code, was_interrupted, false)
                            .as_str()
                            .to_string(),
                        operation: "handle".to_string(),
                        error: format!("timeout after {}s", timeout_secs),
                    },
                    chrono::Utc::now(),
                );
                // Return an explicit error action. The worker must apply it,
                // which runs release recovery before the cycle can advance.
                // Verification never ran, so conservatively treat as unverified.
                Ok(HandlerResult {
                    outcome: classify(output.exit_code, was_interrupted, false),
                    bead_action: BeadAction::Errored,
                    telemetry_events: vec![],
                })
            }
        }
    }

    /// Success: verification already passed, now verify bead closure.
    ///
    /// CRITICAL: This is only called when verification PASSED.
    /// If verification failed, classification produces Failure and handle_failure
    /// is called instead. This ensures Success means verification passed.
    ///
    /// Flow:
    /// 1. If gates ran, emit VerificationPassed telemetry.
    /// 2. Check if agent closed the bead.
    ///    - Closed → emit BeadCompleted.
    ///    - Still open → emit BeadOrphaned warning.
    ///
    /// NEEDLE does NOT auto-close — the agent owns closure via `br close`.
    async fn handle_success(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        gate_report: Option<GateReport>,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::info!(bead_id = %bead.id, "agent completed successfully");

        // If gates ran and passed, emit telemetry.
        if let Some(report) = gate_report {
            let gates_run = report.results.len() as u32;
            self.telemetry.emit(
                EventKind::VerificationPassed {
                    bead_id: bead.id.clone(),
                    gates_run,
                },
                chrono::Utc::now(),
            )?;
            tracing::info!(
                bead_id = %bead.id,
                gates_run,
                "all validation gates passed"
            );

            // Check if workspace was degraded and restore it.
            let workspace_path = &bead.workspace;
            if let Ok(true) = gate_health::is_degraded(workspace_path) {
                tracing::info!(
                    workspace = %bead.workspace.display(),
                    bead_id = %bead.id,
                    "workspace was degraded — restoring after successful gate run"
                );

                if let Err(e) = self
                    .restore_degraded_workspace(store, workspace_path, &bead.id)
                    .await
                {
                    tracing::error!(
                        workspace = %bead.workspace.display(),
                        error = %e,
                        "failed to restore degraded workspace — manual intervention may be required"
                    );
                }
            }
        }

        // Normal success flow: check if agent closed the bead.
        let mut events = Vec::new();

        // Use timeout for show() to prevent indefinite hang in HANDLING state.
        match self.timeout_op(|| store.show(&bead.id), "show").await {
            Ok(Some(current)) if current.status.is_done() => {
                if self.config.worker.enforce_shipped_work {
                    match verify_shipped_work(&current, &bead.workspace, store).await {
                        Ok(crate::validation::GateResult::Fail(reason)) => {
                            tracing::warn!(
                                bead_id = %bead.id,
                                reason = %reason,
                                "bead closed but shipped-work check failed — reopening and releasing"
                            );
                            let report = GateReport::single_failure("shipped_work", reason);
                            return self.handle_gate_failure(store, bead, &report).await;
                        }
                        Ok(crate::validation::GateResult::Pass) => {
                            // Shipped work verified — but a bypass of the
                            // Definition-of-Done hook during this dispatch is
                            // still a failed gate, whatever the commit
                            // contains. Checked here rather than as a
                            // configured gate so it never depends on a commit
                            // message naming the bead; it routes through the
                            // same failure path and counts toward quarantine
                            // like any other gate failure.
                            let snapshot = predispatch::load(&bead.workspace, &bead.id).await;
                            match dod_bypass::check_dod_bypass(&bead.workspace, snapshot.as_ref())
                                .await
                            {
                                Ok(GateResult::Fail(reason)) => {
                                    tracing::warn!(
                                        bead_id = %bead.id,
                                        reason = %reason,
                                        "bead closed but a DoD bypass was recorded during \
                                         dispatch — failing the dispatch"
                                    );
                                    let report =
                                        GateReport::single_failure(dod_bypass::GATE_NAME, reason);
                                    return self.handle_gate_failure(store, bead, &report).await;
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!(
                                        bead_id = %bead.id,
                                        error = %e,
                                        "DoD bypass check errored — failing open"
                                    );
                                }
                            }

                            // Shipped-work check passed — reset failure count now that we're
                            // certain the bead is properly closed and shipped.
                            let _ = self.reset_failure_count(store, bead).await;
                        }
                        Ok(crate::validation::GateResult::ExecutionError { command, reason }) => {
                            // A gate that could not run is not a gate that
                            // failed (needle-4aaa010c): release without touching
                            // the failure count, so an unsatisfiable check —
                            // e.g. a workspace with no upstream configured
                            // (GitHub issue #18) — cannot burn the retry counter
                            // toward quarantine or feed mitosis on work the gate
                            // never judged.
                            tracing::warn!(
                                bead_id = %bead.id,
                                command = %command,
                                reason = %reason,
                                "shipped-work gate could not run — releasing without incrementing failure count"
                            );
                            return self
                                .handle_gate_error(
                                    store,
                                    bead,
                                    &bead.workspace.display().to_string(),
                                    "shipped_work",
                                    &command,
                                    &reason,
                                )
                                .await;
                        }
                        Err(e) => {
                            tracing::warn!(
                                bead_id = %bead.id,
                                error = %e,
                                "shipped-work check errored — failing open, NOT resetting failure count"
                            );
                            // CRITICAL: Do NOT reset failure count on error.
                            // A bead that closes repeatedly with errors (e.g., no snapshot,
                            // verification failures) must accumulate failures and eventually
                            // quarantine, not loop forever with a freshly-reset count each time.
                            // See GitHub issue #16 (bead needle-0fbf5145 cycled 14 times).
                        }
                    }
                } else {
                    // Shipped-work enforcement disabled — DO NOT reset failure count.
                    // Only reset when shipped-work verification PASSES. A bead that closes
                    // without verification must accumulate failures and quarantine, not loop
                    // forever with a reset count on every closure.
                    tracing::debug!(
                        bead_id = %bead.id,
                        "shipped-work enforcement disabled — leaving failure count as-is"
                    );
                }

                // Dispatch is fully accounted for — drop its snapshot so the
                // next claim of this bead starts from a fresh baseline.
                crate::validation::predispatch::clear(&bead.workspace, &bead.id).await;

                tracing::info!(bead_id = %bead.id, "bead confirmed closed by agent");
                events.push(EventKind::BeadCompleted {
                    bead_id: bead.id.clone(),
                    duration_ms: 0,
                });
                // Increment success_count for any skills that matched this bead.
                if !bead.workspace.as_os_str().is_empty() {
                    if let Ok(lib) = crate::skill::SkillLibrary::load(&bead.workspace) {
                        if let Err(e) = lib.increment_success_for_bead(&bead.labels, &bead.title) {
                            tracing::warn!(
                                bead_id = %bead.id,
                                error = %e,
                                "failed to increment skill success counts"
                            );
                        }
                    }
                }
            }
            Ok(Some(current)) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    status = %current.status,
                    "agent exited successfully but bead is still open (orphaned)"
                );
                events.push(EventKind::BeadOrphaned {
                    bead_id: bead.id.clone(),
                });

                // Use verify_shipped_work to decide close-vs-release.
                // If the agent shipped work, mark as completed. Otherwise, release
                // and increment failure count so repeat offenders quarantine.
                if self.config.worker.enforce_shipped_work {
                    match verify_shipped_work(&current, &bead.workspace, store).await {
                        Ok(crate::validation::GateResult::Pass) => {
                            tracing::info!(
                                bead_id = %bead.id,
                                "shipped work detected — marking orphaned bead as completed"
                            );
                            // Clear the predispatch snapshot since work is complete.
                            crate::validation::predispatch::clear(&bead.workspace, &bead.id).await;
                            // Shipped work detected — reset failure count.
                            let _ = self.reset_failure_count(store, bead).await;
                            // Mark as completed - the agent shipped work but forgot to close.
                            // We can't close via bead store (no close method), so emit completion
                            // event and release. The bead remains open but work is done.
                            events.push(EventKind::BeadCompleted {
                                bead_id: bead.id.clone(),
                                duration_ms: 0,
                            });
                            let mut release_events =
                                self.prepare_release_events(store, bead).await?;
                            events.append(&mut release_events);
                            return Ok((BeadAction::Released, events));
                        }
                        Ok(crate::validation::GateResult::Fail(reason)) => {
                            tracing::warn!(
                                bead_id = %bead.id,
                                reason = %reason,
                                "no shipped work detected — releasing orphaned bead with failure increment"
                            );
                            // Release and increment failure count to apply quarantine.
                            let mut release_events =
                                self.prepare_release_events(store, bead).await?;
                            events.append(&mut release_events);
                            // The release itself now happens later in the worker's apply_bead_action(),
                            // so no BeadReleased event exists here. Testing for one made this always
                            // false, silently disabling the failure-count/quarantine follow-up. Treat
                            // the prepare step as successful when it reported no error.
                            let release_succeeded = !events
                                .iter()
                                .any(|e| matches!(e, EventKind::WorkerHandlingTimeout { .. }));
                            if release_succeeded {
                                let _ = self.increment_failure_count(store, bead).await;
                            }
                            return Ok((BeadAction::Released, events));
                        }
                        Ok(crate::validation::GateResult::ExecutionError { command, reason }) => {
                            tracing::warn!(
                                bead_id = %bead.id,
                                command = %command,
                                reason = %reason,
                                "shipped-work gate execution error — releasing without incrementing failure count"
                            );
                            // Release without incrementing failure count
                            let mut release_events =
                                self.prepare_release_events(store, bead).await?;
                            events.append(&mut release_events);
                            return Ok((BeadAction::Released, events));
                        }
                        Err(e) => {
                            tracing::warn!(
                                bead_id = %bead.id,
                                error = %e,
                                "shipped-work check errored — releasing orphaned bead with failure increment"
                            );
                            // On error, release and increment failure count.
                            let mut release_events =
                                self.prepare_release_events(store, bead).await?;
                            events.append(&mut release_events);
                            // The release itself now happens later in the worker's apply_bead_action(),
                            // so no BeadReleased event exists here. Testing for one made this always
                            // false, silently disabling the failure-count/quarantine follow-up. Treat
                            // the prepare step as successful when it reported no error.
                            let release_succeeded = !events
                                .iter()
                                .any(|e| matches!(e, EventKind::WorkerHandlingTimeout { .. }));
                            if release_succeeded {
                                let _ = self.increment_failure_count(store, bead).await;
                            }
                            return Ok((BeadAction::Released, events));
                        }
                    }
                } else {
                    tracing::warn!(
                        bead_id = %bead.id,
                        "enforce_shipped_work disabled — releasing orphaned bead"
                    );
                    // If enforce_shipped_work is disabled, just release without closing.
                    let mut release_events = self.prepare_release_events(store, bead).await?;
                    events.append(&mut release_events);
                    return Ok((BeadAction::Released, events));
                }
            }
            Ok(None) => {
                // Timeout - we cannot verify bead closure, so release to enforce postcondition.
                tracing::warn!(
                    bead_id = %bead.id,
                    "show() timed out, releasing bead to enforce postcondition"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "success".to_string(),
                    operation: "show".to_string(),
                    error: "timeout after 30s".to_string(),
                });
                let mut release_events = self.prepare_release_events(store, bead).await?;
                events.append(&mut release_events);
                return Ok((BeadAction::Released, events));
            }
            Err(e) => {
                // Error - we cannot verify bead closure, so release to enforce postcondition.
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "show() failed, releasing bead to enforce postcondition"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "success".to_string(),
                    operation: "show".to_string(),
                    error: e.to_string(),
                });
                let mut release_events = self.prepare_release_events(store, bead).await?;
                events.append(&mut release_events);
                return Ok((BeadAction::Released, events));
            }
        }

        // Completion requires a durable checkpoint. A workspace sync failure
        // must not emit the queued BeadCompleted event or retry finished work.
        match self.timeout_op(|| store.flush(), "flush").await {
            Ok(Some(())) => {
                tracing::debug!(bead_id = %bead.id, "flushed bead state to JSONL after success");
            }
            Ok(None) => {
                store.pause_workspace("checkpoint publication timed out after success".to_string());
                anyhow::bail!(
                    "checkpoint publication timed out after success; completion is unverified"
                );
            }
            Err(e) => {
                store.pause_workspace(format!(
                    "checkpoint publication failed after success: {e:#}"
                ));
                return Err(e.context(
                    "checkpoint publication failed after success; completion is unverified",
                ));
            }
        }

        Ok((BeadAction::Closed, events))
    }

    /// Handle gate failure: reopen the bead if it was closed, then release it.
    async fn handle_gate_failure(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        report: &crate::validation::GateReport,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        // Find the first failing gate for telemetry.
        let (failed_gate, reason) = report
            .results
            .iter()
            .find(|(_, r)| !r.passed())
            .map(|(name, r)| (name.clone(), r.failure_reason().unwrap_or("").to_string()))
            .unwrap_or_else(|| ("unknown".to_string(), "unknown error".to_string()));

        tracing::warn!(
            bead_id = %bead.id,
            gate = %failed_gate,
            reason = %reason,
            "validation gate failed — releasing bead"
        );

        // Emit verification failure telemetry.
        self.telemetry.emit(
            EventKind::VerificationFailed {
                bead_id: bead.id.clone(),
                command: failed_gate.clone(),
                exit_code: None,
                output: reason,
            },
            chrono::Utc::now(),
        )?;

        let mut events = Vec::new();

        // If the agent already closed the bead, reopen it before releasing.
        // Use timeout to prevent indefinite hang in HANDLING state.
        match self.timeout_op(|| store.show(&bead.id), "show").await {
            Ok(Some(current)) if current.status.is_done() => {
                tracing::info!(
                    bead_id = %bead.id,
                    "reopening bead closed by agent (verification failed)"
                );
                match self.timeout_op(|| store.reopen(&bead.id), "reopen").await {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            error = %e,
                            "failed to reopen bead — attempting release anyway"
                        );
                        events.push(EventKind::WorkerHandlingTimeout {
                            bead_id: bead.id.clone(),
                            outcome: "gate_failure".to_string(),
                            operation: "reopen".to_string(),
                            error: e.to_string(),
                        });
                    }
                }
            }
            Ok(None) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    "show() timed out during gate failure handling, skipping reopen"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "gate_failure".to_string(),
                    operation: "show".to_string(),
                    error: "timeout after 30s".to_string(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "show() failed during gate failure handling"
                );
            }
            _ => {}
        }

        // Release the bead back to open with flush-before-release and sync recovery.
        let mut release_events = self.prepare_release_events(store, bead).await?;
        events.append(&mut release_events);

        // If release succeeded, increment the failure count and apply the same
        // quarantine ceiling `handle_failure` uses. Without this, a bead that
        // fails a gate every cycle is released back to open forever: the
        // ARMOR/bf-135k storm ran one bead 24 times in a single day, each
        // attempt leaving another commit behind. A gate failure is no less
        // repeatable than an agent failure and must respect the same ceiling.
        // The release itself now happens later in the worker's apply_bead_action(),
        // so no BeadReleased event exists here. Testing for one made this always
        // false, silently disabling the failure-count/quarantine follow-up. Treat
        // the prepare step as successful when it reported no error.
        let release_succeeded = !events
            .iter()
            .any(|e| matches!(e, EventKind::WorkerHandlingTimeout { .. }));
        let mut action = BeadAction::Released;
        if release_succeeded {
            match self.increment_failure_count(store, bead).await {
                Ok(new_count) => {
                    let threshold = self.config.outcome.quarantine_after_failures;
                    if threshold > 0 && new_count >= threshold {
                        match self
                            .quarantine_bead(store, bead, new_count, threshold)
                            .await
                        {
                            Ok(quarantine_events) => {
                                events.extend(quarantine_events);
                                action = BeadAction::Quarantined;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    bead_id = %bead.id,
                                    error = %e,
                                    "failed to quarantine bead after exceeding gate-failure threshold"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %e,
                        "failed to increment failure count after gate failure"
                    );
                }
            }
        }

        // Add a label indicating verification failure.
        if let Err(e) = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            store.add_label(&bead.id, "verification-failed"),
        )
        .await
        {
            tracing::warn!(
                bead_id = %bead.id,
                error = %e,
                "failed to add verification-failed label"
            );
        }

        Ok((action, events))
    }

    /// Handle gate execution error: gate could not run (ENOENT/EACCES/missing directory/timeout).
    ///
    /// This is distinct from `handle_gate_failure`: a gate that ran and failed verification
    /// is handled by `handle_gate_failure`, while a gate that could not run at all is
    /// handled here. Gate errors release the bead WITHOUT incrementing the failure count
    /// or adding the `cycling` label.
    ///
    /// # Arguments
    ///
    /// * `store` - Bead store for state operations
    /// * `bead` - The bead to handle
    /// * `workspace` - The workspace path (for telemetry)
    /// * `gate_name` - Name of the gate that failed
    /// * `command` - The command that could not run
    /// * `reason` - Human-readable error reason (e.g., "ENOENT", "EACCES", "directory not found")
    async fn handle_gate_error(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        workspace: &str,
        gate_name: &str,
        command: &str,
        reason: &str,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::warn!(
            bead_id = %bead.id,
            workspace,
            gate = %gate_name,
            command = %command,
            reason = %reason,
            "gate execution error — releasing bead without incrementing failure count"
        );

        let mut events = Vec::new();

        // Emit gate execution error telemetry.
        self.telemetry.emit(
            EventKind::GateExecutionError {
                bead_id: bead.id.clone(),
                workspace: workspace.to_string(),
                gate: gate_name.to_string(),
                command: command.to_string(),
                reason: reason.to_string(),
            },
            chrono::Utc::now(),
        )?;

        // Record the error in gate health state and check if workspace is degraded.
        let workspace_path = PathBuf::from(workspace);
        match gate_health::record_error(&workspace_path, command.to_string(), reason.to_string()) {
            Ok((previous_state, now_degraded)) => {
                if now_degraded {
                    tracing::error!(
                        workspace = %workspace,
                        gate = %gate_name,
                        command = %command,
                        reason = %reason,
                        consecutive_errors = previous_state.as_ref().map(|s| s.consecutive_errors + 1).unwrap_or(1),
                        "workspace degraded after 3 consecutive gate execution errors"
                    );

                    // Create the "Gate broken" alert bead with fingerprint deduplication.
                    if let Err(e) = self
                        .create_gate_broken_bead(
                            store,
                            workspace,
                            gate_name,
                            command,
                            reason,
                            previous_state.as_ref(),
                        )
                        .await
                    {
                        tracing::error!(
                            error = %e,
                            workspace = %workspace,
                            "failed to create Gate broken alert bead for degraded workspace"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    workspace = %workspace,
                    "failed to record gate health state — degradation tracking may be inaccurate"
                );
            }
        }

        // Release the bead without incrementing failure count.
        let mut release_events = self.prepare_release_events(store, bead).await?;
        events.append(&mut release_events);

        // NOTE: We do NOT increment the failure count for gate execution errors.
        // The gate never ran, so this is not a failure of the work — it's a
        // configuration or environment issue that should be fixed before retry.

        Ok((BeadAction::Released, events))
    }

    /// Create a "Gate broken" alert bead when workspace degrades.
    ///
    /// This method creates a P0 bead with fingerprinting to prevent duplicates.
    /// The bead remains claimable - fixing a gate is verified by running it.
    async fn create_gate_broken_bead(
        &self,
        store: &dyn BeadStore,
        workspace: &str,
        gate_name: &str,
        command: &str,
        reason: &str,
        previous_state: Option<&crate::gate_health::GateHealthState>,
    ) -> Result<()> {
        use crate::fingerprint::{build_alert_labels, compute_fingerprint};

        let cause = format!("gate={}, command={}, reason={}", gate_name, command, reason);
        let fingerprint = compute_fingerprint(workspace, &AlertKind::GateBroken, &cause);

        // Check for existing beads with the same fingerprint
        let dedup_result =
            check_alert_deduplication(store, workspace, &AlertKind::GateBroken, &cause)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        error = %e,
                        workspace,
                        "Failed to check Gate broken alert deduplication, proceeding with creation"
                    );
                    AlertDeduplication::CreateNew
                });

        match dedup_result {
            AlertDeduplication::Deduplicated { bead_id, .. } => {
                tracing::info!(
                    bead_id = %bead_id,
                    fingerprint = %fingerprint,
                    workspace,
                    "Gate broken alert deduplicated - existing bead already open"
                );

                // Emit telemetry for the existing bead
                self.telemetry.emit(
                    EventKind::WorkspaceGateDegraded {
                        workspace: workspace.to_string(),
                        gate: gate_name.to_string(),
                        command: command.to_string(),
                        reason: reason.to_string(),
                        consecutive_errors: previous_state
                            .map(|s| s.consecutive_errors)
                            .unwrap_or(0),
                        bead_id: bead_id.clone(),
                    },
                    Utc::now(),
                )?;

                Ok(())
            }
            AlertDeduplication::Suppressed { bead_id, closed_at } => {
                tracing::info!(
                    bead_id = %bead_id,
                    closed_at = %closed_at,
                    fingerprint = %fingerprint,
                    workspace,
                    "Gate broken alert suppressed - bead was closed within 24h"
                );

                // Still emit telemetry even though we're not creating a bead
                self.telemetry.emit(
                    EventKind::WorkspaceGateDegraded {
                        workspace: workspace.to_string(),
                        gate: gate_name.to_string(),
                        command: command.to_string(),
                        reason: reason.to_string(),
                        consecutive_errors: previous_state
                            .map(|s| s.consecutive_errors)
                            .unwrap_or(0),
                        bead_id: bead_id.clone(),
                    },
                    Utc::now(),
                )?;

                Ok(())
            }
            AlertDeduplication::CreateNew => {
                // Create the new bead
                let bead_title = format!("Gate broken: {} — {}", command, reason);
                let bead_body = format!(
                    "## Gate Execution Error\n\
                     \n\
                     The gate command `{}` failed to execute in workspace `{}`.\n\
                     \n\
                     ### Error Details\n\
                     - **Gate**: {}\n\
                     - **Command**: `{}`\n\
                     - **Reason**: {}\n\
                     - **Consecutive errors**: {}\n\
                     \n\
                     ### Impact\n\
                     This workspace is now **degraded**. Pluck and Explore strands will skip it for ordinary dispatch.\n\
                     The workspace remains claimable for manual intervention or fixing this specific gate.\n\
                     \n\
                     ### Resolution\n\
                     Fix the gate command (e.g., install missing dependency, correct path, resolve permissions) \n\
                     and the workspace will be automatically restored on the next successful gate run.\n\
                     \n\
                     ### Acceptance Criteria\n\
                     - [ ] Gate command runs successfully\n\
                     - [ ] No stderr errors (ENOENT, EACCES, timeout)\n\
                     - [ ] Exit code is 0\n\
                     \n\
                     ### Verification\n\
                     The gate command will be re-run on the next dispatch attempt. A successful run will:\n\
                     - Clear the degraded state\n\
                     - Close this bead automatically\n\
                     - Restore normal workspace operation\n",
                    command, workspace, gate_name, command, reason,
                    previous_state.map(|s| s.consecutive_errors.to_string()).unwrap_or_else(|| "unknown".to_string())
                );

                let labels = build_alert_labels(&fingerprint, &["infra", "priority:0"]);
                let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();

                let bead_id = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    store.create_bead(&bead_title, &bead_body, &label_refs),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!("create_bead timed out after 30s during Gate broken handling")
                })?
                .context("failed to create Gate broken bead")?;

                tracing::info!(
                    bead_id = %bead_id,
                    fingerprint = %fingerprint,
                    workspace,
                    "Created Gate broken alert bead for degraded workspace"
                );

                // Emit telemetry for the new bead
                self.telemetry.emit(
                    EventKind::WorkspaceGateDegraded {
                        workspace: workspace.to_string(),
                        gate: gate_name.to_string(),
                        command: command.to_string(),
                        reason: reason.to_string(),
                        consecutive_errors: previous_state
                            .map(|s| s.consecutive_errors)
                            .unwrap_or(0),
                        bead_id: bead_id.clone(),
                    },
                    Utc::now(),
                )?;

                Ok(())
            }
        }
    }

    /// Restore a degraded workspace after successful gate run.
    ///
    /// This method:
    /// 1. Clears the gate health state
    /// 2. Finds the associated "Gate broken" bead by fingerprint
    /// 3. Closes the bead with a reason
    /// 4. Emits workspace.gate_restored telemetry
    async fn restore_degraded_workspace(
        &self,
        store: &dyn BeadStore,
        workspace_path: &std::path::Path,
        success_bead_id: &BeadId,
    ) -> Result<()> {
        // Get the previous state before clearing
        let previous_state = gate_health::clear_state(workspace_path).unwrap_or(None);

        let degraded_duration_secs = if let Some(ref state) = previous_state {
            let last_error = chrono::DateTime::parse_from_rfc3339(&state.last_error_at)
                .unwrap_or_else(|_| chrono::Utc::now().into());
            let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
            (now - last_error).num_seconds().max(0) as u64
        } else {
            0
        };

        // Find the Gate broken bead for this workspace
        let workspace = workspace_path.to_string_lossy().to_string();

        // We need to find beads with the gate-broken alert kind in this workspace
        let all_beads = store
            .list_all()
            .await
            .context("failed to list beads while restoring degraded workspace")?;

        // Find Gate broken beads in this workspace
        let gate_broken_beads: Vec<&Bead> = all_beads
            .iter()
            .filter(|b| {
                b.workspace == workspace_path
                    && b.title.starts_with("Gate broken:")
                    && b.status != BeadStatus::Closed
            })
            .collect();

        // Close each Gate broken bead found
        for bead in &gate_broken_beads {
            tracing::info!(
                bead_id = %bead.id,
                workspace = %workspace,
                "Closing Gate broken bead after workspace restoration"
            );

            let close_reason = format!(
                "Workspace restored after successful gate run in bead {}. \
                 Gates are now functioning correctly.",
                success_bead_id
            );

            // Close the bead
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                store.close(&bead.id, &close_reason),
            )
            .await
            .map_err(|_| anyhow::anyhow!("close timed out after 30s during workspace restoration"))?
            .context("failed to close Gate broken bead during workspace restoration")?;

            // Emit restoration telemetry
            self.telemetry.emit(
                EventKind::WorkspaceGateRestored {
                    workspace: workspace.clone(),
                    bead_id: bead.id.clone(),
                    degraded_duration_secs,
                },
                Utc::now(),
            )?;
        }

        tracing::info!(
            workspace = %workspace,
            beads_closed = gate_broken_beads.len(),
            degraded_duration_secs,
            "Successfully restored degraded workspace"
        );

        Ok(())
    }

    /// Failure: release bead and increment failure count.
    ///
    /// Mitosis evaluation (for multi-task splitting) is handled externally by
    /// the worker after outcome handling — see `MitosisEvaluator`.
    ///
    /// If br calls timeout or fail, logs the error and continues — does not
    /// block the worker in HANDLING state indefinitely.
    async fn handle_failure(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::warn!(bead_id = %bead.id, "agent failure — releasing bead");

        let mut events = self.prepare_release_events(store, bead).await?;

        // If release succeeded, increment failure count and check the
        // quarantine threshold. A bead that has exceeded
        // `outcome.quarantine_after_failures` consecutive failures is
        // quarantined (status=blocked) instead of being left released-to-open
        // for the next cycle to re-claim and fail again indefinitely. This
        // also closes the mitosis `NotSplittable` fallthrough (worker/mod.rs):
        // that verdict no longer matters for beads at or past the ceiling,
        // since this check already ran before mitosis evaluation this cycle.
        // The release itself now happens later in the worker's apply_bead_action(),
        // so no BeadReleased event exists here. Testing for one made this always
        // false, silently disabling the failure-count/quarantine follow-up. Treat
        // the prepare step as successful when it reported no error.
        let release_succeeded = !events
            .iter()
            .any(|e| matches!(e, EventKind::WorkerHandlingTimeout { .. }));
        let mut action = BeadAction::Released;
        if release_succeeded {
            match self.increment_failure_count(store, bead).await {
                Ok(new_count) => {
                    let threshold = self.config.outcome.quarantine_after_failures;
                    if threshold > 0 && new_count >= threshold {
                        match self
                            .quarantine_bead(store, bead, new_count, threshold)
                            .await
                        {
                            Ok(quarantine_events) => {
                                events.extend(quarantine_events);
                                action = BeadAction::Quarantined;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    bead_id = %bead.id,
                                    error = %e,
                                    "failed to quarantine bead after exceeding failure threshold"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %e,
                        "failed to increment failure count after release"
                    );
                }
            }
        }

        // Ensure we emit a reason event for telemetry.
        if !events.iter().any(|e| {
            matches!(
                e,
                EventKind::BeadReleased { .. } | EventKind::BeadReleaseFailed { .. }
            )
        }) {
            events.push(EventKind::BeadReleased {
                bead_id: bead.id.clone(),
                reason: "failure".to_string(),
            });
        }

        Ok((action, events))
    }

    /// Timeout: release bead and add `deferred` label.
    ///
    /// If br calls timeout or fail, logs the error and continues — does not
    /// block the worker in HANDLING state indefinitely.
    async fn handle_timeout(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::warn!(bead_id = %bead.id, "agent timed out — releasing bead as deferred");

        let mut events = self.prepare_release_events(store, bead).await?;

        // If release succeeded, increment failure count and add deferred label.
        // The release itself now happens later in the worker's apply_bead_action(),
        // so no BeadReleased event exists here. Testing for one made this always
        // false, silently disabling the failure-count/quarantine follow-up. Treat
        // the prepare step as successful when it reported no error.
        let release_succeeded = !events
            .iter()
            .any(|e| matches!(e, EventKind::WorkerHandlingTimeout { .. }));
        if release_succeeded {
            // Increment failure count for auto-split tracking.
            if let Err(e) = self.increment_failure_count(store, bead).await {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "failed to increment failure count after timeout"
                );
            }

            if let Err(e) = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                store.add_label(&bead.id, "deferred"),
            )
            .await
            {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "add_label deferred timed out or failed after timeout release"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "timeout".to_string(),
                    operation: "add_label".to_string(),
                    error: e.to_string(),
                });
            }
        }

        Ok((BeadAction::Deferred, events))
    }

    /// Crash: release bead and create alert bead with diagnostic info.
    async fn handle_crash(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        signal_code: i32,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::error!(
            bead_id = %bead.id,
            signal_code,
            agent = %self.config.agent.default,
            "agent crashed — releasing bead and creating alert"
        );

        let events = self.prepare_release_events(store, bead).await?;

        // Create alert bead with diagnostic info (best-effort).
        let signal_num = if signal_code > 128 {
            signal_code - 128
        } else {
            signal_code
        };
        let timestamp = Utc::now().to_rfc3339();
        let alert_title = format!("ALERT: Agent crash on bead {}", bead.id);

        // Check for deduplication using fingerprint
        let workspace = bead.workspace.display().to_string();
        let cause = format!(
            "bead={}, agent={}, signal={}, exit_code={}",
            bead.id, self.config.agent.default, signal_num, signal_code
        );

        let dedup_result = check_alert_deduplication(store, &workspace, &AlertKind::Crash, &cause)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    bead_id = %bead.id,
                    "Failed to check crash alert deduplication, proceeding with creation"
                );
                AlertDeduplication::CreateNew
            });

        match dedup_result {
            AlertDeduplication::Deduplicated {
                bead_id,
                fingerprint,
            } => {
                let note = format!(
                    "Crash recurred for bead {}: signal={}, exit_code={}, agent={}, timestamp={}",
                    bead.id, signal_num, signal_code, self.config.agent.default, timestamp
                );
                let _ = append_alert_note(store, &bead_id, &note).await;

                tracing::info!(
                    bead_id = %bead.id,
                    crash_alert_bead_id = %bead_id,
                    fingerprint = %fingerprint,
                    signal = signal_num,
                    "Crash alert deduplicated - appended note to existing bead"
                );
            }
            AlertDeduplication::Suppressed { bead_id, closed_at } => {
                tracing::info!(
                    bead_id = %bead.id,
                    crash_alert_bead_id = %bead_id,
                    closed_at = %closed_at,
                    "Crash alert suppressed - alert was closed within 24h"
                );
            }
            AlertDeduplication::CreateNew => {
                let alert_body = format!(
                    "## Agent Crash Report\n\
                     \n\
                     - **Bead ID**: {}\n\
                     - **Agent**: {}\n\
                     - **Exit code**: {} (signal {})\n\
                     - **Workspace**: {}\n\
                     - **Timestamp**: {}\n\
                     \n\
                     The agent process was killed. This bead has been released for retry.",
                    bead.id,
                    self.config.agent.default,
                    signal_code,
                    signal_num,
                    bead.workspace.display(),
                    timestamp,
                );

                let fingerprint =
                    crate::fingerprint::compute_fingerprint(&workspace, &AlertKind::Crash, &cause);

                // Hook 4: propagate stitch labels from the crashed bead to the alert.
                let signal_label = format!("signal-{}", signal_num);
                let alert_labels =
                    build_alert_labels(&fingerprint, &["alert", "crash", &signal_label]);

                // Add stitch labels
                let mut final_labels = alert_labels;
                final_labels.extend(crate::types::extract_stitch_labels(&bead.labels));
                let alert_label_refs: Vec<&str> = final_labels.iter().map(|s| s.as_str()).collect();

                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    store.create_bead(&alert_title, &alert_body, &alert_label_refs),
                )
                .await
                {
                    Ok(Ok(alert_id)) => {
                        tracing::info!(
                            bead_id = %bead.id,
                            %alert_id,
                            fingerprint = %fingerprint,
                            "crash alert bead created"
                        );
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            error = %e,
                            "failed to create crash alert bead"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            "create_bead timed out after 30s during crash handling"
                        );
                    }
                }
            }
        }

        Ok((BeadAction::Alerted, events))
    }

    /// AgentNotFound: release bead, emit error. No retry — this is a config issue.
    async fn handle_agent_not_found(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::error!(
            bead_id = %bead.id,
            agent = %self.config.agent.default,
            "agent binary not found — releasing bead (config issue, no retry)"
        );

        let events = self.prepare_release_events(store, bead).await?;
        Ok((BeadAction::Released, events))
    }

    /// Interrupted: release bead for graceful shutdown.
    async fn handle_interrupted(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::info!(bead_id = %bead.id, "agent interrupted — releasing bead for clean shutdown");

        let events = self.prepare_release_events(store, bead).await?;
        Ok((BeadAction::Interrupted, events))
    }

    /// Increment the failure count label on a bead.
    ///
    /// Labels follow the pattern `failure-count:N`. If `failure-count:2` exists,
    /// the old label is removed and `failure-count:3` is added.
    ///
    /// Returns the new failure count (or 0 if the operation failed).
    ///
    /// All `br` calls are wrapped in timeouts to prevent indefinite hang in
    /// HANDLING state. Failures are non-fatal — we log and continue.
    async fn increment_failure_count(&self, store: &dyn BeadStore, bead: &Bead) -> Result<u32> {
        // Read labels with timeout.
        let labels =
            match tokio::time::timeout(std::time::Duration::from_secs(30), store.labels(&bead.id))
                .await
            {
                Ok(Ok(l)) => l,
                Ok(Err(e)) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %e,
                        "could not read labels to increment failure count"
                    );
                    return Ok(0);
                }
                Err(_) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        "labels() timed out after 30s, skipping failure count increment"
                    );
                    return Ok(0);
                }
            };

        let current_count = labels
            .iter()
            .filter_map(|l| l.strip_prefix("failure-count:"))
            .filter_map(|n| n.parse::<u32>().ok())
            .max()
            .unwrap_or(0);

        let new_count = current_count + 1;
        let new_label = format!("failure-count:{}", new_count);

        // Remove old failure-count labels before adding the new one.
        // Each remove_label call is wrapped in a timeout.
        for label in &labels {
            if label.starts_with("failure-count:") {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    store.remove_label(&bead.id, label),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            label,
                            error = %e,
                            "failed to remove old failure-count label"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            label,
                            "remove_label timed out after 30s"
                        );
                    }
                }
            }
        }

        // Add the new label with timeout.
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            store.add_label(&bead.id, &new_label),
        )
        .await
        {
            Ok(Ok(())) => {
                tracing::debug!(
                    bead_id = %bead.id,
                    count = new_count,
                    "failure count incremented"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "failed to add failure-count label"
                );
            }
            Err(_) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    "add_label timed out after 30s"
                );
            }
        }

        Ok(new_count)
    }

    /// Reset the failure count label on a bead by removing all `failure-count:N` labels.
    ///
    /// Called on success to clear the failure counter so the bead starts fresh
    /// on the next cycle.
    ///
    /// All `br` calls are wrapped in timeouts to prevent indefinite hang in
    /// HANDLING state. Failures are non-fatal — we log and continue.
    async fn reset_failure_count(&self, store: &dyn BeadStore, bead: &Bead) -> Result<()> {
        // Read labels with timeout.
        let labels =
            match tokio::time::timeout(std::time::Duration::from_secs(30), store.labels(&bead.id))
                .await
            {
                Ok(Ok(l)) => l,
                Ok(Err(e)) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %e,
                        "could not read labels to reset failure count"
                    );
                    return Ok(());
                }
                Err(_) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        "labels() timed out after 30s, skipping failure count reset"
                    );
                    return Ok(());
                }
            };

        // Remove all failure-count labels.
        let mut removed_count = 0;
        for label in &labels {
            if label.starts_with("failure-count:") {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    store.remove_label(&bead.id, label),
                )
                .await
                {
                    Ok(Ok(())) => {
                        removed_count += 1;
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            label,
                            error = %e,
                            "failed to remove failure-count label"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            label,
                            "remove_label timed out after 30s"
                        );
                    }
                }
            }
        }

        if removed_count > 0 {
            tracing::debug!(
                bead_id = %bead.id,
                removed_count,
                "failure count reset"
            );
        }

        Ok(())
    }

    /// Quarantine a bead by setting it deferred with a quarantine reason label.
    ///
    /// This is called when a bead exceeds the configured failure threshold.
    /// Emits a BeadQuarantined telemetry event and a FalseCloseDetected event.
    ///
    /// All `br` calls are wrapped in timeouts to prevent indefinite hang in
    /// HANDLING state. Failures are non-fatal — we log and continue.
    async fn quarantine_bead(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        failure_count: u32,
        threshold: u32,
    ) -> Result<Vec<EventKind>> {
        let mut events = Vec::new();

        tracing::warn!(
            bead_id = %bead.id,
            failure_count,
            threshold,
            "quarantining bead after exceeding failure threshold"
        );

        // Add 'deferred' label to stop Pluck from re-selecting the bead.
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            store.add_label(&bead.id, "deferred"),
        )
        .await
        {
            Ok(Ok(())) => {
                tracing::debug!(
                    bead_id = %bead.id,
                    "added 'deferred' label"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "failed to add 'deferred' label during quarantine"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "quarantine".to_string(),
                    operation: "add_label".to_string(),
                    error: e.to_string(),
                });
            }
            Err(_) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    "add_label timed out after 30s during quarantine"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "quarantine".to_string(),
                    operation: "add_label".to_string(),
                    error: "timeout after 30s".to_string(),
                });
            }
        }

        // Add a quarantine reason label with the failure count and context.
        let reason_label = format!(
            "quarantine: false-close-detected-after-{}-tries",
            failure_count
        );
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            store.add_label(&bead.id, &reason_label),
        )
        .await
        {
            Ok(Ok(())) => {
                tracing::debug!(
                    bead_id = %bead.id,
                    "added quarantine reason label"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "failed to add quarantine reason label"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "quarantine".to_string(),
                    operation: "add_label".to_string(),
                    error: e.to_string(),
                });
            }
            Err(_) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    "add_label timed out after 30s"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "quarantine".to_string(),
                    operation: "add_label".to_string(),
                    error: "timeout after 30s".to_string(),
                });
            }
        }

        // Emit the BeadQuarantined event.
        // The legacy quarantine path has no rounds ladder or expiry yet — that
        // is the expiring-quarantine work (needle-40c6c60e). Report round 1
        // with no expiry rather than blocking on fields this path cannot fill.
        events.push(EventKind::BeadQuarantined {
            bead_id: bead.id.clone(),
            round: 1,
            until: String::new(),
            failure_count,
        });

        // Emit the FalseCloseDetected event for observability.
        events.push(EventKind::FalseCloseDetected {
            bead_id: bead.id.clone(),
            failure_count,
            threshold,
            reason: "shipped-work-verification-failed".to_string(),
        });

        Ok(events)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Outcome Display
// ──────────────────────────────────────────────────────────────────────────────

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Success => write!(f, "Success"),
            Outcome::Failure => write!(f, "Failure"),
            Outcome::Timeout => write!(f, "Timeout"),
            Outcome::AgentNotFound => write!(f, "AgentNotFound"),
            Outcome::Interrupted => write!(f, "Interrupted"),
            Outcome::Crash(code) => write!(f, "Crash({})", code),
            Outcome::GateError => write!(f, "GateError"),
            Outcome::GateUnsatisfiable => write!(f, "GateUnsatisfiable"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::Filters;
    use crate::config::ValidationConfig;
    use crate::telemetry::Sink;
    use crate::types::{BeadId, ClaimResult};
    use async_trait::async_trait;
    use chrono::Utc;
    use std::path::PathBuf;
    use std::sync::Mutex;

    // ── Mock BeadStore ──

    #[derive(Debug, Clone)]
    #[allow(dead_code)] // Fields read via pattern matching in test assertions
    enum StoreAction {
        Release(String),
        Block(String),
        Reopen(String),
        AddLabel(String, String),
        RemoveLabel(String, String),
        Show(String),
        CreateBead(String, String),
        AddDependency(String, String),
    }

    struct MockBeadStore {
        actions: Mutex<Vec<StoreAction>>,
        show_status: BeadStatus,
        labels: Vec<String>,
        fail_flush: bool,
    }

    impl MockBeadStore {
        fn new(show_status: BeadStatus) -> Self {
            MockBeadStore {
                actions: Mutex::new(Vec::new()),
                show_status,
                labels: Vec::new(),
                fail_flush: false,
            }
        }

        fn with_labels(mut self, labels: Vec<String>) -> Self {
            self.labels = labels;
            self
        }

        fn actions(&self) -> Vec<StoreAction> {
            self.actions.lock().unwrap().clone()
        }
    }

    fn test_bead(status: BeadStatus) -> Bead {
        Bead {
            id: BeadId::from("needle-test"),
            title: "Test bead".to_string(),
            body: Some("Test body".to_string()),
            priority: 1,
            status,
            assignee: Some("worker-01".to_string()),
            labels: vec![],
            workspace: PathBuf::from("/tmp"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn test_store(status: BeadStatus) -> MockBeadStore {
        MockBeadStore::new(status)
    }

    #[async_trait]
    impl BeadStore for MockBeadStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::Show(id.to_string()));
            Ok(test_bead(self.show_status.clone()))
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "mock".to_string(),
            })
        }

        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "mock".to_string(),
            })
        }

        async fn release(&self, id: &BeadId) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::Release(id.to_string()));
            Ok(())
        }
        async fn block(&self, id: &BeadId) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::Block(id.to_string()));
            Ok(())
        }
        async fn flush(&self) -> Result<()> {
            if self.fail_flush {
                anyhow::bail!("fixture checkpoint publication failure");
            }
            Ok(())
        }
        async fn reopen(&self, id: &BeadId) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::Reopen(id.to_string()));
            Ok(())
        }
        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            Ok(self.labels.clone())
        }
        async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::AddLabel(id.to_string(), label.to_string()));
            Ok(())
        }
        async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::RemoveLabel(id.to_string(), label.to_string()));
            Ok(())
        }
        async fn create_bead(&self, title: &str, body: &str, _labels: &[&str]) -> Result<BeadId> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::CreateBead(title.to_string(), body.to_string()));
            Ok(BeadId::from("alert-001"))
        }
        async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
            Ok(crate::bead_store::RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<crate::bead_store::RepairReport> {
            Ok(crate::bead_store::RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::AddDependency(
                    blocker_id.to_string(),
                    blocked_id.to_string(),
                ));
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }

        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        fn has_valid_store(&self) -> bool {
            true // Mock store always has a valid store
        }
    }

    struct NopSink;

    impl Sink for NopSink {
        fn accept(&self, _event: &crate::telemetry::TelemetryEvent) -> Result<()> {
            Ok(())
        }
        fn flush(&self, _deadline: std::time::Duration) -> Result<()> {
            Ok(())
        }
    }

    fn test_handler() -> OutcomeHandler {
        test_handler_with_config(Config::default())
    }

    /// Test handler with shipped-work enforcement disabled for tests that
    /// don't specifically test the shipped-work gate. The mock store doesn't
    /// provide predispatch snapshots, so the check would always fail and
    /// interfere with the test's actual purpose.
    fn test_handler_without_shipped_work() -> OutcomeHandler {
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        test_handler_with_config(config)
    }

    fn test_output(exit_code: i32) -> AgentOutcome {
        AgentOutcome {
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    // ── classify tests ──

    #[test]
    fn classify_was_interrupted_always_returns_interrupted() {
        assert_eq!(classify(0, true, true), Outcome::Interrupted);
        assert_eq!(classify(1, true, false), Outcome::Interrupted);
        assert_eq!(classify(127, true, true), Outcome::Interrupted);
    }

    #[test]
    fn classify_not_interrupted_uses_exit_code_and_verification() {
        // Exit code 0 with verification passes → Success
        assert_eq!(classify(0, false, true), Outcome::Success);
        // Exit code 0 with verification fails → Failure
        assert_eq!(classify(0, false, false), Outcome::Failure);
        // Non-zero exit codes always fail regardless of verification
        assert_eq!(classify(1, false, true), Outcome::Failure);
        assert_eq!(classify(1, false, false), Outcome::Failure);
        assert_eq!(classify(124, false, true), Outcome::Timeout);
        assert_eq!(classify(127, false, true), Outcome::AgentNotFound);
        assert_eq!(classify(129, false, true), Outcome::Crash(129));
    }

    #[test]
    fn classify_no_wildcard_arms() {
        // Verify key exit codes map correctly per spec.
        // Exit code 0 ONLY succeeds when verification passes
        assert_eq!(classify(0, false, true), Outcome::Success);
        assert_eq!(classify(0, false, false), Outcome::Failure);
        assert_eq!(classify(1, false, true), Outcome::Failure);
        assert_eq!(classify(2, false, true), Outcome::Failure);
        assert_eq!(classify(99, false, true), Outcome::Failure);
        assert_eq!(classify(100, false, true), Outcome::Failure);
        assert_eq!(classify(124, false, true), Outcome::Timeout);
        assert_eq!(classify(125, false, true), Outcome::Failure);
        assert_eq!(classify(128, false, true), Outcome::Failure); // not >128 per spec
        assert_eq!(classify(129, false, true), Outcome::Crash(129));
        assert_eq!(classify(137, false, true), Outcome::Crash(137));
        assert_eq!(classify(-9, false, true), Outcome::Crash(-9));
    }

    #[test]
    fn classify_verification_gate() {
        // The core fix: exit code 0 does NOT guarantee Success.
        // Verification is the gate.
        assert_eq!(
            classify(0, false, false),
            Outcome::Failure,
            "exit_code=0, verified=false must return Failure"
        );
        assert_eq!(
            classify(0, false, true),
            Outcome::Success,
            "exit_code=0, verified=true must return Success"
        );
    }

    // ── classify_with_stream tests ──

    /// The 2026-09-02 zai-proxy outage shape: exit 0, subtype "success",
    /// is_error true, terminal_reason "api_error", num_turns 1.
    fn api_error_stream() -> String {
        concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,",
            "\"api_error_status\":503,\"terminal_reason\":\"api_error\",\"num_turns\":1,",
            "\"result\":\"API Error: 503 no available server\"}\n"
        )
        .to_string()
    }

    #[test]
    fn classify_with_stream_api_error_envelope_is_failure_despite_exit_zero() {
        // The claude CLI exits 0 when the session ends on an API error — the
        // envelope, not the exit code, decides.
        assert_eq!(
            classify_with_stream(0, false, true, &api_error_stream()),
            Outcome::Failure
        );
    }

    #[test]
    fn classify_with_stream_error_terminal_reason_is_failure_despite_exit_zero() {
        let stdout = r#"{"type":"result","subtype":"success","terminal_reason":"api_error"}"#;
        assert_eq!(
            classify_with_stream(0, false, true, stdout),
            Outcome::Failure
        );
    }

    #[test]
    fn classify_with_stream_clean_envelope_exit_zero_is_success() {
        let stdout = r#"{"type":"result","subtype":"success","is_error":false}"#;
        assert_eq!(
            classify_with_stream(0, false, true, stdout),
            Outcome::Success
        );
    }

    #[test]
    fn classify_with_stream_without_envelope_falls_back_to_exit_code() {
        // Formats without a result envelope keep the exit-code classifier.
        assert_eq!(classify_with_stream(0, false, true, ""), Outcome::Success);
        assert_eq!(classify_with_stream(1, false, true, ""), Outcome::Failure);
        assert_eq!(classify_with_stream(124, false, true, ""), Outcome::Timeout);
    }

    #[test]
    fn classify_with_stream_interruption_still_takes_precedence() {
        assert_eq!(
            classify_with_stream(0, true, true, &api_error_stream()),
            Outcome::Interrupted
        );
    }

    #[test]
    fn classify_with_stream_unverified_still_fails() {
        assert_eq!(classify_with_stream(0, false, false, ""), Outcome::Failure);
    }

    // ── handle tests ──

    #[tokio::test]
    async fn handle_success_bead_closed_by_agent() {
        let handler = test_handler_without_shipped_work();
        let store = test_store(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        assert!(!result.telemetry_events.is_empty());
        let actions = store.actions();
        assert!(
            !actions.iter().any(|a| matches!(a, StoreAction::Release(_))),
            "success should not release bead"
        );
    }

    #[tokio::test]
    async fn handle_success_bead_still_open_is_failure_not_orphaned() {
        // needle-97397df2 inverts the old leak assertion: an agent process
        // exiting successfully is not a successful dispatch when verification fails.
        // The old test expected Success plus BeadOrphaned and thereby locked in
        // the leaked in_progress claim.
        let handler = test_handler_with_verification(vec!["false".to_string()]);
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failure);
        assert_eq!(
            result.bead_action,
            BeadAction::Released,
            "exit 0 without verified closure must release the claim"
        );
        let actions = store.actions();
        assert!(
            actions.iter().any(|a| matches!(a, StoreAction::Show(_))),
            "verification failure should check whether the bead needs reopening"
        );
        assert!(
            !result
                .telemetry_events
                .iter()
                .any(|e| matches!(e, EventKind::BeadOrphaned { .. })),
            "an unverified exit must never enter the success/orphan path"
        );
        // The handler no longer calls store.release() -- apply_bead_action() does.
        // "must not remain in_progress" is asserted above via result.bead_action.
    }

    #[tokio::test]
    async fn handle_failure_releases_and_increments_count() {
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(1), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failure);
        assert_eq!(result.bead_action, BeadAction::Released);
        assert!(!result.telemetry_events.is_empty());

        let actions = store.actions();
        // NOTE: the handler no longer calls store.release() -- release is applied by
        // the worker via apply_bead_action(). The release intent is asserted above as
        // result.bead_action; a StoreAction::Release here would now never appear.
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "failure-count:1")
            ),
            "failure must add failure-count:1"
        );
    }

    #[tokio::test]
    async fn handle_failure_increments_existing_count() {
        let handler = test_handler();
        let store = Arc::new(
            MockBeadStore::new(BeadStatus::InProgress)
                .with_labels(vec!["failure-count:2".to_string()]),
        );
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(store.as_ref(), &bead, &test_output(1), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Released);
        let actions = store.actions();
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::RemoveLabel(_, label) if label == "failure-count:2")
            ),
            "should remove old failure-count label"
        );
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "failure-count:3")
            ),
            "should add failure-count:3"
        );
    }

    // ── ADR-012: failure-quarantine circuit breaker ──

    #[tokio::test]
    async fn handle_failure_quarantines_bead_at_threshold() {
        // Default quarantine_after_failures is 5. A bead already at
        // failure-count:4 crosses the threshold on this attempt.
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress)
            .with_labels(vec!["failure-count:4".to_string()]);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(1), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Quarantined);
        let actions = store.actions();
        // bead-rs 0.2.x has no stored 'blocked' status, so quarantine defers
        // the bead with a 'deferred' label instead of calling block().
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(id, label) if id == "needle-test" && label == "deferred")),
            "5th consecutive failure must defer the bead"
        );
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label.starts_with("quarantine:"))
            ),
            "quarantine must record its reason as a label, got: {actions:?}"
        );
        assert!(
            result.telemetry_events.iter().any(|e| matches!(
                e,
                EventKind::BeadQuarantined {
                    failure_count: 5,
                    ..
                }
            )),
            "must emit BeadQuarantined with the crossing count and configured threshold"
        );
    }

    #[tokio::test]
    async fn handle_failure_below_threshold_does_not_quarantine() {
        // Same setup as the threshold test, one failure count lower — this is
        // the regression case for the mitosis NotSplittable fallthrough
        // (ADR-006 Context point 2): a bead below the ceiling still just
        // releases normally, it does not get blocked prematurely.
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress)
            .with_labels(vec!["failure-count:3".to_string()]);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(1), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Released);
        let actions = store.actions();
        assert!(
            !actions.iter().any(|a| matches!(a, StoreAction::Block(_))),
            "4th consecutive failure must not yet quarantine"
        );
    }

    #[tokio::test]
    async fn handle_failure_quarantine_disabled_when_threshold_zero() {
        let mut config = Config::default();
        config.outcome.quarantine_after_failures = 0;
        let handler = test_handler_with_config(config);
        let store = MockBeadStore::new(BeadStatus::InProgress)
            .with_labels(vec!["failure-count:99".to_string()]);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(1), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Released);
        let actions = store.actions();
        assert!(
            !actions.iter().any(|a| matches!(a, StoreAction::Block(_))),
            "threshold=0 must disable quarantine entirely, regardless of failure count"
        );
    }

    #[tokio::test]
    async fn handle_success_resets_failure_count() {
        // Success should reset failure count by removing all failure-count:N labels.
        let handler = test_handler();
        let store =
            MockBeadStore::new(BeadStatus::Done).with_labels(vec!["failure-count:3".to_string()]);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        let actions = store.actions();
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::RemoveLabel(_, label) if label == "failure-count:3")
            ),
            "should remove failure-count:3 on success"
        );
    }

    // ── Regression tests for needle-b39fe1b6: failure count reset timing ──

    #[tokio::test]
    async fn handle_success_without_shipped_work_quarantines_after_three_attempts() {
        // Regression test for GitHub issue #16: a bead that closes without
        // shipped work (e.g., a GitHub comment) should increment the failure
        // count each time and quarantine after the third attempt, not loop
        // forever because the count was reset before shipped-work verification.
        let mut config = Config::default();
        config.outcome.quarantine_after_failures = 3;
        config.worker.enforce_shipped_work = true;
        let handler = test_handler_with_config(config);

        // First attempt: bead has failure-count:2, closes with no shipped work
        let store =
            MockBeadStore::new(BeadStatus::Done).with_labels(vec!["failure-count:2".to_string()]);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        // Should quarantine because shipped-work check fails
        assert_eq!(result.bead_action, BeadAction::Quarantined);
        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(_, label) if label == "deferred")),
            "quarantine must add the deferred label"
        );
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label.contains("quarantine:"))
            ),
            "quarantine must add a reason label"
        );
        assert!(
            result.telemetry_events.iter().any(|e| matches!(
                e,
                EventKind::FalseCloseDetected {
                    failure_count: 3,
                    threshold: 3,
                    ..
                }
            )),
            "must emit FalseCloseDetected with failure_count=3"
        );
    }

    #[tokio::test]
    async fn handle_success_with_shipped_work_resets_failure_count() {
        // A bead that closes WITH shipped work should reset the failure count.
        // This is the positive case: genuine success clears the slate.
        let mut config = Config::default();
        config.worker.enforce_shipped_work = true;
        let handler = test_handler_with_config(config);

        // Bead has failure-count:2 but ships real work (simulated by Done status)
        let store =
            MockBeadStore::new(BeadStatus::Done).with_labels(vec!["failure-count:2".to_string()]);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        let actions = store.actions();
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::RemoveLabel(_, label) if label == "failure-count:2")
            ),
            "shipped work should remove failure-count:2"
        );
    }

    #[tokio::test]
    async fn handle_orphan_without_shipped_work_increments_failure_count() {
        // Orphan path: agent exits 0, bead still open, no shipped work.
        // Should increment failure count, not reset it.
        let mut config = Config::default();
        config.worker.enforce_shipped_work = true;
        let handler = test_handler_with_config(config);

        // Bead is still open (InProgress) with failure-count:1
        let store = MockBeadStore::new(BeadStatus::InProgress)
            .with_labels(vec!["failure-count:1".to_string()]);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Released);
        let actions = store.actions();
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "failure-count:2")
            ),
            "orphan without shipped work should increment to failure-count:2"
        );
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::RemoveLabel(_, label) if label == "failure-count:1")
            ),
            "should remove old failure-count:1"
        );
    }

    /// A shipped-work gate that CANNOT RUN must not be judged as a failure.
    ///
    /// GitHub issue #18: with no upstream configured, the shipped-work gate
    /// cannot verify a push at all. Its `ExecutionError` must release the bead
    /// WITHOUT incrementing the failure count (the needle-4aaa010c precedent)
    /// — otherwise every closure in a workspace with no remote burns the
    /// retry counter toward quarantine and feeds mitosis with work the gate
    /// never judged. Regression: this arm routed to `handle_gate_failure`
    /// while its own log line claimed otherwise.
    #[tokio::test]
    async fn shipped_work_execution_error_releases_without_incrementing_failure_count() {
        // The gate reads its predispatch snapshot and records gate health under
        // $HOME/.needle/state — pin HOME to a private dir for the whole body.
        let _env = crate::util::test_env::isolate_env();
        let home = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());

        // A real repo with a substantial new commit and NO upstream: exactly
        // the shape the gate cannot judge.
        let repo = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "test"]);
        std::fs::write(repo.path().join("README.md"), "init\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        let pre_sha = String::from_utf8(run(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        std::fs::write(repo.path().join("src.rs"), "fn main() {}\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "real work"]);
        // No remote, no `push -u` — `@{u}` does not resolve.

        // The dispatch baseline: HEAD was pre_sha, notes were empty.
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = repo.path().to_path_buf();
        let snapshot = crate::validation::predispatch::PreDispatch {
            head_sha: Some(pre_sha),
            notes_hash: Some(crate::validation::predispatch::hash_notes("")),
            dirty_files: Vec::new(),
            captured_at: None,
        };
        let snap_path = crate::validation::predispatch::snapshot_path(repo.path(), &bead.id);
        std::fs::create_dir_all(snap_path.parent().unwrap()).unwrap();
        std::fs::write(&snap_path, serde_json::to_vec(&snapshot).unwrap()).unwrap();

        let mut config = Config::default();
        config.worker.enforce_shipped_work = true;
        let helper = crate::telemetry::test_utils::TestHelper::new("shipped-gate-error-test");
        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        let store =
            MockBeadStore::new(BeadStatus::Done).with_labels(vec!["failure-count:2".to_string()]);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        // Released (the gate-error action), not reopened or quarantined.
        assert_eq!(result.bead_action, BeadAction::Released);

        // The unsatisfiable check must not burn the failure counter, label the
        // bead as a verification failure, or cycle it.
        let actions = store.actions();
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(_, label)
                if label.starts_with("failure-count")
                    || label == "cycling"
                    || label == "deferred"
                    || label == "verification-failed")),
            "a gate that could not run must not increment the failure count \
             or label the bead: {actions:?}"
        );

        // And it must have gone out through the gate-error path, not silently.
        helper.sync().await;
        assert!(
            !helper.events_by_type("gate.execution_error").is_empty(),
            "shipped-work gate that cannot run must emit gate.execution_error, got: {:?}",
            helper
                .all_events()
                .iter()
                .map(|e| e.event_type.clone())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn handle_timeout_releases_and_adds_deferred() {
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(124), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Timeout);
        assert_eq!(result.bead_action, BeadAction::Deferred);

        let actions = store.actions();
        // NOTE: the handler no longer calls store.release() -- release is applied by
        // the worker via apply_bead_action(). The release intent is asserted above as
        // result.bead_action; a StoreAction::Release here would now never appear.
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(_, label) if label == "deferred")),
            "timeout must add deferred label"
        );
    }

    #[tokio::test]
    async fn handle_crash_releases_and_creates_alert_bead() {
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(137), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Crash(137));
        assert_eq!(result.bead_action, BeadAction::Alerted);

        let actions = store.actions();
        // NOTE: the handler no longer calls store.release() -- release is applied by
        // the worker via apply_bead_action(). The release intent is asserted above as
        // result.bead_action; a StoreAction::Release here would now never appear.
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::CreateBead(title, _) if title.contains("needle-test"))
            ),
            "crash must create alert bead referencing the original bead"
        );
    }

    #[tokio::test]
    async fn handle_crash_negative_exit_code() {
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(-1), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Crash(-1));
        assert_eq!(result.bead_action, BeadAction::Alerted);
    }

    #[tokio::test]
    async fn handle_agent_not_found_releases() {
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(127), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::AgentNotFound);
        assert_eq!(result.bead_action, BeadAction::Released);

        // NOTE: the handler no longer calls store.release() -- release is applied by
        // the worker via apply_bead_action(). The release intent is asserted above as
        // result.bead_action; a StoreAction::Release here would now never appear.
    }

    #[tokio::test]
    async fn handle_interrupted_releases() {
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), true)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Interrupted);
        assert_eq!(result.bead_action, BeadAction::Interrupted);

        // NOTE: the handler no longer calls store.release() -- release is applied by
        // the worker via apply_bead_action(). The release intent is asserted above as
        // result.bead_action; a StoreAction::Release here would now never appear.
    }

    #[tokio::test]
    async fn handle_failure_emits_telemetry_events() {
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(2), false)
            .await
            .unwrap();

        assert!(
            result
                .telemetry_events
                .iter()
                .any(|e| matches!(e, EventKind::BeadReleased { .. })),
            "failure should emit BeadReleased event"
        );
    }

    #[test]
    fn outcome_display_covers_all_variants() {
        assert_eq!(format!("{}", Outcome::Success), "Success");
        assert_eq!(format!("{}", Outcome::Failure), "Failure");
        assert_eq!(format!("{}", Outcome::Timeout), "Timeout");
        assert_eq!(format!("{}", Outcome::AgentNotFound), "AgentNotFound");
        assert_eq!(format!("{}", Outcome::Interrupted), "Interrupted");
        assert_eq!(format!("{}", Outcome::Crash(-9)), "Crash(-9)");
    }

    // ── verification gate tests ──

    fn test_handler_with_verification(commands: Vec<String>) -> OutcomeHandler {
        let config = Config {
            verification: commands,
            ..Config::default()
        };
        let telemetry = Telemetry::with_sink("test-worker".to_string(), NopSink);
        OutcomeHandler::new(config, telemetry)
    }

    #[tokio::test]
    async fn handle_success_no_verification_default_behavior() {
        // No verification configured → normal success flow (unchanged behavior).
        // Disable shipped-work enforcement since this test doesn't mock predispatch snapshots.
        let handler = test_handler_without_shipped_work();
        let store = test_store(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
    }

    #[tokio::test]
    async fn failed_completion_flush_does_not_report_success_or_release_finished_work() {
        let handler = test_handler_without_shipped_work();
        let mut store = test_store(BeadStatus::Done);
        store.fail_flush = true;
        let result = handler
            .handle(
                &store,
                &test_bead(BeadStatus::InProgress),
                &test_output(0),
                false,
            )
            .await;
        assert!(result.is_err());
        assert!(store
            .actions()
            .iter()
            .all(|action| !matches!(action, StoreAction::Release(_))));
    }

    #[tokio::test]
    async fn handle_success_verification_passes_accepts_closure() {
        // Verification passes → bead closure accepted.
        // Disable shipped-work enforcement since this test doesn't mock predispatch snapshots.
        let handler = test_handler_without_shipped_work();
        let store = test_store(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        assert!(result
            .telemetry_events
            .iter()
            .any(|e| matches!(e, EventKind::BeadCompleted { .. })));
    }

    #[tokio::test]
    async fn handle_success_verification_fails_releases_bead() {
        // Verification fails → bead released.
        let handler = test_handler_with_verification(vec!["false".to_string()]);
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failure);
        assert_eq!(result.bead_action, BeadAction::Released);

        let actions = store.actions();
        // NOTE: the handler no longer calls store.release() -- release is applied by
        // the worker via apply_bead_action(). The release intent is asserted above as
        // result.bead_action; a StoreAction::Release here would now never appear.
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "verification-failed")
            ),
            "verification failure must add verification-failed label"
        );
    }

    #[tokio::test]
    async fn handle_success_verification_fails_reopens_closed_bead() {
        // Agent closed the bead, but verification fails → reopen then release.
        let handler = test_handler_with_verification(vec!["false".to_string()]);
        let store = test_store(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Released);

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Reopen(id) if id == "needle-test")),
            "verification failure on closed bead must reopen it first"
        );
        // NOTE: the handler no longer calls store.release() -- release is applied by
        // the worker via apply_bead_action(). The release intent is asserted above as
        // result.bead_action; a StoreAction::Release here would now never appear.
    }

    #[tokio::test]
    async fn handle_success_verification_fails_increments_failure_count() {
        let handler = test_handler_with_verification(vec!["false".to_string()]);
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let _result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        let actions = store.actions();
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "failure-count:1")
            ),
            "verification failure must increment failure count"
        );
    }

    #[tokio::test]
    async fn handle_success_multiple_gates_first_fails() {
        // First gate passes, second fails → should stop and release.
        let handler = test_handler_with_verification(vec![
            "true".to_string(),
            "false".to_string(),
            "echo should-not-run".to_string(),
        ]);
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Released);
    }

    // ── timeout and resilience tests ──

    #[tokio::test]
    async fn handle_failure_with_flush_timeout_preserves_bead_state() {
        // Test that flush timeout doesn't block the worker in HANDLING state.
        struct SlowFlushStore {
            inner: MockBeadStore,
        }

        #[async_trait]
        impl BeadStore for SlowFlushStore {
            fn has_valid_store(&self) -> bool {
                true // Mock store always has a valid store
            }

            async fn list_all(&self) -> Result<Vec<Bead>> {
                self.inner.list_all().await
            }
            async fn ready(&self, filters: &crate::bead_store::Filters) -> Result<Vec<Bead>> {
                self.inner.ready(filters).await
            }
            async fn show(&self, id: &BeadId) -> Result<Bead> {
                self.inner.show(id).await
            }
            async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
                self.inner.claim(id, actor).await
            }

            async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
                self.inner.claim_auto(actor).await
            }

            async fn release(&self, id: &BeadId) -> Result<()> {
                self.inner.release(id).await
            }
            async fn block(&self, id: &BeadId) -> Result<()> {
                self.inner.block(id).await
            }
            async fn flush(&self) -> Result<()> {
                // Simulate a slow flush that times out.
                tokio::time::sleep(std::time::Duration::from_secs(35)).await;
                Ok(())
            }
            async fn reopen(&self, id: &BeadId) -> Result<()> {
                self.inner.reopen(id).await
            }
            async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
                self.inner.labels(id).await
            }
            async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
                self.inner.add_label(id, label).await
            }
            async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
                self.inner.remove_label(id, label).await
            }
            async fn create_bead(
                &self,
                title: &str,
                body: &str,
                labels: &[&str],
            ) -> Result<BeadId> {
                self.inner.create_bead(title, body, labels).await
            }
            async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
                self.inner.doctor_repair().await
            }
            async fn doctor_check(&self) -> Result<crate::bead_store::RepairReport> {
                self.inner.doctor_check().await
            }
            async fn full_rebuild(&self) -> Result<()> {
                self.inner.full_rebuild().await
            }
            async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
                self.inner.add_dependency(blocker_id, blocked_id).await
            }
            async fn remove_dependency(
                &self,
                blocked_id: &BeadId,
                blocker_id: &BeadId,
            ) -> Result<()> {
                self.inner.remove_dependency(blocked_id, blocker_id).await
            }

            async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
                self.inner.clear_assignee(id).await
            }
        }

        let handler = test_handler();
        let store = Arc::new(SlowFlushStore {
            inner: MockBeadStore::new(BeadStatus::InProgress),
        });
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(store.as_ref(), &bead, &test_output(1), false)
            .await;

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("preserving bead state"));
        assert!(store
            .inner
            .actions()
            .iter()
            .all(|action| !matches!(action, StoreAction::Release(_))));
    }

    #[tokio::test]
    async fn handle_failure_defers_release_until_worker_applies_action() {
        // The handler prepares an action; the worker performs the release.
        struct SlowReleaseStore {
            inner: MockBeadStore,
        }

        #[async_trait]
        impl BeadStore for SlowReleaseStore {
            fn has_valid_store(&self) -> bool {
                true // Mock store always has a valid store
            }

            async fn list_all(&self) -> Result<Vec<Bead>> {
                self.inner.list_all().await
            }
            async fn ready(&self, filters: &crate::bead_store::Filters) -> Result<Vec<Bead>> {
                self.inner.ready(filters).await
            }
            async fn show(&self, id: &BeadId) -> Result<Bead> {
                self.inner.show(id).await
            }
            async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
                self.inner.claim(id, actor).await
            }

            async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
                self.inner.claim_auto(actor).await
            }

            async fn release(&self, _id: &BeadId) -> Result<()> {
                // Not exercised any more: the handler never calls release() -- the
                // worker's apply_bead_action() does. Kept fast so this store cannot
                // silently reintroduce a 35s stall if a caller is added back.
                Ok(())
            }
            async fn block(&self, id: &BeadId) -> Result<()> {
                self.inner.block(id).await
            }
            async fn flush(&self) -> Result<()> {
                self.inner.flush().await
            }
            async fn reopen(&self, id: &BeadId) -> Result<()> {
                self.inner.reopen(id).await
            }
            async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
                self.inner.labels(id).await
            }
            async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
                self.inner.add_label(id, label).await
            }
            async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
                self.inner.remove_label(id, label).await
            }
            async fn create_bead(
                &self,
                title: &str,
                body: &str,
                labels: &[&str],
            ) -> Result<BeadId> {
                self.inner.create_bead(title, body, labels).await
            }
            async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
                self.inner.doctor_repair().await
            }
            async fn doctor_check(&self) -> Result<crate::bead_store::RepairReport> {
                self.inner.doctor_check().await
            }
            async fn full_rebuild(&self) -> Result<()> {
                self.inner.full_rebuild().await
            }
            async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
                self.inner.add_dependency(blocker_id, blocked_id).await
            }
            async fn remove_dependency(
                &self,
                blocked_id: &BeadId,
                blocker_id: &BeadId,
            ) -> Result<()> {
                self.inner.remove_dependency(blocked_id, blocker_id).await
            }

            async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
                self.inner.clear_assignee(id).await
            }
        }

        let handler = test_handler();
        let store = Arc::new(SlowReleaseStore {
            inner: MockBeadStore::new(BeadStatus::InProgress),
        });
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(store.as_ref(), &bead, &test_output(1), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failure);
        assert_eq!(result.bead_action, BeadAction::Released);
        assert!(store
            .inner
            .actions()
            .iter()
            .all(|action| !matches!(action, StoreAction::Release(_))));
    }

    #[tokio::test]
    async fn handle_with_cancellation_respects_cancelled_flag() {
        // Test that handle_with_cancellation returns early when cancelled.
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let cancelled = Arc::new(AtomicBool::new(true));
        let result = handler
            .handle_with_cancellation(&store, &bead, &test_output(1), false, cancelled)
            .await
            .unwrap();

        // Should return a default result without calling the store.
        assert_eq!(result.outcome, Outcome::Failure);
        assert_eq!(result.bead_action, BeadAction::Errored);
        assert!(result.telemetry_events.is_empty());
    }

    // ── configurable outcome timeout tests (GitHub issue jedarden/NEEDLE#8) ──

    fn test_handler_with_config(config: Config) -> OutcomeHandler {
        let telemetry = Telemetry::with_sink("test-worker".to_string(), NopSink);
        OutcomeHandler::new(config, telemetry)
    }

    #[test]
    fn validation_outcome_timeout_seconds_defaults_to_50() {
        // Preserves the previous hardcoded behavior as the default.
        assert_eq!(Config::default().validation.outcome_timeout_seconds, 50);
    }

    #[test]
    fn validation_outcome_timeout_seconds_parses_override() {
        let yaml = "validation:\n  outcome_timeout_seconds: 300\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.validation.outcome_timeout_seconds, 300);
        // stderr_cap_bytes still defaults even though only outcome_timeout_seconds was set.
        assert_eq!(config.validation.stderr_cap_bytes, 4096);
    }

    #[tokio::test]
    async fn handle_with_cancellation_respects_configured_timeout() {
        // A bead store whose `show()` genuinely sleeps (a real .await yield
        // point, unlike a blocking `std::process::Command` gate — see below)
        // for 2s, with outcome_timeout_seconds configured to 1s: far shorter
        // than the previous hardcoded 50s and shorter than the store's own
        // inner 30s timeout_op. If the outer timeout fires here, the
        // *configured* value is what's enforced, not the old constant.
        //
        // Note: this deliberately does NOT use a slow `verification:` gate
        // command to trigger the timeout. `CommandGate::run_command` calls
        // the fully synchronous, blocking `std::process::Command::output()`
        // with no `.await` point — tokio's `Timeout::poll` polls the wrapped
        // future first and only checks the deadline if it's still `Pending`,
        // so a wrapped future with no yield point during a slow segment
        // always "wins" the race once it finally completes, regardless of
        // the configured timeout. That's a separate, pre-existing limitation
        // of the blocking gate-execution path (unchanged by this fix, and
        // out of scope for GitHub issue jedarden/NEEDLE#8) — not something
        // to paper over by picking a mechanism that can't actually prove the
        // config value is enforced.
        struct SlowShowStore {
            inner: MockBeadStore,
        }

        #[async_trait]
        impl BeadStore for SlowShowStore {
            fn has_valid_store(&self) -> bool {
                true
            }
            async fn list_all(&self) -> Result<Vec<Bead>> {
                self.inner.list_all().await
            }
            async fn ready(&self, filters: &crate::bead_store::Filters) -> Result<Vec<Bead>> {
                self.inner.ready(filters).await
            }
            async fn show(&self, id: &BeadId) -> Result<Bead> {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                self.inner.show(id).await
            }
            async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
                self.inner.claim(id, actor).await
            }
            async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
                self.inner.claim_auto(actor).await
            }
            async fn release(&self, id: &BeadId) -> Result<()> {
                self.inner.release(id).await
            }
            async fn block(&self, id: &BeadId) -> Result<()> {
                self.inner.block(id).await
            }
            async fn flush(&self) -> Result<()> {
                self.inner.flush().await
            }
            async fn reopen(&self, id: &BeadId) -> Result<()> {
                self.inner.reopen(id).await
            }
            async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
                self.inner.labels(id).await
            }
            async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
                self.inner.add_label(id, label).await
            }
            async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
                self.inner.remove_label(id, label).await
            }
            async fn create_bead(
                &self,
                title: &str,
                body: &str,
                labels: &[&str],
            ) -> Result<BeadId> {
                self.inner.create_bead(title, body, labels).await
            }
            async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
                self.inner.doctor_repair().await
            }
            async fn doctor_check(&self) -> Result<crate::bead_store::RepairReport> {
                self.inner.doctor_check().await
            }
            async fn full_rebuild(&self) -> Result<()> {
                self.inner.full_rebuild().await
            }
            async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
                self.inner.add_dependency(blocker_id, blocked_id).await
            }
            async fn remove_dependency(
                &self,
                blocked_id: &BeadId,
                blocker_id: &BeadId,
            ) -> Result<()> {
                self.inner.remove_dependency(blocked_id, blocker_id).await
            }
            async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
                self.inner.clear_assignee(id).await
            }
        }

        // No gates configured — `handle_success` goes straight to `store.show()`.
        let config = Config {
            validation: ValidationConfig {
                outcome_timeout_seconds: 1,
                ..Default::default()
            },
            ..Config::default()
        };
        let handler = test_handler_with_config(config);
        let store = Arc::new(SlowShowStore {
            inner: MockBeadStore::new(BeadStatus::Done),
        });
        let bead = test_bead(BeadStatus::InProgress);
        let cancelled = Arc::new(AtomicBool::new(false));

        let start = std::time::Instant::now();
        let result = handler
            .handle_with_cancellation(store.as_ref(), &bead, &test_output(0), false, cancelled)
            .await
            .unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_secs() < 10,
            "expected the ~1s configured timeout to fire well before the store's \
             2s show() or its own 30s inner timeout, took {:?}",
            elapsed
        );
        assert_eq!(result.bead_action, BeadAction::Errored);
        assert!(result.telemetry_events.is_empty());
    }

    #[tokio::test]
    async fn handle_with_cancellation_kills_a_slow_verification_gate_command() {
        // End-to-end version of the test above, using a real `verification:`
        // gate command instead of a slow store call — the exact scenario
        // GitHub issue jedarden/NEEDLE#8 is actually about, and the one that
        // originally exposed bf-3saat (CommandGate used a blocking
        // std::process::Command with no .await yield point, so this same
        // setup used to run the full 3s command to completion instead of
        // being cut off at the configured 1s timeout). Now that CommandGate
        // uses tokio::process::Command with kill_on_drop(true)
        // (src/validation/mod.rs), this must actually preempt and kill the
        // gate command around the configured timeout, not just eventually
        // report it as having taken too long.
        let marker = tempfile::NamedTempFile::new().unwrap();
        let marker_path = marker.path().to_path_buf();
        std::fs::remove_file(&marker_path).ok();

        let config = Config {
            verification: vec![format!("sleep 3 && touch {}", marker_path.display())],
            validation: ValidationConfig {
                outcome_timeout_seconds: 1,
                ..Default::default()
            },
            ..Config::default()
        };
        let handler = test_handler_with_config(config);
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);
        let cancelled = Arc::new(AtomicBool::new(false));

        let start = std::time::Instant::now();
        let result = handler
            .handle_with_cancellation(&store, &bead, &test_output(0), false, cancelled)
            .await
            .unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_secs() < 3,
            "expected the ~1s configured timeout to cut off the 3s gate command, took {:?}",
            elapsed
        );
        assert_eq!(result.bead_action, BeadAction::Errored);
        assert!(result.telemetry_events.is_empty());

        // Give any straggling kill signal a moment to land, then confirm the
        // gate command was actually killed, not left running in the
        // background to finish on its own.
        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
        assert!(
            !marker_path.exists(),
            "gate command was not actually killed — it ran to completion in the background"
        );
    }

    // ── Regression test for needle-6d76f548: vanished workspace directory ──

    #[tokio::test]
    async fn handle_success_releases_bead_when_workspace_vanishes() {
        // Regression test for needle-6d76f548: when the workspace directory is
        // deleted while the worker is handling outcome (e.g., by a concurrent
        // operation or supervisor restart), the bead MUST still be released to
        // enforce the postcondition, even though store operations fail.
        //
        // This reproduces the bash error seen in the wild:
        // "getcwd: cannot access parent directories: No such file or directory"
        struct VanishingWorkspaceStore {
            inner: MockBeadStore,
            show_fail_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
        }

        #[async_trait::async_trait]
        impl BeadStore for VanishingWorkspaceStore {
            fn has_valid_store(&self) -> bool {
                true
            }

            async fn list_all(&self) -> Result<Vec<Bead>> {
                self.inner.list_all().await
            }
            async fn ready(&self, filters: &crate::bead_store::Filters) -> Result<Vec<Bead>> {
                self.inner.ready(filters).await
            }

            async fn show(&self, _id: &BeadId) -> Result<Bead> {
                // Simulate the workspace directory vanishing during show()
                // by failing after a few calls
                let count = self
                    .show_fail_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // handle_success calls show() exactly once, so this must fail on the
                // first call. A "succeed twice, then vanish" mock silently never fired
                // once the release path moved out of the handler, and the regression
                // this test exists for went unexercised.
                let _ = count;
                anyhow::bail!("workspace directory vanished: getcwd failed")
            }

            async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
                self.inner.claim(id, actor).await
            }

            async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
                self.inner.claim_auto(actor).await
            }

            async fn release(&self, id: &BeadId) -> Result<()> {
                self.inner.release(id).await
            }
            async fn block(&self, id: &BeadId) -> Result<()> {
                self.inner.block(id).await
            }
            async fn flush(&self) -> Result<()> {
                // Flush succeeds even after workspace vanishes
                self.inner.flush().await
            }
            async fn reopen(&self, id: &BeadId) -> Result<()> {
                self.inner.reopen(id).await
            }
            async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
                self.inner.labels(id).await
            }
            async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
                self.inner.add_label(id, label).await
            }
            async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
                self.inner.remove_label(id, label).await
            }
            async fn create_bead(
                &self,
                title: &str,
                body: &str,
                labels: &[&str],
            ) -> Result<BeadId> {
                self.inner.create_bead(title, body, labels).await
            }
            async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
                self.inner.doctor_repair().await
            }
            async fn doctor_check(&self) -> Result<crate::bead_store::RepairReport> {
                self.inner.doctor_check().await
            }
            async fn full_rebuild(&self) -> Result<()> {
                self.inner.full_rebuild().await
            }
            async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
                self.inner.add_dependency(blocker_id, blocked_id).await
            }
            async fn remove_dependency(
                &self,
                blocked_id: &BeadId,
                blocker_id: &BeadId,
            ) -> Result<()> {
                self.inner.remove_dependency(blocked_id, blocker_id).await
            }

            async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
                self.inner.clear_assignee(id).await
            }
        }

        let handler = test_handler();
        let store = VanishingWorkspaceStore {
            inner: MockBeadStore::new(BeadStatus::InProgress),
            show_fail_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        let bead = test_bead(BeadStatus::InProgress);

        // The handler should still release the bead even when show() fails
        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        // Critical: the bead MUST be released even though workspace operations failed
        assert_eq!(result.bead_action, BeadAction::Released);
        assert!(!result.telemetry_events.is_empty());

        // Verify that the error was logged and handled
        assert!(
            result
                .telemetry_events
                .iter()
                .any(|e| matches!(e, EventKind::WorkerHandlingTimeout { .. })),
            "workspace failure should emit WorkerHandlingTimeout event"
        );
    }

    #[tokio::test]
    async fn handle_gate_execution_error_releases_without_incrementing_failure_count() {
        // Regression test for needle-4aaa010c: gate execution errors should release
        // the bead WITHOUT incrementing failure count or adding the cycling label.
        //
        // A capturing sink, because the execution-error event is emitted through
        // telemetry rather than returned in HandlerResult.
        let helper = crate::telemetry::test_utils::TestHelper::new("gate-error-test");
        let handler = OutcomeHandler::new(Config::default(), helper.telemetry().clone());
        let store = MockBeadStore::new(BeadStatus::InProgress)
            .with_labels(vec!["failure-count:2".to_string()]);
        let bead = test_bead(BeadStatus::InProgress);

        // Drive the execution-error branch `handle` dispatches to when a gate
        // could not be run at all. Building a GateReport and calling `handle`
        // does not reach it: `handle` only consults gate results it ran itself
        // (exit code 0), so a report built here is never looked at, and the
        // ordinary failure path — which does increment — runs instead.
        let (bead_action, telemetry_events) = handler
            .handle_gate_error(
                &store,
                &bead,
                &bead.workspace.display().to_string(),
                "test_gate",
                "nonexistent_command",
                "ENOENT",
            )
            .await
            .unwrap();
        let result = HandlerResult {
            bead_action,
            telemetry_events,
            outcome: Outcome::GateError,
        };

        // The bead should be released
        assert_eq!(result.bead_action, BeadAction::Released);

        // Verify failure count was NOT incremented
        let actions = store.actions();
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(_, label)
                    if label == "failure-count:3" || label.contains("failure-count"))),
            "gate execution error should NOT increment failure count"
        );

        // Verify cycling label was NOT added
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(_, label) if label == "cycling")),
            "gate execution error should NOT add cycling label"
        );

        // The memory sink is written asynchronously.
        helper.sync().await;

        // Verify gate.execution_error event was emitted. It goes out through
        // telemetry, not through HandlerResult.telemetry_events — that vec
        // carries only the subset the worker itself acts on.
        assert!(
            !helper.events_by_type("gate.execution_error").is_empty(),
            "gate execution error should emit gate.execution_error event, got: {:?}",
            helper
                .all_events()
                .iter()
                .map(|e| e.event_type.clone())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn handle_gate_failure_still_increments_failure_count() {
        // Existing gate failures (verification failures) should still increment
        // failure count. This is the positive case — we're ensuring the new
        // GateError handling doesn't break existing gate failure behavior.
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress)
            .with_labels(vec!["failure-count:1".to_string()]);
        let bead = test_bead(BeadStatus::InProgress);

        // Create a gate report with a verification failure (gate ran but failed)
        let mut results = std::collections::HashMap::new();
        results.insert(
            "test_gate".to_string(),
            crate::validation::GateResult::Fail("test failed".to_string()),
        );
        let _gate_report = Some(crate::validation::GateReport::new(results));

        // Simulate the outcome handling with gate failure
        let result = handler
            .handle(&store, &bead, &test_output(1), false)
            .await
            .unwrap();

        // The bead should be released
        assert_eq!(result.bead_action, BeadAction::Released);

        // Verify failure count WAS incremented
        let actions = store.actions();
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "failure-count:2")
            ),
            "gate failure should increment failure count"
        );

        // Verify gate.execution_error event was NOT emitted
        assert!(
            !result
                .telemetry_events
                .iter()
                .any(|e| matches!(e, EventKind::GateExecutionError { .. })),
            "gate failure should NOT emit gate.execution_error event"
        );
    }
}
