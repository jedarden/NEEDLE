//! Outcome routing: map agent exit codes to explicit handlers.
//!
//! Every possible exit code has a named handler. The type system enforces
//! exhaustiveness — if an outcome can happen, it must have a handler.
//!
//! Depends on: `types`, `config`, `bead_store`, `telemetry`, `validation`.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;

use crate::bead_store::{BeadStore, SyncRecoveryError};
use crate::config::Config;
use crate::telemetry::{EventKind, Telemetry};
#[cfg(test)]
use crate::types::BeadStatus;
use crate::types::{AgentOutcome, Bead, BeadAction, HandlerResult, Outcome};
use crate::validation::{verify_shipped_work, GateConfig, GateReport, ValidationGate};

// ──────────────────────────────────────────────────────────────────────────────
// classify (convenience re-export)
// ──────────────────────────────────────────────────────────────────────────────

/// Classify an agent exit code into an `Outcome`, with shutdown signal support.
///
/// Delegates to `Outcome::classify()`. Provided here for ergonomic imports.
pub fn classify(exit_code: i32, was_interrupted: bool, verified: bool) -> Outcome {
    Outcome::classify(exit_code, was_interrupted, verified)
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

    /// Release a bead with sync conflict recovery and graceful failure handling.
    ///
    /// Flow:
    /// 1. Run `br sync --flush-only` to push local writes to JSONL first.
    /// 2. Attempt `br update` to release the bead (set status=open, assignee="").
    /// 3. If SYNC_CONFLICT recovery fails, emit `BeadReleaseFailed` and return Ok.
    ///
    /// This method never returns an error — release failures are non-fatal.
    /// The worker can continue processing other beads even if release fails.
    async fn release_bead(&self, store: &dyn BeadStore, bead: &Bead) -> Result<Vec<EventKind>> {
        let mut events = Vec::new();

        // Step 1: Flush local writes to JSONL before attempting release.
        // Emit heartbeat before br call to track where hang occurs.
        let _ = self.telemetry.emit_try_lock(EventKind::HeartbeatEmitted {
            bead_id: Some(bead.id.clone()),
            state: "HANDLING_FLUSH".to_string(),
        });

        match self.timeout_op(|| store.flush(), "flush").await {
            Ok(Some(())) => {
                tracing::debug!(
                    bead_id = %bead.id,
                    "flushed local changes to JSONL before release"
                );
                // Emit heartbeat after successful flush.
                let _ = self.telemetry.emit_try_lock(EventKind::HeartbeatEmitted {
                    bead_id: Some(bead.id.clone()),
                    state: "HANDLING_FLUSH_DONE".to_string(),
                });
            }
            Ok(None) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    "flush timed out before release, attempting release anyway"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "release".to_string(),
                    operation: "flush".to_string(),
                    error: "timeout after 30s".to_string(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "flush failed before release, attempting release anyway"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "release".to_string(),
                    operation: "flush".to_string(),
                    error: e.to_string(),
                });
            }
        }

        // Step 2: Attempt release with timeout protection.
        // Emit heartbeat before br release call to track where hang occurs.
        let _ = self.telemetry.emit_try_lock(EventKind::HeartbeatEmitted {
            bead_id: Some(bead.id.clone()),
            state: "HANDLING_RELEASE".to_string(),
        });

        let release_result =
            tokio::time::timeout(std::time::Duration::from_secs(30), store.release(&bead.id)).await;

        match release_result {
            Ok(Ok(())) => {
                tracing::debug!(
                    bead_id = %bead.id,
                    "bead released successfully"
                );
                // Emit heartbeat after successful release.
                let _ = self.telemetry.emit_try_lock(EventKind::HeartbeatEmitted {
                    bead_id: Some(bead.id.clone()),
                    state: "HANDLING_RELEASE_DONE".to_string(),
                });
                events.push(EventKind::BeadReleased {
                    bead_id: bead.id.clone(),
                    reason: "release_success".to_string(),
                });
            }
            Ok(Err(e)) => {
                // Release failed (br error, not timeout).
                // Check if this is a SyncRecoveryError (sync recovery was attempted but failed).
                if e.downcast_ref::<SyncRecoveryError>().is_some() {
                    tracing::error!(
                        bead_id = %bead.id,
                        error = %e,
                        "br release failed after SYNC_CONFLICT recovery — \
                         worker will continue (bead may remain assigned)"
                    );
                } else {
                    tracing::error!(
                        bead_id = %bead.id,
                        error = %e,
                        "br release failed — worker will continue (bead may remain assigned)"
                    );
                }
                events.push(EventKind::BeadReleaseFailed {
                    bead_id: bead.id.clone(),
                    reason: e.to_string(),
                });
            }
            Err(_) => {
                // Timeout after 30s.
                tracing::error!(
                    bead_id = %bead.id,
                    "br release timed out after 30s — worker will continue (bead may remain assigned)"
                );
                events.push(EventKind::BeadReleaseFailed {
                    bead_id: bead.id.clone(),
                    reason: "timeout after 30s".to_string(),
                });
            }
        }

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

        let outcome = classify(output.exit_code, was_interrupted, verified);

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
        let _ = self.telemetry.emit_try_lock(EventKind::OutcomeClassified {
            bead_id: bead.id.clone(),
            outcome: outcome.as_str().to_string(),
            exit_code: output.exit_code,
        });

        let (bead_action, telemetry_events) = match outcome.clone() {
            Outcome::Success => self.handle_success(store, bead, gate_report).await?,
            Outcome::Failure => {
                // If we have a gate report with failures, handle as gate failure.
                if let Some(report) = gate_report {
                    if !report.all_passed {
                        self.handle_gate_failure(store, bead, &report).await?
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
        };

        // Emit sub-handler events (e.g. BeadCompleted, BeadOrphaned) to the
        // telemetry sink so they appear in the JSONL log.
        // Use emit_try_lock() to avoid blocking if telemetry writer is stuck.
        for event in &telemetry_events {
            let _ = self.telemetry.emit_try_lock(event.clone());
        }

        // Use emit_try_lock() to avoid blocking if telemetry writer is stuck.
        let _ = self.telemetry.emit_try_lock(EventKind::OutcomeHandled {
            bead_id: bead.id.clone(),
            outcome: outcome.as_str().to_string(),
            action: bead_action.to_string(),
        });

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
            // Return a default HandlerResult to allow the worker to recover.
            // Verification never ran, so conservatively treat as unverified.
            return Ok(HandlerResult {
                outcome: classify(output.exit_code, was_interrupted, false),
                bead_action: BeadAction::None,
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
                let _ = telemetry.emit(EventKind::WorkerHandlingTimeout {
                    bead_id: bead_id.clone(),
                    outcome: classify(output.exit_code, was_interrupted, false)
                        .as_str()
                        .to_string(),
                    operation: "handle".to_string(),
                    error: format!("timeout after {}s", timeout_secs),
                });
                // Return a default HandlerResult to allow the worker to recover.
                // The worker's 60-second timeout wrapper will handle best-effort release.
                // Verification never ran, so conservatively treat as unverified.
                Ok(HandlerResult {
                    outcome: classify(output.exit_code, was_interrupted, false),
                    bead_action: BeadAction::Released,
                    telemetry_events: vec![EventKind::BeadReleased {
                        bead_id,
                        reason: "handler_timeout".to_string(),
                    }],
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
            self.telemetry.emit(EventKind::VerificationPassed {
                bead_id: bead.id.clone(),
                gates_run,
            })?;
            tracing::info!(
                bead_id = %bead.id,
                gates_run,
                "all validation gates passed"
            );
        }

        // Reset failure count on successful gate completion.
        // This ensures the bead starts fresh on the next cycle if re-claimed.
        let _ = self.reset_failure_count(store, bead).await;

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
                        Ok(crate::validation::GateResult::Pass) => {}
                        Err(e) => {
                            tracing::warn!(
                                bead_id = %bead.id,
                                error = %e,
                                "shipped-work check errored — failing open"
                            );
                        }
                    }
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
                            // Mark as completed - the agent shipped work but forgot to close.
                            // We can't close via bead store (no close method), so emit completion
                            // event and release. The bead remains open but work is done.
                            events.push(EventKind::BeadCompleted {
                                bead_id: bead.id.clone(),
                                duration_ms: 0,
                            });
                            let mut release_events = self.release_bead(store, bead).await?;
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
                            let mut release_events = self.release_bead(store, bead).await?;
                            events.append(&mut release_events);
                            let release_succeeded = events
                                .iter()
                                .any(|e| matches!(e, EventKind::BeadReleased { .. }));
                            if release_succeeded {
                                let _ = self.increment_failure_count(store, bead).await;
                            }
                            return Ok((BeadAction::Released, events));
                        }
                        Err(e) => {
                            tracing::warn!(
                                bead_id = %bead.id,
                                error = %e,
                                "shipped-work check errored — releasing orphaned bead with failure increment"
                            );
                            // On error, release and increment failure count.
                            let mut release_events = self.release_bead(store, bead).await?;
                            events.append(&mut release_events);
                            let release_succeeded = events
                                .iter()
                                .any(|e| matches!(e, EventKind::BeadReleased { .. }));
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
                    let mut release_events = self.release_bead(store, bead).await?;
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
                let mut release_events = self.release_bead(store, bead).await?;
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
                let mut release_events = self.release_bead(store, bead).await?;
                events.append(&mut release_events);
                return Ok((BeadAction::Released, events));
            }
        }

        // Flush SQLite state to JSONL so the closed/orphaned bead is captured in
        // the git-committed backup. Non-fatal: a flush failure is logged but does
        // not block the worker from picking up the next bead.
        match self.timeout_op(|| store.flush(), "flush").await {
            Ok(Some(())) => {
                tracing::debug!(bead_id = %bead.id, "flushed bead state to JSONL after success");
            }
            Ok(None) => {
                tracing::warn!(bead_id = %bead.id, "flush timed out after success — JSONL may lag SQLite");
            }
            Err(e) => {
                tracing::warn!(bead_id = %bead.id, error = %e, "flush failed after success — JSONL may lag SQLite");
            }
        }

        Ok((BeadAction::None, events))
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
        self.telemetry.emit(EventKind::VerificationFailed {
            bead_id: bead.id.clone(),
            command: failed_gate.clone(),
            exit_code: None,
            output: reason,
        })?;

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
        let mut release_events = self.release_bead(store, bead).await?;
        events.append(&mut release_events);

        // If release succeeded, increment the failure count and apply the same
        // quarantine ceiling `handle_failure` uses. Without this, a bead that
        // fails a gate every cycle is released back to open forever: the
        // ARMOR/bf-135k storm ran one bead 24 times in a single day, each
        // attempt leaving another commit behind. A gate failure is no less
        // repeatable than an agent failure and must respect the same ceiling.
        let release_succeeded = events
            .iter()
            .any(|e| matches!(e, EventKind::BeadReleased { .. }));
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

        let mut events = self.release_bead(store, bead).await?;

        // If release succeeded, increment failure count and check the
        // quarantine threshold. A bead that has exceeded
        // `outcome.quarantine_after_failures` consecutive failures is
        // quarantined (status=blocked) instead of being left released-to-open
        // for the next cycle to re-claim and fail again indefinitely. This
        // also closes the mitosis `NotSplittable` fallthrough (worker/mod.rs):
        // that verdict no longer matters for beads at or past the ceiling,
        // since this check already ran before mitosis evaluation this cycle.
        let release_succeeded = events
            .iter()
            .any(|e| matches!(e, EventKind::BeadReleased { .. }));
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

        let mut events = self.release_bead(store, bead).await?;

        // If release succeeded, increment failure count and add deferred label.
        let release_succeeded = events
            .iter()
            .any(|e| matches!(e, EventKind::BeadReleased { .. }));
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

        let events = self.release_bead(store, bead).await?;

        // Create alert bead with diagnostic info (best-effort).
        let signal_num = if signal_code > 128 {
            signal_code - 128
        } else {
            signal_code
        };
        let timestamp = Utc::now().to_rfc3339();
        let alert_title = format!("ALERT: Agent crash on bead {}", bead.id);
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

        // Hook 4: propagate stitch labels from the crashed bead to the alert.
        let signal_label = format!("signal-{}", signal_num);
        let mut alert_labels: Vec<String> = vec!["alert".into(), "crash".into(), signal_label];
        alert_labels.extend(crate::types::extract_stitch_labels(&bead.labels));
        let alert_label_refs: Vec<&str> = alert_labels.iter().map(|s| s.as_str()).collect();
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

        let events = self.release_bead(store, bead).await?;
        Ok((BeadAction::Released, events))
    }

    /// Interrupted: release bead for graceful shutdown.
    async fn handle_interrupted(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::info!(bead_id = %bead.id, "agent interrupted — releasing bead for clean shutdown");

        let events = self.release_bead(store, bead).await?;
        Ok((BeadAction::Released, events))
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

    /// Quarantine a bead by setting status=blocked and adding the 'cycling' label.
    ///
    /// This is called when a bead exceeds the configured failure threshold.
    /// Emits a BeadQuarantined telemetry event.
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

        // Set status=blocked with timeout. This is what actually stops Pluck
        // from re-selecting the bead — the 'cycling' label below is a
        // human-facing marker, not the enforcement mechanism.
        match tokio::time::timeout(std::time::Duration::from_secs(30), store.block(&bead.id)).await
        {
            Ok(Ok(())) => {
                tracing::debug!(bead_id = %bead.id, "set status=blocked");
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "failed to set status=blocked during quarantine"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "quarantine".to_string(),
                    operation: "block".to_string(),
                    error: e.to_string(),
                });
            }
            Err(_) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    "block timed out after 30s during quarantine"
                );
                events.push(EventKind::WorkerHandlingTimeout {
                    bead_id: bead.id.clone(),
                    outcome: "quarantine".to_string(),
                    operation: "block".to_string(),
                    error: "timeout after 30s".to_string(),
                });
            }
        }

        // Add the 'cycling' label with timeout.
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            store.add_label(&bead.id, "cycling"),
        )
        .await
        {
            Ok(Ok(())) => {
                tracing::debug!(
                    bead_id = %bead.id,
                    "added 'cycling' label"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "failed to add 'cycling' label"
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
        events.push(EventKind::BeadQuarantined {
            bead_id: bead.id.clone(),
            failure_count,
            threshold,
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
    }

    impl MockBeadStore {
        fn new(show_status: BeadStatus) -> Self {
            MockBeadStore {
                actions: Mutex::new(Vec::new()),
                show_status,
                labels: Vec::new(),
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
        let config = Config::default();
        let telemetry = Telemetry::with_sink("test-worker".to_string(), NopSink);
        OutcomeHandler::new(config, telemetry)
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

    // ── handle tests ──

    #[tokio::test]
    async fn handle_success_bead_closed_by_agent() {
        let handler = test_handler();
        let store = test_store(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::None);
        assert!(!result.telemetry_events.is_empty());
        let actions = store.actions();
        assert!(
            !actions.iter().any(|a| matches!(a, StoreAction::Release(_))),
            "success should not release bead"
        );
    }

    #[tokio::test]
    async fn handle_success_bead_still_open_releases_to_prevent_leak() {
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(
            result.bead_action,
            BeadAction::Released,
            "bead still open after success must be released to enforce postcondition"
        );
        let actions = store.actions();
        assert!(
            actions.iter().any(|a| matches!(a, StoreAction::Show(_))),
            "success should check bead status"
        );
        assert!(
            result
                .telemetry_events
                .iter()
                .any(|e| matches!(e, EventKind::BeadOrphaned { .. })),
            "orphan event should be emitted even though bead is released"
        );
        assert!(
            actions.iter().any(|a| matches!(a, StoreAction::Release(_))),
            "bead must be released when still open after success"
        );
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
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "failure must release bead"
        );
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
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Block(id) if id == "needle-test")),
            "5th consecutive failure must set status=blocked"
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(_, label) if label == "cycling")),
            "quarantine must add the cycling label"
        );
        assert!(
            result.telemetry_events.iter().any(|e| matches!(
                e,
                EventKind::BeadQuarantined {
                    failure_count: 5,
                    threshold: 5,
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
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "timeout must release bead"
        );
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
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "crash must release bead"
        );
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

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "agent_not_found must release bead"
        );
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
        assert_eq!(result.bead_action, BeadAction::Released);

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "interrupted must release bead"
        );
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
        let handler = test_handler();
        let store = test_store(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::None);
    }

    #[tokio::test]
    async fn handle_success_verification_passes_accepts_closure() {
        // Verification passes → bead closure accepted.
        let handler = test_handler_with_verification(vec!["true".to_string()]);
        let store = test_store(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::None);
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

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Released);

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "verification failure must release bead"
        );
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
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "verification failure must release bead after reopening"
        );
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
    async fn handle_failure_with_flush_timeout_continues_gracefully() {
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
            .await
            .unwrap();

        // Should still complete with Released action despite flush timeout.
        assert_eq!(result.outcome, Outcome::Failure);
        assert_eq!(result.bead_action, BeadAction::Released);

        // Should emit a timeout event.
        assert!(result
            .telemetry_events
            .iter()
            .any(|e| matches!(e, EventKind::WorkerHandlingTimeout { operation, .. } if operation == "flush")));
    }

    #[tokio::test]
    async fn handle_failure_with_release_timeout_continues_gracefully() {
        // Test that release timeout doesn't block the worker in HANDLING state.
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
                // Simulate a slow release that times out.
                tokio::time::sleep(std::time::Duration::from_secs(35)).await;
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

        // Should still complete despite release timeout.
        assert_eq!(result.outcome, Outcome::Failure);
        // The bead action should still be Released (best-effort).
        assert_eq!(result.bead_action, BeadAction::Released);

        // Should emit a release failure event.
        assert!(result
            .telemetry_events
            .iter()
            .any(|e| matches!(e, EventKind::BeadReleaseFailed { .. })));
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
        assert_eq!(result.bead_action, BeadAction::None);
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
        assert_eq!(result.bead_action, BeadAction::Released);
        assert!(result.telemetry_events.iter().any(
            |e| matches!(e, EventKind::BeadReleased { reason, .. } if reason == "handler_timeout")
        ));
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
        assert_eq!(result.bead_action, BeadAction::Released);
        assert!(result.telemetry_events.iter().any(
            |e| matches!(e, EventKind::BeadReleased { reason, .. } if reason == "handler_timeout")
        ));

        // Give any straggling kill signal a moment to land, then confirm the
        // gate command was actually killed, not left running in the
        // background to finish on its own.
        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
        assert!(
            !marker_path.exists(),
            "gate command was not actually killed — it ran to completion in the background"
        );
    }

    // ── Regression tests for needle-3386daef: dispatch postcondition ──

    #[tokio::test]
    async fn regression_2026_08_17_orphan_loop_bead_released_not_leaked() {
        // Regression test for the 2026-08-17 loop where needle-55ec0193 was worked
        // THREE times and leaked every time. Each worker exited with success but the
        // bead remained in_progress, allowing re-claim in the same second as the
        // success. This test verifies that an agent exiting 0 without closing the
        // bead now releases it back to open, preventing the leak.
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        // Simulate agent exiting 0 without closing the bead
        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        // Postcondition: bead must be released, not left in_progress
        assert_eq!(
            result.bead_action,
            BeadAction::Released,
            "agent exit 0 without closure must release bead to prevent claim leak"
        );

        // Verify the release was actually performed
        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "bead must be released to open/unassigned state"
        );

        // Verify BeadOrphaned event was emitted for observability
        assert!(
            result
                .telemetry_events
                .iter()
                .any(|e| matches!(e, EventKind::BeadOrphaned { .. })),
            "orphan event must be emitted even though bead is released"
        );
    }
}
