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
use crate::config::{Config, GatesConfig};
use crate::fingerprint::{
    append_alert_note, build_alert_labels, check_alert_deduplication, AlertDeduplication, AlertKind,
};
use crate::gate_health;
use crate::quarantine_expiry::{
    capped_exponential_backoff, content_hash_label, QUARANTINE_BASE_SECS, QUARANTINE_MAX_SECS,
};
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{AgentOutcome, Bead, BeadAction, BeadId, BeadStatus, HandlerResult, Outcome};
use crate::validation::{
    dod_bypass, predispatch, verify_shipped_work, GateConfig, GateReport, GateResult,
    ValidationGate,
};

/// Fleet-wide cooling period after an unsuccessful attempt.  The window grows
/// across consecutive failures so another ready bead can run instead of every
/// worker immediately reclaiming the same deterministic frontier entry.
const RETRY_COOLDOWN_BASE_SECS: u64 = 5 * 60;
const RETRY_COOLDOWN_MAX_SECS: u64 = 30 * 60;

/// The first line of a normalized failure summary, capped for a bead title.
///
/// The fingerprint label on the bead carries the full identity; the title
/// only has to be recognizable in a list.
fn summary_short(summary: &str) -> &str {
    let first_line = summary.lines().next().unwrap_or("");
    // char_indices yields byte offsets of char boundaries, so this cut is
    // UTF-8 safe.
    let end = first_line
        .char_indices()
        .map(|(i, _)| i)
        .nth(64)
        .unwrap_or(first_line.len());
    first_line[..end].trim_end()
}

/// Label prefix marking a failure-count increment applied while the bead's
/// workspace was gate-degraded (N-T22).
///
/// The value is the bead's failure count *before* the window's increment, so
/// restoration can put the count back where it was before the window opened
/// rather than wiping a history that predates it.
const DEGRADED_WINDOW_MARKER_PREFIX: &str = "degraded-window-failure:";

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
// Attempt resolution (plan section 4.4 step 1, N-T16)
// ──────────────────────────────────────────────────────────────────────────────

/// Map a process [`Outcome`] to its semantic ledger outcome.
///
/// The ledger's outcome vocabulary (plan section 3.2) separates what the work
/// did from what the lifecycle will do about it:
///
/// - `verified_success` — the work passed every gate.
/// - `work_failure` — a gate ran and rejected the work, or the agent reported
///   failure; attributable to the attempt.
/// - `infrastructure_failure` — nothing judged the work: the agent binary was
///   missing or crashed, or a gate could not run / could never pass. Feeds
///   workspace health, not the bead's failure count.
/// - `cancelled` — the worker was interrupted before a verdict.
/// - `indeterminate` — the attempt ended without a verdict: the time budget
///   expired while the work was still running.
/// - `stale_ownership` — reserved; assigned by the resolver once ownership is
///   re-checked at resolution time (ADR-024), not by this handler.
fn semantic_outcome(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Success => "verified_success",
        Outcome::Failure => "work_failure",
        Outcome::Timeout => "indeterminate",
        Outcome::AgentNotFound => "infrastructure_failure",
        Outcome::Interrupted => "cancelled",
        Outcome::Crash(_) => "infrastructure_failure",
        Outcome::GateError => "infrastructure_failure",
        Outcome::GateUnsatisfiable => "infrastructure_failure",
    }
}

/// Short machine-readable reason the attempt reached its terminal state.
///
/// Carries the detail the semantic outcome cannot: which gate rejected the
/// work, which signal killed the agent, what the exit code was. `None` on a
/// verified success, where there is nothing to explain.
fn terminal_reason(
    outcome: &Outcome,
    exit_code: i32,
    gate_report: Option<&GateReport>,
) -> Option<String> {
    match outcome {
        Outcome::Success => None,
        Outcome::Failure => {
            // A gate that ran and rejected the work names the gate; a plain
            // non-zero exit carries the code.
            let failed: Vec<&str> = gate_report
                .map(|r| {
                    r.results
                        .iter()
                        .filter(|(_, result)| !result.passed())
                        .map(|(name, _)| name.as_str())
                        .collect()
                })
                .unwrap_or_default();
            if failed.is_empty() {
                Some(format!("exit_code:{exit_code}"))
            } else {
                let mut names = failed;
                names.sort_unstable();
                Some(format!("gate:{}", names.join(",")))
            }
        }
        Outcome::Timeout => Some("timeout".to_string()),
        Outcome::AgentNotFound => Some(format!("exit_code:{exit_code}")),
        Outcome::Interrupted => Some("interrupted".to_string()),
        Outcome::Crash(code) => {
            let signal = if *code > 128 { code - 128 } else { *code };
            Some(format!("signal:{signal}"))
        }
        Outcome::GateError => {
            let errored: Vec<&str> = gate_report
                .map(|r| {
                    r.results
                        .iter()
                        .filter(|(_, result)| result.is_execution_error())
                        .map(|(name, _)| name.as_str())
                        .collect()
                })
                .unwrap_or_default();
            if errored.is_empty() {
                Some("gate_error".to_string())
            } else {
                let mut names = errored;
                names.sort_unstable();
                Some(format!("gate_error:{}", names.join(",")))
            }
        }
        Outcome::GateUnsatisfiable => {
            let names: Vec<&str> = gate_report
                .map(|r| r.results.keys().map(|k| k.as_str()).collect())
                .unwrap_or_default();
            if names.is_empty() {
                Some("gate_unsatisfiable".to_string())
            } else {
                let mut names = names;
                names.sort_unstable();
                Some(format!("gate_unsatisfiable:{}", names.join(",")))
            }
        }
    }
}

/// Convert a gate report into the ledger's per-gate entries, ordered by name.
///
/// Gate execution is not individually timed today, so `duration_ms` is 0
/// until per-gate timing exists; the field is part of the schema so consumers
/// can rely on its presence.
fn gate_result_entries(gate_report: Option<&GateReport>) -> Vec<crate::telemetry::GateResultEntry> {
    let Some(report) = gate_report else {
        return Vec::new();
    };
    let mut entries: Vec<crate::telemetry::GateResultEntry> = report
        .results
        .iter()
        .map(|(name, result)| {
            let status = match result {
                GateResult::Pass => "pass",
                GateResult::Fail(_) => "fail",
                GateResult::ExecutionError { .. } => "execution_error",
            };
            crate::telemetry::GateResultEntry {
                name: name.clone(),
                status: status.to_string(),
                duration_ms: 0,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

// ──────────────────────────────────────────────────────────────────────────────
// OutcomeHandler
// ──────────────────────────────────────────────────────────────────────────────

/// Dispatch-scoped facts about the attempt being resolved (plan section 4.4
/// step 1, N-T16).
///
/// The worker fills this in as the dispatch cycle runs — adapter identity when
/// the agent is resolved, token and cost figures once execution finishes — and
/// hands it to the outcome handler, which folds it into the single
/// `attempt.resolved` ledger event emitted at the end of
/// [`OutcomeHandler::handle`]. Without it the handler only knows the process
/// result and the bead, which is not enough to say *what* an attempt was.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttemptContext {
    /// Adapter that executed the attempt (e.g. `"claude-code-glm-4.7"`).
    pub adapter: String,
    /// Model identifier from the adapter config.
    pub model: Option<String>,
    /// Provider name (e.g. `"anthropic"`).
    pub provider: Option<String>,
    /// Prompt template that built the dispatch prompt (e.g. `"pluck"`).
    pub prompt_template: String,
    /// Version tag of that template (e.g. `"pluck-default"`).
    pub template_version: String,
    /// HEAD SHA captured just before agent dispatch, so downstream consumers
    /// can attribute commits made during the attempt.
    pub bead_revision_start: Option<String>,
    /// Commit SHAs created in the workspace between `bead_revision_start` and
    /// the end of execution.
    pub commits: Vec<String>,
    /// Identity of the predispatch snapshot this dispatch wrote, as returned
    /// by `predispatch::record`. The shipped-work cleanup removes the
    /// snapshot file only when it still carries this identity: the file is
    /// single-slot per (workspace, bead), so a completing twin dispatch would
    /// otherwise destroy the survivor's baseline and wedge every later
    /// closure on "no pre-dispatch snapshot recorded" (bead needle-e4fbe47c).
    pub predispatch_token: Option<String>,
    /// Input tokens reported by the agent's token extractor.
    pub tokens_in: Option<u64>,
    /// Output tokens reported by the agent's token extractor.
    pub tokens_out: Option<u64>,
    /// Estimated cost in USD (None when no pricing is configured).
    pub estimated_cost_usd: Option<f64>,
    /// When the cycle started (claim time), for the attempt's `duration_ms`.
    pub started_at: Option<std::time::Instant>,
}

/// Routes agent outcomes to their explicit handlers.
pub struct OutcomeHandler {
    config: Config,
    telemetry: Telemetry,
    /// Dispatch context for the attempt in flight, set by the worker each
    /// cycle before it enters HANDLING. Held behind a mutex so the setter can
    /// take `&self` — the handler is owned by the worker but its handle
    /// methods only borrow it. `take`n by [`OutcomeHandler::handle`], so a
    /// stale context can never leak into the next attempt.
    attempt_context: Arc<std::sync::Mutex<Option<AttemptContext>>>,
    /// Attempt ID of the `attempt.resolved` row this handler already emitted
    /// for the attempt in flight, if any. The exactly-once guard: the wrapper
    /// paths that end a dispatch without reaching one of `handle`'s terminal
    /// sub-handlers consult it before emitting their own row. Cleared when a
    /// new attempt is recorded ([`Self::set_attempt_context`]) — the guard
    /// answers "has *this* dispatch resolved?", never "has any dispatch ever
    /// resolved through this handler?", which would deny every later
    /// wrapper-path dispatch its row.
    ledger_row_emitted: Arc<std::sync::Mutex<Option<String>>>,
}

impl OutcomeHandler {
    pub fn new(config: Config, telemetry: Telemetry) -> Self {
        OutcomeHandler {
            config,
            telemetry,
            attempt_context: Arc::new(std::sync::Mutex::new(None)),
            ledger_row_emitted: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Record the dispatch context for the attempt about to be resolved.
    ///
    /// Called by the worker each cycle once execution has finished; the next
    /// [`OutcomeHandler::handle`] call consumes it. This is also the point
    /// where the exactly-once guard resets: the handler outlives every cycle
    /// it serves, so a guard left set by a previous dispatch would make
    /// [`Self::emit_unresolved_terminal_row`] skip a dispatch that has not
    /// resolved at all.
    pub fn set_attempt_context(&self, context: AttemptContext) {
        *self
            .ledger_row_emitted
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        *self
            .attempt_context
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(context);
    }

    /// Take the pending dispatch context, falling back to an all-unknown one.
    ///
    /// The fallback keeps direct callers (and tests that exercise a single
    /// handler without a dispatch cycle) emitting schema-valid rows rather
    /// than silently dropping the ledger event.
    fn take_attempt_context(&self) -> AttemptContext {
        self.attempt_context
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_default()
    }

    /// Non-consuming read of the in-flight attempt context.
    ///
    /// The shipped-work check needs the dispatch's own facts — the in-memory
    /// pre-dispatch HEAD and the snapshot token — while the attempt is still
    /// being resolved, but [`Self::take_attempt_context`] consumes the context
    /// for the terminal ledger row later in [`Self::handle`]. Clone, don't
    /// take.
    fn peek_attempt_context(&self) -> AttemptContext {
        self.attempt_context
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default()
    }

    /// The dispatch's in-memory shipped-work baseline, substituted when the
    /// on-disk predispatch snapshot is gone.
    ///
    /// The snapshot file is single-slot per (workspace, bead), so a twin
    /// dispatch finishing first clears it out from under this one and every
    /// later closure bounced with "no pre-dispatch snapshot recorded" no
    /// matter what was shipped (bead needle-e4fbe47c: five closes bounced
    /// until an operator intervened). `bead_revision_start` was captured by
    /// this very dispatch right before the agent ran — the same evidence
    /// quality the file would have carried. Notes have no in-memory baseline,
    /// so the gate falls back to its unreadable-at-dispatch semantics for
    /// them.
    fn fallback_predispatch(context: &AttemptContext) -> Option<predispatch::PreDispatch> {
        context
            .bead_revision_start
            .as_ref()
            .map(|head| predispatch::PreDispatch {
                head_sha: Some(head.clone()),
                notes_hash: None,
                dirty_files: Vec::new(),
                captured_at: None,
            })
    }

    /// The provisional attempt ID of the in-flight dispatch, if one is assigned.
    ///
    /// The ID is a UUIDv7 generated at dispatch start (plan section 4.4 step 1)
    /// and shared through the telemetry handle this handler was built with, so
    /// it is readable here at outcome time — ready for the attempt-resolution
    /// emission, and until then the handler's view of which dispatch it is
    /// finishing.
    pub fn attempt_id(&self) -> Option<String> {
        self.telemetry.attempt_id()
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
    ///
    /// Gates resolve from the workspace the BEAD belongs to — that
    /// workspace's own `.needle.yaml` — never from the worker's startup
    /// config, which carries the worker's HOME workspace's gates. A worker
    /// homed in a gate-declaring workspace that roams onto a foreign bead
    /// must judge that bead by the foreign workspace's own rules: workers
    /// homed in commitgraph ran commitgraph's
    /// `scripts/definition-of-done.sh --fast` against every foreign bead they
    /// touched and failed them all with exit 127 on a script that workspace
    /// never had (needle-da77b68a, live incident 2026-09-09). This is the
    /// same per-workspace resolution `bead_cli.backend` already gets, and a
    /// workspace that declares no gates runs none — not another workspace's.
    /// A workspace directory with no `.needle.yaml` at all resolves the same
    /// way: `gates_for_workspace` returns an empty `GatesConfig` and no gate
    /// runs.
    ///
    /// An unset or relative bead workspace resolves no gates at all.
    /// `gates_for_workspace` joins `.needle.yaml` onto the given path, so an
    /// empty, `.`, or otherwise relative path reads whatever config the
    /// process CWD happens to contain — during `cargo test` that is this
    /// repo's own `.needle.yaml`, whose clean-mode gates (definition-of-done
    /// plus `cargo test --lib`) then ran *inside* the unit test and crawled
    /// CI's lib lane to its timeout (full_cycle_with_echo_agent, whose bead
    /// carries workspace `.`, 2026-09-11). The claim path resolves unset
    /// workspaces onto the worker's absolute `current_workspace` before the
    /// outcome handler runs, so anything still relative here never named a
    /// real workspace; running no gates beats running gates read from an
    /// arbitrary directory.
    async fn run_verification_gates(&self, bead: &Bead) -> Result<(bool, Option<GateReport>)> {
        if bead.workspace.as_os_str().is_empty() || bead.workspace.is_relative() {
            tracing::debug!(
                bead_id = %bead.id,
                workspace = %bead.workspace.display(),
                "bead workspace is unset or relative — no workspace config to resolve gates from, running none"
            );
            return Ok((true, None));
        }
        let GatesConfig {
            gates: workspace_gates,
            verification: workspace_verification,
        } = crate::config::gates_for_workspace(&bead.workspace).with_context(|| {
            format!(
                "failed to load validation gates from the bead's workspace config {}",
                bead.workspace.join(".needle.yaml").display()
            )
        })?;

        if workspace_gates.is_empty() && workspace_verification.is_empty() {
            // The bead's workspace declares no gates — the dispatch is judged
            // on its own merits, even when the worker's home workspace gates
            // everything homed there.
            tracing::debug!(
                bead_id = %bead.id,
                workspace = %bead.workspace.display(),
                "bead's workspace declares no validation gates — running none"
            );
            return Ok((true, None));
        }
        tracing::debug!(
            bead_id = %bead.id,
            workspace = %bead.workspace.display(),
            command_gates = workspace_gates.len(),
            legacy_verification = workspace_verification.len(),
            "resolved validation gates from the bead's workspace config"
        );

        // Validate the resolved gates' command paths against the bead's own
        // workspace — the resolution-time counterpart of the boot-time check,
        // which can only ever see the worker's home declaration. A workspace
        // that declares a gate whose script does not exist there used to
        // surface only as the gate's own exit 127; name the missing path
        // first. Warn and let the gate run: the verdict still belongs to the
        // gate's execution, same contract as boot.
        if let crate::validation::GatePathValidationResult::Invalid { errors } =
            crate::validation::validate_gate_command_paths(
                &workspace_gates,
                &workspace_verification,
                &bead.workspace,
                Some(&bead.workspace.join(".needle.yaml")),
            )
        {
            for error in &errors {
                tracing::warn!(
                    bead_id = %bead.id,
                    workspace = %bead.workspace.display(),
                    command = %error.command,
                    path = %error.path,
                    path_type = ?error.path_type,
                    "gate.command_missing: bead-workspace gate command path does not exist — \
                     the gate will fail when it runs"
                );
            }
            if let Err(error) = self.telemetry.emit(
                EventKind::GatePathMissing {
                    count: errors.len(),
                    paths: errors.iter().map(|e| e.path.clone()).collect(),
                },
                chrono::Utc::now(),
            ) {
                tracing::warn!(error = %error, "failed to emit gate.path_missing");
            }
        }

        // Pluggable gates first, fall back to legacy verification commands.
        // Fill in each command gate's stderr cap from
        // `validation.stderr_cap_bytes` unless the gate already set its own
        // override — see GitHub issue jedarden/NEEDLE#9. The cap is a
        // host-level setting (`validation` is not workspace-overridable), so
        // the worker's resolved value applies to foreign workspaces too.
        let gate_opt = if !workspace_gates.is_empty() {
            let default_stderr_cap = self.config.validation.stderr_cap_bytes;
            let gate_configs: Vec<(String, GateConfig)> = workspace_gates
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
        } else {
            // Legacy verification command format.
            ValidationGate::from_commands_with_stderr_cap(
                workspace_verification,
                bead.workspace.clone(),
                self.config.validation.stderr_cap_bytes,
            )
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

        // Gate evidence is captured before the routing match below, which
        // moves the report into the terminal handlers (N-T16 ledger row).
        let gate_results = gate_result_entries(gate_report.as_ref());
        let resolved_outcome = semantic_outcome(&outcome);
        let resolved_reason = terminal_reason(&outcome, output.exit_code, gate_report.as_ref());

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

        self.emit_attempt_resolved(
            bead,
            output,
            bead_action.to_string(),
            resolved_outcome,
            resolved_reason,
            gate_results,
        );

        Ok(HandlerResult {
            outcome,
            bead_action,
            telemetry_events,
        })
    }

    /// Emit the terminal `attempt.resolved` ledger row for this dispatch.
    ///
    /// Plan section 4.4 step 1 (N-T16): every dispatch produces exactly one
    /// row recording what the attempt was and how it resolved. The emission
    /// sits after the match that routes to the eight terminal sub-handlers —
    /// not inside them — so a sub-handler that re-routes into another
    /// (`handle_success` → `handle_gate_failure` on a shipped-work failure)
    /// still yields a single event, and no terminal path can forget it.
    ///
    /// `attempt_id` and `provisional: true` are fixed until N-T03 resolves
    /// attempts against beads: the ID is the dispatch-local UUIDv7 shared by
    /// every event in the cycle (read back through the telemetry handle), and
    /// no consumer may treat a provisional row as authoritative.
    fn emit_attempt_resolved(
        &self,
        bead: &Bead,
        output: &AgentOutcome,
        requested_action: String,
        resolved_outcome: &str,
        resolved_reason: Option<String>,
        gate_results: Vec<crate::telemetry::GateResultEntry>,
    ) {
        let attempt = self.take_attempt_context();
        let attempt_id = match self.telemetry.attempt_id() {
            Some(id) => id,
            None => {
                // No dispatch cycle assigned one (direct handler use). The row
                // still needs an ID; mint a provisional one rather than emitting
                // an event that cannot be joined to anything. Publishing it to
                // the telemetry handle keeps the event envelope — stamped from
                // the same cell — in agreement with the row's own field.
                let minted = uuid::Uuid::now_v7().to_string();
                self.telemetry.set_attempt_id(minted.clone());
                minted
            }
        };
        let duration_ms = attempt
            .started_at
            .map(|started| started.elapsed().as_millis() as u64)
            .unwrap_or(0);

        // Record the ID before the emit: the wrapper paths below use this to
        // keep their fallback rows from doubling a row that already exists.
        *self
            .ledger_row_emitted
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(attempt_id.clone());

        let event = EventKind::AttemptResolved(Box::new(crate::telemetry::AttemptResolvedFields {
            attempt_id,
            // Always true until N-T03 lands: NEEDLE cannot yet attest that an
            // attempt ID identifies a durable attempt rather than a dispatch.
            provisional: true,
            bead_id: bead.id.clone(),
            workspace: bead.workspace.display().to_string(),
            bead_revision_start: attempt.bead_revision_start,
            worker: self.telemetry.worker_id().to_string(),
            adapter: attempt.adapter,
            model: attempt.model,
            provider: attempt.provider,
            prompt_template: attempt.prompt_template,
            template_version: attempt.template_version,
            // ContextManifest hashing is N-T10; the hash is absent until then.
            context_manifest_hash: None,
            gate_results,
            outcome: resolved_outcome.to_string(),
            requested_action,
            // The authoritative post-action state comes from the resolver's
            // re-read (plan section 3.2 step 5); the action is only requested
            // here, so no state is claimed.
            confirmed_state: None,
            tokens_in: attempt.tokens_in,
            tokens_out: attempt.tokens_out,
            estimated_cost_usd: attempt.estimated_cost_usd,
            commits: attempt.commits,
            duration_ms,
            terminal_reason: resolved_reason,
            // The exit code is observation only: it says what the process did,
            // not whether the work was accepted — `outcome` carries that.
            exit_code: output.exit_code,
        }));

        let timestamp = Utc::now();
        if let Err(e) = self.telemetry.emit_try_lock(event, timestamp) {
            // Losing the ledger row is worse than losing any other event in
            // this handler — surface it even though the dispatch continues.
            tracing::warn!(
                bead_id = %bead.id,
                error = %e,
                "failed to enqueue attempt.resolved ledger event"
            );
        }
    }

    /// Emit the ledger row for a dispatch that ended without reaching one of
    /// [`OutcomeHandler::handle`]'s terminal sub-handlers: cancelled before
    /// handling, the handler's own timeout, or a handler error. These paths
    /// used to leave the dispatch with no row at all, which is exactly the
    /// hole a durable ledger cannot have (plan section 4.4 step 1).
    ///
    /// Guarded by [`Self::ledger_row_emitted`]: if `handle` already emitted
    /// its row before the dispatch was torn down, this does nothing, so a
    /// dispatch still resolves to exactly one row.
    fn emit_unresolved_terminal_row(
        &self,
        bead: &Bead,
        output: &AgentOutcome,
        requested_action: &str,
        resolved_outcome: &str,
        reason: &str,
    ) {
        if self
            .ledger_row_emitted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return;
        }
        self.emit_attempt_resolved(
            bead,
            output,
            requested_action.to_string(),
            resolved_outcome,
            Some(reason.to_string()),
            // No gate evidence exists on these paths: verification never
            // ran, or never got far enough to record a verdict.
            Vec::new(),
        );
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
            //
            // The dispatch is still terminal and still gets its ledger row:
            // `handle` never ran, so nothing else emits it.
            self.emit_unresolved_terminal_row(
                bead,
                output,
                "Errored",
                "cancelled",
                "cancelled_before_handling",
            );
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
                // Nothing judged the work — the attempt died inside the
                // handler. Emit unless `handle` got far enough to emit its
                // own row before failing.
                self.emit_unresolved_terminal_row(
                    bead,
                    output,
                    "Errored",
                    "infrastructure_failure",
                    "outcome_handler_error",
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
                // The aborted `handle` future cannot have finished its ledger
                // row — emit one here, or the dispatch ends with none. If the
                // future was dropped after emitting, the guard inside skips.
                self.emit_unresolved_terminal_row(
                    bead,
                    output,
                    "Errored",
                    "indeterminate",
                    "outcome_handler_timeout",
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
                // This dispatch's own facts: the in-memory pre-dispatch HEAD
                // substitutes for a vanished snapshot file, and the snapshot
                // token scopes the cleanup below to this dispatch's baseline.
                let attempt = self.peek_attempt_context();
                let fallback = Self::fallback_predispatch(&attempt);

                if self.config.worker.enforce_shipped_work {
                    match verify_shipped_work(&current, &bead.workspace, store, fallback.as_ref())
                        .await
                    {
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
                            let snapshot = predispatch::load(&bead.workspace, &bead.id)
                                .await
                                .or(fallback);
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
                // next claim of this bead starts from a fresh baseline. Only
                // this dispatch's own snapshot: a twin dispatch on the same
                // bead may have overwritten the single-slot file, and the
                // survivor's baseline is the one still in flight
                // (needle-e4fbe47c).
                crate::validation::predispatch::clear_if_own(
                    &bead.workspace,
                    &bead.id,
                    attempt.predispatch_token.as_deref(),
                )
                .await;

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
                    let attempt = self.peek_attempt_context();
                    let fallback = Self::fallback_predispatch(&attempt);
                    match verify_shipped_work(&current, &bead.workspace, store, fallback.as_ref())
                        .await
                    {
                        Ok(crate::validation::GateResult::Pass) => {
                            tracing::info!(
                                bead_id = %bead.id,
                                "shipped work detected — marking orphaned bead as completed"
                            );
                            // Clear the predispatch snapshot since work is complete —
                            // only this dispatch's own baseline (see the twin-dispatch
                            // note on the closed-bead path).
                            crate::validation::predispatch::clear_if_own(
                                &bead.workspace,
                                &bead.id,
                                attempt.predispatch_token.as_deref(),
                            )
                            .await;
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
                output: reason.clone(),
            },
            chrono::Utc::now(),
        )?;

        // N-T22: fingerprint the failure against the workspace's sliding
        // window BEFORE penalising the bead. When one fingerprint — gate name
        // plus normalized output — dominates recent failures across several
        // distinct beads, no bead is at fault: from 2026-09-01 the clean gate
        // failed identically on every bead it judged for two days, 40 beads
        // were penalised and 9 quarantined before a human noticed. A trip
        // degrades the workspace exactly as three consecutive GateErrors do
        // (needle-0abc120d), and both the tripping failure and any later
        // failure carrying the degraded fingerprint release without a
        // penalty.
        let verification_recording = match gate_health::record_verification_failure(
            &bead.workspace,
            bead.id.as_ref(),
            &failed_gate,
            &reason,
            &self.config.workspace_health.detector_config(),
        ) {
            Ok(recording) => {
                if let gate_health::VerificationRecording::Tripped {
                    fingerprint,
                    failures,
                    distinct_beads,
                    summary,
                } = &recording
                {
                    tracing::error!(
                        bead_id = %bead.id,
                        workspace = %bead.workspace.display(),
                        gate = %failed_gate,
                        fingerprint = %fingerprint,
                        failures = failures,
                        distinct_beads = distinct_beads,
                        "verification-failure fingerprint dominates the workspace window — degrading"
                    );
                    if let Err(e) = self
                        .create_or_update_fingerprint_gate_broken_bead(
                            store,
                            bead,
                            &failed_gate,
                            fingerprint,
                            summary,
                            *failures,
                            *distinct_beads,
                        )
                        .await
                    {
                        tracing::error!(
                            error = %e,
                            workspace = %bead.workspace.display(),
                            "failed to create or update the fingerprint Gate broken bead"
                        );
                    }
                }
                Some(recording)
            }
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "could not record the verification fingerprint — treating the failure as a bead failure"
                );
                None
            }
        };
        let infra_failure = verification_recording
            .as_ref()
            .is_some_and(gate_health::VerificationRecording::is_infra);

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
        if infra_failure {
            // The gate, not the bead, produced this failure. Release without
            // incrementing the failure count — the same contract as a gate
            // that could not run at all — so no bead can reach quarantine on
            // a fingerprint that degraded the workspace.
            tracing::info!(
                bead_id = %bead.id,
                workspace = %bead.workspace.display(),
                gate = %failed_gate,
                fingerprint = verification_recording
                    .as_ref()
                    .map(|r| r.fingerprint().to_string())
                    .unwrap_or_default(),
                "infrastructure verification failure — releasing bead without incrementing failure count"
            );
        } else if release_succeeded {
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

        // Add a label indicating verification failure — but not for an
        // infrastructure failure: the bead's work was never judged, and a
        // `verification-failed` label would misrank it in later selection.
        if !infra_failure {
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
        }

        Ok((action, events))
    }

    /// Create — or leave open and reuse — the single "Gate broken" bead for a
    /// fingerprint-tripped degradation (N-T22).
    ///
    /// This is the verification-failure sibling of
    /// [`OutcomeHandler::create_gate_broken_bead`]: that one answers "the gate
    /// could not run", this one answers "the gate ran and failed everything
    /// the same way". The bead carries the detector's fingerprint as a label,
    /// so repeated trips fold into one claimable repair instead of one bead
    /// per penalised attempt.
    #[allow(clippy::too_many_arguments)] // the fingerprint alert's shape; see its siblings
    async fn create_or_update_fingerprint_gate_broken_bead(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        gate: &str,
        fingerprint: &str,
        summary: &str,
        failures: usize,
        distinct_beads: usize,
    ) -> Result<()> {
        let workspace = bead.workspace.display().to_string();
        let label = crate::verification_fingerprint::fingerprint_label(fingerprint);

        // One bead per fingerprint. An open bead carrying this label is the
        // repair ticket already in flight — re-emit the degradation against
        // it rather than filing a duplicate.
        let all_beads = store
            .list_all()
            .await
            .context("failed to list beads while filing the fingerprint Gate broken bead")?;
        let existing = all_beads.iter().find(|b| {
            b.workspace == bead.workspace
                && b.status != BeadStatus::Closed
                && b.labels.iter().any(|l| l == &label)
        });

        let bead_title = format!(
            "Gate broken: {} — {} (fingerprint:{})",
            gate,
            summary_short(summary),
            fingerprint
        );

        if let Some(existing) = existing {
            tracing::info!(
                bead_id = %existing.id,
                fingerprint,
                workspace = %workspace,
                "fingerprint Gate broken bead already open — reusing it"
            );
            self.telemetry.emit(
                EventKind::WorkspaceGateDegraded {
                    workspace,
                    gate: gate.to_string(),
                    command: summary.to_string(),
                    reason: format!(
                        "fingerprint covers {} of recent failures across {} beads",
                        failures, distinct_beads
                    ),
                    consecutive_errors: failures as u32,
                    bead_id: existing.id.clone(),
                },
                Utc::now(),
            )?;
            return Ok(());
        }

        let bead_body = format!(
            "## Shared verification-failure fingerprint\n\
             \n\
             The gate `{}` in workspace `{}` failed with one identical \
             fingerprint on {} of the workspace's recent verification \
             failures, across {} distinct beads. The signal is statistical, \
             not textual: when every failure in a workspace shares a \
             fingerprint, no bead is at fault — the gate is.\n\
             \n\
             ### Failure shape\n\
             - **Gate**: {}\n\
             - **Normalized output**: `{}`\n\
             - **Fingerprint**: `{}`\n\
             - **Window failures**: {}\n\
             - **Distinct beads affected**: {}\n\
             \n\
             ### Impact\n\
             This workspace is now **gate-degraded**: Pluck and Explore skip \
             it for ordinary dispatch, and failures carrying this fingerprint \
             no longer increment any bead's failure count. The workspace \
             remains claimable — repairing the gate is verified by running it.\n\
             \n\
             ### Resolution\n\
             Fix the gate so a clean run passes, and the next successful \
             verification in this workspace will clear the degradation, close \
             this bead, and undo the failure penalties recorded while the \
             window was degraded.\n",
            gate,
            workspace,
            failures,
            distinct_beads,
            gate,
            summary,
            fingerprint,
            failures,
            distinct_beads,
        );

        let labels = [label, "infra".to_string(), "priority:0".to_string()];
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

        let created = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            store.create_bead(&bead_title, &bead_body, &label_refs),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!("create_bead timed out after 30s during fingerprint degradation")
        })?
        .context("failed to create the fingerprint Gate broken bead")?;

        tracing::info!(
            bead_id = %created,
            fingerprint,
            workspace = %workspace,
            "created the Gate broken bead for a fingerprint-degraded workspace"
        );

        self.telemetry.emit(
            EventKind::WorkspaceGateDegraded {
                workspace,
                gate: gate.to_string(),
                command: summary.to_string(),
                reason: format!(
                    "fingerprint covers {} of recent failures across {} beads",
                    failures, distinct_beads
                ),
                consecutive_errors: failures as u32,
                bead_id: created,
            },
            Utc::now(),
        )?;

        Ok(())
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

        // N-T22: the gate is proven good again, so the failure penalties
        // recorded while the window was degraded no longer describe the
        // work. Undo them before the workspace re-enters rotation so a
        // penalised bead does not come back one failure from quarantine on
        // counts the broken gate gave it.
        match self
            .undo_degraded_window_penalties(store, workspace_path)
            .await
        {
            Ok(undone) if undone > 0 => {
                tracing::info!(
                    workspace = %workspace,
                    beads_reset = undone,
                    "undid failure penalties recorded during the degraded window"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    workspace = %workspace,
                    error = %e,
                    "failed to undo degraded-window penalties — beads keep the counts the broken gate gave them"
                );
            }
        }

        Ok(())
    }

    /// Undo the failure penalties applied while a workspace was
    /// gate-degraded (N-T22).
    ///
    /// Every increment made during the degraded window carries a
    /// `degraded-window-failure:<pre-count>` marker (see
    /// `increment_failure_count`). The minimum marked count on a bead is
    /// therefore where it stood when the window opened: the count returns
    /// there, and the quarantine and cooldown labels those counts earned go
    /// with them. Penalties from before the window are left alone.
    ///
    /// Returns how many beads were reset.
    async fn undo_degraded_window_penalties(
        &self,
        store: &dyn BeadStore,
        workspace_path: &std::path::Path,
    ) -> Result<usize> {
        let all_beads = store
            .list_all()
            .await
            .context("failed to list beads while undoing degraded-window penalties")?;

        let marked: Vec<&Bead> = all_beads
            .iter()
            .filter(|b| {
                b.workspace == workspace_path
                    && b.labels
                        .iter()
                        .any(|l| l.starts_with(DEGRADED_WINDOW_MARKER_PREFIX))
            })
            .collect();

        let mut reset = 0;
        for marked_bead in marked {
            let labels = match self
                .timeout_op(|| store.labels(&marked_bead.id), "labels")
                .await
            {
                Ok(Some(labels)) => labels,
                Ok(None) => {
                    tracing::warn!(
                        bead_id = %marked_bead.id,
                        "labels() timed out while undoing a degraded-window penalty"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        bead_id = %marked_bead.id,
                        error = %e,
                        "could not read labels to undo a degraded-window penalty"
                    );
                    continue;
                }
            };

            // The oldest marker wins: a bead penalised repeatedly inside the
            // window carries its pre-window count from the first increment.
            let pre_window = labels
                .iter()
                .filter_map(|l| l.strip_prefix(DEGRADED_WINDOW_MARKER_PREFIX))
                .filter_map(|n| n.parse::<u32>().ok())
                .min();

            let mut removed_quarantine = false;
            for label in labels.iter().filter(|l| {
                l.starts_with(DEGRADED_WINDOW_MARKER_PREFIX)
                    || l.starts_with("failure-count:")
                    || l.starts_with("quarantine")
                    || l.as_str() == "cycling"
            }) {
                if label.starts_with("quarantine") {
                    removed_quarantine = true;
                }
                match self
                    .timeout_op(
                        || store.remove_label(&marked_bead.id, label),
                        "remove_label",
                    )
                    .await
                {
                    Ok(Some(())) => {}
                    Ok(None) => {
                        tracing::warn!(
                            bead_id = %marked_bead.id,
                            label,
                            "remove_label timed out while undoing a degraded-window penalty"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            bead_id = %marked_bead.id,
                            label,
                            error = %e,
                            "failed to remove a degraded-window label"
                        );
                    }
                }
            }

            // Restore the count the bead carried before the window opened.
            if let Some(pre_window) = pre_window.filter(|count| *count > 0) {
                let restored = format!("failure-count:{}", pre_window);
                match self
                    .timeout_op(|| store.add_label(&marked_bead.id, &restored), "add_label")
                    .await
                {
                    Ok(Some(())) => {}
                    Ok(None) => {
                        tracing::warn!(
                            bead_id = %marked_bead.id,
                            "add_label timed out restoring the pre-window failure count"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            bead_id = %marked_bead.id,
                            error = %e,
                            "failed to restore the pre-window failure count"
                        );
                    }
                }
            }

            if removed_quarantine {
                // The quarantine this bead earned inside the window is over;
                // say so in the same vocabulary ADR-022 uses for one that
                // expires. Non-fatal: the reset already happened.
                if let Err(e) = self.telemetry.emit(
                    EventKind::QuarantineExpired {
                        bead_id: marked_bead.id.clone(),
                    },
                    Utc::now(),
                ) {
                    tracing::warn!(
                        bead_id = %marked_bead.id,
                        error = %e,
                        "failed to emit quarantine expiry for a degraded-window reset"
                    );
                }
            }

            tracing::info!(
                bead_id = %marked_bead.id,
                restored_count = pre_window.unwrap_or(0),
                "reset a bead penalised during the degraded window"
            );
            reset += 1;
        }

        Ok(reset)
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
        // quarantined behind an expiring fleet-visible label instead of being
        // left immediately claimable for the next cycle. This
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

    /// Timeout: release the bead and let it cool down behind an expiring
    /// deferral.
    ///
    /// The handler only increments the failure count — the round the deferral
    /// ladder reads. The `deferred:<rfc3339>` hold itself is written by the
    /// worker's `apply_bead_action`, the single choke point for
    /// [`BeadAction::Deferred`], so no path can pair the action with the
    /// permanent bare `deferred` label again (N-T26).
    ///
    /// If br calls timeout or fail, logs the error and continues — does not
    /// block the worker in HANDLING state indefinitely.
    async fn handle_timeout(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::warn!(bead_id = %bead.id, "agent timed out — releasing bead as deferred");

        let events = self.prepare_release_events(store, bead).await?;

        // The release itself now happens later in the worker's apply_bead_action(),
        // so no BeadReleased event exists here. Testing for one made this always
        // false, silently disabling the failure-count/quarantine follow-up. Treat
        // the prepare step as successful when it reported no error.
        let release_succeeded = !events
            .iter()
            .any(|e| matches!(e, EventKind::WorkerHandlingTimeout { .. }));
        if release_succeeded {
            // Increment failure count for auto-split tracking; the count is
            // also the deferral ladder round the applied hold will use.
            if let Err(e) = self.increment_failure_count(store, bead).await {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "failed to increment failure count after timeout"
                );
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

        // N-T22: a penalty applied while the bead's workspace is
        // gate-degraded is provisional. The workspace's recent failures say
        // the gate may be at fault, so mark the increment with the
        // pre-increment count: if a clean verification restores the
        // workspace, the penalty reset in `undo_degraded_window_penalties`
        // uses that marker to undo exactly the penalties the degraded window
        // added, and no others.
        match gate_health::is_degraded(&bead.workspace) {
            Ok(true) => {
                let marker = format!("{}{}", DEGRADED_WINDOW_MARKER_PREFIX, current_count);
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    store.add_label(&bead.id, &marker),
                )
                .await
                {
                    Ok(Ok(())) => {
                        tracing::info!(
                            bead_id = %bead.id,
                            pre_window_count = current_count,
                            "failure count incremented during a degraded window — penalty marked for reset"
                        );
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            error = %e,
                            "failed to mark the degraded-window penalty — restoration will not undo this increment"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            "degraded-window marker add_label timed out after 30s"
                        );
                    }
                }
            }
            Ok(false) => {}
            Err(e) => {
                tracing::debug!(
                    bead_id = %bead.id,
                    error = %e,
                    "could not read gate health state — degraded-window marker skipped"
                );
            }
        }

        // The bead-rs ready frontier is deterministic.  Releasing a failed
        // bead without changing its eligibility makes the very next worker
        // select it again, which is retry churn rather than distribution.
        // Persist a short expiring exclusion on the bead so every worker sees
        // the same cooldown.  The full quarantine path below replaces this
        // window when the configured failure ceiling is reached.
        let threshold = self.config.outcome.quarantine_after_failures;
        if threshold == 0 || new_count < threshold {
            if let Err(error) = self.apply_retry_cooldown(store, bead, new_count).await {
                tracing::warn!(
                    bead_id = %bead.id,
                    failure_count = new_count,
                    error = %error,
                    "failed to apply retry cooldown; bead remains eligible"
                );
            }
        }

        Ok(new_count)
    }

    /// Replace the bead's current expiry label with a bounded retry window.
    ///
    /// This is deliberately stored on the bead rather than in one worker's
    /// memory: a local exclusion merely hands the same bead to a peer.  Pluck
    /// already treats a future `quarantine-until` label as a never-relaxed
    /// constraint and automatically admits it after expiry.
    async fn apply_retry_cooldown(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        failure_count: u32,
    ) -> Result<()> {
        let labels =
            tokio::time::timeout(std::time::Duration::from_secs(30), store.labels(&bead.id))
                .await
                .context("labels() timed out while applying retry cooldown")??;

        for label in labels
            .iter()
            .filter(|label| label.starts_with("quarantine-until:"))
        {
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                store.remove_label(&bead.id, label),
            )
            .await
            .context("remove_label() timed out while applying retry cooldown")??;
        }

        let exponent = failure_count.saturating_sub(1);
        let cooldown_secs =
            capped_exponential_backoff(RETRY_COOLDOWN_BASE_SECS, exponent, RETRY_COOLDOWN_MAX_SECS);
        let until = Utc::now() + chrono::Duration::seconds(cooldown_secs as i64);
        let label = format!("quarantine-until:{}", until.to_rfc3339());
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            store.add_label(&bead.id, &label),
        )
        .await
        .context("add_label() timed out while applying retry cooldown")??;

        tracing::info!(
            bead_id = %bead.id,
            failure_count,
            cooldown_secs,
            retry_after = %until.to_rfc3339(),
            "deferred failed bead behind a fleet-wide retry cooldown"
        );
        Ok(())
    }

    /// Reset failure and retry state after verified shipped work.
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

        // Remove every retry/quarantine label. A successful dispatch after an
        // expired quarantine starts a fresh failure series; leaving an old
        // future timestamp behind would hide otherwise healthy work. Stale
        // `deferred:<rfc3339>` windows (N-T26) are cleared here too — only the
        // expiring form, never the operator-owned bare `deferred`.
        let mut removed_count = 0;
        for label in &labels {
            if label.starts_with("failure-count:")
                || label.starts_with("quarantine-until:")
                || label.starts_with("quarantine-round:")
                || label.starts_with("quarantine:")
                || label.starts_with("deferred:")
                || label == "quarantined"
                || label == "cycling"
            {
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
                            "failed to remove failure/quarantine label"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            label,
                        "remove_label timed out after 30s while resetting failure state"
                        );
                    }
                }
            }
        }

        if removed_count > 0 {
            tracing::debug!(
                bead_id = %bead.id,
                removed_count,
                "failure and quarantine state reset"
            );
        }

        Ok(())
    }

    /// Quarantine a bead with an expiring, fleet-visible label window.
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
        let labels = self
            .timeout_op(|| store.labels(&bead.id), "quarantine labels")
            .await?
            .ok_or_else(|| anyhow::anyhow!("timed out reading labels for quarantine"))?;
        let prior_round = labels
            .iter()
            .filter_map(|label| label.strip_prefix("quarantine-round:"))
            .filter_map(|round| round.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        let round = prior_round.saturating_add(1);
        let quarantine_secs = capped_exponential_backoff(
            QUARANTINE_BASE_SECS,
            round.saturating_sub(1),
            QUARANTINE_MAX_SECS,
        );
        let until = Utc::now() + chrono::Duration::seconds(quarantine_secs as i64);
        let round_label = format!("quarantine-round:{round}");
        let until_label = format!("quarantine-until:{}", until.to_rfc3339());
        let reason_label = format!("quarantine:failure-count:{failure_count}");
        // Stamp the content this quarantine was taken against, so the
        // expiry-time re-evaluation (quarantine_expiry) can tell "still
        // broken" apart from "someone edited the bead since".
        let hash_label = content_hash_label(bead);

        tracing::warn!(
            bead_id = %bead.id,
            failure_count,
            threshold,
            round,
            quarantine_secs,
            until = %until.to_rfc3339(),
            "quarantining bead after exceeding failure threshold"
        );

        // Add the new exclusion before pruning old labels.  If a backend call
        // fails midway, the bead remains excluded by either the prior retry
        // window or the new one rather than briefly returning to the frontier.
        for label in [
            "quarantined".to_string(),
            round_label.clone(),
            until_label.clone(),
            reason_label,
            hash_label.clone(),
        ] {
            self.timeout_op(|| store.add_label(&bead.id, &label), "quarantine add_label")
                .await?
                .ok_or_else(|| anyhow::anyhow!("timed out adding quarantine label"))?;
        }

        for label in labels.iter().filter(|label| {
            (label.starts_with("quarantine-round:") && label.as_str() != round_label)
                || (label.starts_with("quarantine-until:") && label.as_str() != until_label)
                || (label.starts_with(crate::quarantine_expiry::CONTENT_HASH_LABEL_PREFIX)
                    && label.as_str() != hash_label)
        }) {
            self.timeout_op(
                || store.remove_label(&bead.id, label),
                "quarantine remove_label",
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("timed out pruning old quarantine label"))?;
        }

        let events = vec![
            EventKind::BeadQuarantined {
                bead_id: bead.id.clone(),
                round,
                until: until.to_rfc3339(),
                failure_count,
            },
            // Keep the pre-existing false-close signal for dashboards and
            // operators alongside the more specific quarantine event.
            EventKind::FalseCloseDetected {
                bead_id: bead.id.clone(),
                failure_count,
                threshold,
                reason: "shipped-work-verification-failed".to_string(),
            },
        ];

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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    // ── Test environment isolation ──

    /// Pin `$HOME` to a private directory for the whole test body.
    ///
    /// Gate-health state and predispatch snapshots live under
    /// `$HOME/.needle/state`, so a test whose flow reaches
    /// `handle_gate_failure` or `handle_gate_error` reads and writes the
    /// *fleet's* state files unless HOME is private — the same failure that
    /// accumulated `degraded: true` on the real `/tmp` workspace (see
    /// [`test_workspace`]). The guard serializes with every other test that
    /// swaps HOME; hold it for the whole body.
    fn isolated_home() -> (crate::util::test_env::EnvGuard, tempfile::TempDir) {
        let guard = crate::util::test_env::isolate_env();
        let home = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());
        (guard, home)
    }

    /// A unique, never-existing workspace path for each test bead.
    ///
    /// This used to be `std::env::temp_dir()` — the literal shared `/tmp`, a
    /// real path whose gate-health state file
    /// (`$HOME/.needle/state/gate-health/<sha256("/tmp")[..12]>.json`) is a
    /// single stable key shared by every test run, every concurrent test, and
    /// any real fleet worker running in `/tmp`. Both `handle_gate_error` and
    /// `handle_gate_failure` write that state, so tests piled
    /// `degraded: true` onto the fleet's real `/tmp` entry and raced each
    /// other on it (observed 2026-09-09: `e9671acd2448.json` at
    /// `consecutive_errors: 7`). Each bead now carries its own path, so a
    /// test that forgets [`isolated_home`] above still only ever collides
    /// with itself — and can no longer masquerade as a workspace the fleet
    /// might actually use.
    fn test_workspace() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "needle-outcome-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

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
            workspace: test_workspace(),
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

    /// The workspace a test bead carries must never be a path a real worker
    /// could also carry: gate-health state is keyed by a hash of the workspace
    /// path under `$HOME/.needle/state/gate-health`, so a shared fixture path
    /// would give every test run — and the fleet — one colliding state file.
    #[test]
    fn test_bead_workspaces_are_unique_and_never_a_shared_real_path() {
        let a = test_bead(BeadStatus::Open).workspace;
        let b = test_bead(BeadStatus::Open).workspace;
        assert_ne!(a, b, "each test bead needs its own gate-health state key");
        assert_ne!(
            a,
            std::env::temp_dir(),
            "the temp root itself is a real path with a fleet-visible state key"
        );
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
        let (_guard, _home) = isolated_home();
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let (_ws, bead) = bead_in_workspace_with_verification(&["false".to_string()]);

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

    #[test]
    fn retry_and_quarantine_backoff_is_bounded() {
        assert_eq!(
            capped_exponential_backoff(RETRY_COOLDOWN_BASE_SECS, 0, RETRY_COOLDOWN_MAX_SECS),
            300
        );
        assert_eq!(
            capped_exponential_backoff(RETRY_COOLDOWN_BASE_SECS, 3, RETRY_COOLDOWN_MAX_SECS),
            1800
        );
        assert_eq!(
            capped_exponential_backoff(RETRY_COOLDOWN_BASE_SECS, 20, RETRY_COOLDOWN_MAX_SECS),
            1800
        );
        assert_eq!(
            capped_exponential_backoff(QUARANTINE_BASE_SECS, 0, QUARANTINE_MAX_SECS),
            7200
        );
        assert_eq!(
            capped_exponential_backoff(QUARANTINE_BASE_SECS, 10, QUARANTINE_MAX_SECS),
            172800
        );
    }

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
                .any(|a| matches!(a, StoreAction::AddLabel(id, label) if id == "needle-test" && label == "quarantined")),
            "5th consecutive failure must mark the bead quarantined"
        );
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "quarantine-round:1")
            ),
            "first quarantine must record round 1, got: {actions:?}"
        );
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label.starts_with("quarantine-until:"))
            ),
            "quarantine must carry an expiry, got: {actions:?}"
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
    async fn repeated_quarantine_advances_round_and_replaces_expiry() {
        let handler = test_handler();
        let old_until = "quarantine-until:2026-01-01T00:00:00+00:00";
        let store = MockBeadStore::new(BeadStatus::InProgress).with_labels(vec![
            "failure-count:4".to_string(),
            "quarantined".to_string(),
            "quarantine-round:2".to_string(),
            old_until.to_string(),
        ]);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(1), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Quarantined);
        let actions = store.actions();
        assert!(actions.iter().any(
            |action| matches!(action, StoreAction::AddLabel(_, label) if label == "quarantine-round:3")
        ));
        assert!(actions.iter().any(
            |action| matches!(action, StoreAction::RemoveLabel(_, label) if label == old_until)
        ));
        assert!(result.telemetry_events.iter().any(|event| matches!(
            event,
            EventKind::BeadQuarantined { round: 3, until, .. } if !until.is_empty()
        )));
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
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label.starts_with("quarantine-until:"))
            ),
            "a below-threshold failure must receive a retry cooldown"
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
        let (_guard, _home) = isolated_home();
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
        let (_guard, _home) = isolated_home();
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
        assert!(actions
            .iter()
            .any(|a| matches!(a, StoreAction::AddLabel(_, label) if label == "quarantined")));
        assert!(actions.iter().any(
            |a| matches!(a, StoreAction::AddLabel(_, label) if label.starts_with("quarantine-until:"))
        ));
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
        let (_guard, _home) = isolated_home();
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
        let (_env, _home) = isolated_home();

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
    async fn handle_timeout_returns_deferred_without_a_permanent_label() {
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
            !actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(_, label) if label == "deferred")),
            "the outcome handler must never add the permanent bare deferred label"
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

    /// A test bead whose workspace is a real directory declaring the given
    /// legacy `verification:` commands in its own `.needle.yaml`.
    ///
    /// Gates resolve from the bead's workspace config (needle-da77b68a), so a
    /// gate that must RUN is declared where resolution reads it — the bead's
    /// own `.needle.yaml`. The handler's `Config` models the worker's home
    /// workspace, which a foreign bead is deliberately not judged by. The
    /// directory is real because it is the gate command's cwd; drop the
    /// returned guard to remove it.
    fn bead_in_workspace_with_verification(commands: &[String]) -> (tempfile::TempDir, Bead) {
        let workspace = tempfile::TempDir::new().unwrap();
        let yaml = serde_yaml::to_string(&serde_json::json!({ "verification": commands }))
            .expect("fixture yaml serializes");
        std::fs::write(workspace.path().join(".needle.yaml"), yaml)
            .expect("fixture .needle.yaml writes");
        let bead = Bead {
            workspace: workspace.path().to_path_buf(),
            ..test_bead(BeadStatus::InProgress)
        };
        (workspace, bead)
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
        let (_guard, _home) = isolated_home();
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let (_ws, bead) = bead_in_workspace_with_verification(&["false".to_string()]);

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
        let (_guard, _home) = isolated_home();
        let handler = test_handler();
        let store = test_store(BeadStatus::Done);
        let (_ws, bead) = bead_in_workspace_with_verification(&["false".to_string()]);

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
        let (_guard, _home) = isolated_home();
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let (_ws, bead) = bead_in_workspace_with_verification(&["false".to_string()]);

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
        let (_guard, _home) = isolated_home();
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let (_ws, bead) = bead_in_workspace_with_verification(&[
            "true".to_string(),
            "false".to_string(),
            "echo should-not-run".to_string(),
        ]);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Released);
    }

    // ── per-workspace gate resolution (needle-da77b68a) ──

    #[tokio::test]
    async fn worker_home_gates_never_judge_a_bead_whose_workspace_declares_none() {
        // Direction 1 of needle-da77b68a (live incident aa-48a6e726): a
        // worker homed in commitgraph carried commitgraph's gate in its
        // startup config and ran `scripts/definition-of-done.sh --fast`
        // against every foreign bead it touched — agent-archivist, whose
        // `.needle.yaml` declares only `bead_cli.backend` and which has no
        // `scripts/` directory — failing five dispatches with exit 127.
        // The handler's Config here IS that home config; the bead's
        // workspace is the foreign one. The dispatch must run ZERO command
        // gates and be judged on its own merits.
        let (_guard, _home) = isolated_home();
        let config = Config {
            gates: vec![GateConfig::Command {
                commands: vec!["scripts/definition-of-done.sh --fast".to_string()],
                stderr_cap_bytes: None,
                run_in: Default::default(),
            }],
            worker: crate::config::WorkerConfig {
                enforce_shipped_work: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let handler = test_handler_with_config(config);

        let workspace = tempfile::TempDir::new().unwrap();
        std::fs::write(
            workspace.path().join(".needle.yaml"),
            "bead_cli:\n  backend: bead-rs\n",
        )
        .unwrap();
        let bead = Bead {
            workspace: workspace.path().to_path_buf(),
            ..test_bead(BeadStatus::InProgress)
        };
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        // Had the home gate run, the missing script would exit 127 and this
        // dispatch would be a Failure/Released.
        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
    }

    #[tokio::test]
    async fn bead_workspace_declared_gates_run_from_that_workspaces_config() {
        // Direction 2 of needle-da77b68a: a worker homed in a workspace that
        // declares no gates (default Config) dispatched onto a bead whose
        // workspace declares its OWN pluggable gates must run exactly those
        // gates, in that workspace. The first command drops a marker in its
        // cwd — the bead workspace — and the second fails, so the test
        // observes both that the gate ran and where.
        let (_guard, _home) = isolated_home();
        let handler = test_handler();

        let workspace = tempfile::TempDir::new().unwrap();
        // `run_in: workspace` because the fixture is a bare directory, not a
        // git repo — the default clean mode would fail at `git archive`
        // extraction before running any command, which would make this test
        // pass for the wrong reason (and the marker assert would still
        // catch it, but the failure would say nothing about resolution).
        std::fs::write(
            workspace.path().join(".needle.yaml"),
            "gates:\n\
             \x20 - type: command\n\
             \x20   run_in: workspace\n\
             \x20   commands:\n\
             \x20     - touch gate-ran-in-this-workspace\n\
             \x20     - exit 42\n",
        )
        .unwrap();
        let bead = Bead {
            workspace: workspace.path().to_path_buf(),
            ..test_bead(BeadStatus::InProgress)
        };
        let store = test_store(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failure);
        assert_eq!(result.bead_action, BeadAction::Released);
        let actions = store.actions();
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "verification-failed")
            ),
            "the workspace's own gate must judge this dispatch, got {actions:?}"
        );
        assert!(
            workspace.path().join("gate-ran-in-this-workspace").exists(),
            "the gate must execute with the bead's workspace as its cwd"
        );
    }

    #[tokio::test]
    async fn bead_workspace_gate_paths_are_validated_at_resolution_time_but_still_run() {
        // Resolution-time counterpart of the boot-time gate path check: the
        // boot check can only ever see the worker's home declaration, so a
        // foreign workspace's gate naming a script that workspace does not
        // have is validated here, where the dispatch actually resolves it —
        // the missing path is named in a warning before the gate runs,
        // instead of surfacing only as the gate's own exit 127 (the
        // incident's only symptom). Resolution warns and runs; it does not
        // fail the dispatch on the path check alone — the verdict still
        // belongs to the gate's execution.
        let (_guard, _home) = isolated_home();
        let handler = test_handler();

        let workspace = tempfile::TempDir::new().unwrap();
        // `run_in: workspace` so the failure is the missing script itself and
        // not a clean-mode `git archive` extraction on a bare directory.
        std::fs::write(
            workspace.path().join(".needle.yaml"),
            "gates:\n\
             \x20 - type: command\n\
             \x20   run_in: workspace\n\
             \x20   commands:\n\
             \x20     - scripts/definition-of-done.sh --fast\n",
        )
        .unwrap();
        let bead = Bead {
            workspace: workspace.path().to_path_buf(),
            ..test_bead(BeadStatus::InProgress)
        };

        let (all_passed, report) = handler.run_verification_gates(&bead).await.unwrap();

        // Resolution completed and handed the verdict to the gate: the
        // missing script failed in execution, exactly as it does today.
        assert!(!all_passed);
        let report = report.expect("the resolved gate must still run after a path warning");
        assert!(!report.all_passed);
        assert!(report.results.values().any(|result| !result.passed()));
    }

    #[tokio::test]
    async fn unset_or_relative_bead_workspace_resolves_no_gates() {
        // Follow-up to the per-workspace resolution above: gate resolution
        // joins `.needle.yaml` onto the bead's workspace path, so an unset
        // ("") or relative (".") workspace reads that file from the process
        // CWD — during `cargo test` this repo's own config, whose clean-mode
        // gates (definition-of-done plus `cargo test --lib`) then ran inside
        // the unit test and crawled CI's lib lane to its timeout via
        // full_cycle_with_echo_agent (2026-09-11). A bead without an
        // absolute workspace never named a workspace config: zero gates, no
        // matter what the worker's home config or the CWD declares.
        let (_guard, _home) = isolated_home();
        let config = Config {
            gates: vec![GateConfig::Command {
                commands: vec!["exit 7".to_string()],
                stderr_cap_bytes: None,
                run_in: Default::default(),
            }],
            worker: crate::config::WorkerConfig {
                enforce_shipped_work: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let handler = test_handler_with_config(config);

        for workspace in [
            PathBuf::new(),
            PathBuf::from("."),
            PathBuf::from("some/relative/path"),
        ] {
            let bead = Bead {
                workspace,
                ..test_bead(BeadStatus::InProgress)
            };
            let store = test_store(BeadStatus::Done);

            let result = handler
                .handle(&store, &bead, &test_output(0), false)
                .await
                .unwrap();

            // Had a gate resolved, the configured `exit 7` (or, for "" and
            // ".", this repo's own clean-mode gates) would fail the dispatch.
            assert_eq!(
                result.outcome,
                Outcome::Success,
                "a bead without an absolute workspace must run no gates"
            );
            assert_eq!(result.bead_action, BeadAction::Closed);
        }
    }

    #[tokio::test]
    async fn absolute_workspace_without_config_file_resolves_no_gates() {
        // The remaining resolution branch: an absolute bead workspace whose
        // directory exists but carries no `.needle.yaml` at all. Unlike the
        // unset/relative guard above (an early return before any config
        // read) and direction 1 (a config file that declares no gates), this
        // reaches `load_workspace`, gets Ok(None), and must fall through the
        // same "no gates" default — a bead in a workspace that says nothing
        // never inherits the worker's home gates (needle-da77b68a).
        let (_guard, _home) = isolated_home();
        let config = Config {
            gates: vec![GateConfig::Command {
                commands: vec!["exit 7".to_string()],
                stderr_cap_bytes: None,
                run_in: Default::default(),
            }],
            worker: crate::config::WorkerConfig {
                enforce_shipped_work: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let handler = test_handler_with_config(config);

        let workspace = tempfile::TempDir::new().unwrap();
        let bead = Bead {
            workspace: workspace.path().to_path_buf(),
            ..test_bead(BeadStatus::InProgress)
        };
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(
            result.outcome,
            Outcome::Success,
            "a workspace with no .needle.yaml declares no gates — run none"
        );
        assert_eq!(result.bead_action, BeadAction::Closed);
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

    /// A store whose `show()` genuinely sleeps (a real `.await` yield point,
    /// unlike a blocking `std::process::Command` gate) so the configured
    /// outcome-handler timeout deterministically wins the race. Shared by the
    /// timeout-enforcement test and the ledger-row-on-timeout test.
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
        async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
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
        async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()> {
            self.inner.remove_dependency(blocked_id, blocker_id).await
        }
        async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
            self.inner.clear_assignee(id).await
        }
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

        // The 3s gate is declared by the bead's own workspace, where gate
        // resolution reads it (needle-da77b68a); the handler config only
        // carries the outcome timeout.
        let (_ws, bead) = bead_in_workspace_with_verification(&[format!(
            "sleep 3 && touch {}",
            marker_path.display()
        )]);
        let config = Config {
            validation: ValidationConfig {
                outcome_timeout_seconds: 1,
                ..Default::default()
            },
            ..Config::default()
        };
        let handler = test_handler_with_config(config);
        let store = test_store(BeadStatus::InProgress);
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
        //
        // HOME is pinned because handle_gate_error records the error in the
        // gate-health state under $HOME/.needle/state — without this the test
        // wrote `degraded: true` into the real fleet's state file for whatever
        // path test_bead() carried (needle-50c60e46).
        let (_guard, _home) = isolated_home();
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

    // ── attempt.resolved ledger row (N-T16) ──

    /// Run one dispatch cycle to a terminal outcome and return its ledger rows.
    ///
    /// Each call builds a fresh handler and memory sink: the row is emitted
    /// once per dispatch, so a shared sink would count one emission across
    /// cases instead of per case. Shipped-work enforcement is off because
    /// none of these fixtures set up a predispatch snapshot, and HOME is
    /// pinned because the gate paths read and write gate-health state.
    async fn ledger_rows_for(
        exit_code: i32,
        interrupted: bool,
        verification: Vec<String>,
    ) -> Vec<crate::telemetry::TelemetryEvent> {
        let (_guard, _home) = isolated_home();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let helper = crate::telemetry::test_utils::TestHelper::new("ledger-row-test");
        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        let store = test_store(BeadStatus::InProgress);
        // Any gate under test is declared by the bead's own workspace, where
        // gate resolution reads it (needle-da77b68a).
        let (_ws, bead) = bead_in_workspace_with_verification(&verification);

        let _ = handler
            .handle(&store, &bead, &test_output(exit_code), interrupted)
            .await
            .unwrap();
        helper.sync().await;
        let rows = helper.events_by_type("attempt.resolved");
        for row in &rows {
            assert_row_satisfies_v1_contract(
                &format!("terminal path (exit {exit_code}, interrupted {interrupted})"),
                row,
            );
        }
        rows
    }

    /// Every emitted row satisfies the versioned v1 fixture, and carries the
    /// two pins this audit stands guard on: `provisional: true` until N-T03
    /// resolves attempt identity, and no `context_manifest_hash` until N-T10
    /// ships manifest hashing. The hash pin is *absence*, not null — the
    /// fixture types the field as a plain string, so a null would fail the
    /// conformance check above (and every downstream validator with it).
    fn assert_row_satisfies_v1_contract(label: &str, row: &crate::telemetry::TelemetryEvent) {
        crate::telemetry::test_utils::check_object_matches(
            label,
            &row.data,
            &crate::telemetry::test_utils::fixture_spec(),
        )
        .unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(row.data["schema_version"], 1, "{label}: schema_version");
        assert_eq!(
            row.data["provisional"], true,
            "{label}: every row is provisional until N-T03"
        );
        assert!(
            row.data.get("context_manifest_hash").is_none(),
            "{label}: context_manifest_hash must stay absent until N-T10, got {}",
            row.data["context_manifest_hash"]
        );
    }

    /// The terminal handlers `handle` routes to must each produce one —
    /// and only one — `attempt.resolved` ledger row.
    #[tokio::test]
    async fn attempt_resolved_emitted_exactly_once_on_every_terminal_path() {
        // (label, exit code, interrupted, verification commands). The exit
        // codes and the gate configuration each select a different
        // sub-handler: success and gate_failure (gate ran and rejected)
        // arrive via exit 0.
        //
        // The gate_error sub-handler has no entry in this loop because it is
        // not reachable through the exit-code classifications the loop
        // drives: an ExecutionError in a CommandGate report is flattened to
        // a plain Fail by to_gate_result before the routing match ever sees
        // it (whether sh exit 127 should even count as an execution error —
        // the aa-48a6e726 incident's shape — is needle-3771863e's scope).
        // The route that DOES reach it through handle() is handle_success's
        // shipped-work re-route, pinned by
        // attempt_resolved_emitted_exactly_once_on_the_gate_error_path; its
        // sibling re-route (the shipped-work gate ran and rejected the
        // closure) is pinned by
        // attempt_resolved_emitted_exactly_once_when_success_reroutes_to_gate_failure.
        // The cancelled-before-start, handler-timeout and handler-error
        // wrapper paths each have their own test below.
        let paths: Vec<(&str, i32, bool, Vec<String>)> = vec![
            ("success", 0, false, vec![]),
            ("gate_failure", 0, false, vec!["false".to_string()]),
            ("failure", 1, false, vec![]),
            ("timeout", 124, false, vec![]),
            ("agent_not_found", 127, false, vec![]),
            ("crash", -9, false, vec![]),
            ("interrupted", 0, true, vec![]),
        ];

        for (label, exit_code, interrupted, verification) in paths {
            let rows = ledger_rows_for(exit_code, interrupted, verification).await;
            assert_eq!(
                rows.len(),
                1,
                "terminal path {label} must emit exactly one attempt.resolved, got {rows:?}"
            );
        }
    }

    /// The eighth terminal path: a dispatch whose work was never judged
    /// because the shipped-work gate could not run (no upstream configured —
    /// GitHub issue #18) reaches `handle_gate_error` through `handle_success`'s
    /// re-route, and still resolves to exactly one ledger row. This is the
    /// only route through `handle()` that reaches `handle_gate_error` today:
    /// the `Outcome::Failure` arm's report scan never sees an ExecutionError,
    /// because `CommandGate::validate` flattens the error kind into a plain
    /// Fail before the routing match (needle-3771863e owns preserving it).
    #[tokio::test]
    async fn attempt_resolved_emitted_exactly_once_on_the_gate_error_path() {
        let (_env, _home) = isolated_home();

        // A real repo with a substantial new commit and NO upstream: exactly
        // the shape the shipped-work gate cannot judge.
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
        let helper = crate::telemetry::test_utils::TestHelper::new("ledger-row-test");
        let attempt_id = uuid::Uuid::now_v7().to_string();
        helper.telemetry().set_attempt_id(attempt_id.clone());
        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        handler.set_attempt_context(AttemptContext::default());
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        // Route identity: handle_success re-routed into handle_gate_error —
        // released without burning the retry counter, gate.execution_error
        // emitted — not into handle_gate_failure.
        assert_eq!(result.bead_action, BeadAction::Released);
        helper.sync().await;
        assert!(
            !helper.events_by_type("gate.execution_error").is_empty(),
            "the dispatch must have taken the gate_error route"
        );

        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(
            rows.len(),
            1,
            "the gate_error path must emit exactly one attempt.resolved, got {rows:?}"
        );
        assert_eq!(rows[0].data["attempt_id"], attempt_id);
        assert_row_satisfies_v1_contract("gate_error path", &rows[0]);
    }

    /// The re-route the single emission point exists for: `handle_success`
    /// classifies the dispatch as verified (exit 0, no configured gate
    /// objected) and only then discovers the shipped-work gate rejects the
    /// closure, re-routing into `handle_gate_failure` from inside the success
    /// arm. The row is emitted once, after the routing match — this pins that
    /// a sub-handler reached through a re-route fires it exactly once, never
    /// twice.
    #[tokio::test]
    async fn attempt_resolved_emitted_exactly_once_when_success_reroutes_to_gate_failure() {
        let (_env, _home) = isolated_home();

        // A real repo WITH an upstream, carrying a substantial unpushed
        // commit: the shipped-work gate can run here, and it rejects.
        let repo = tempfile::TempDir::new().unwrap();
        let remote = tempfile::TempDir::new().unwrap();
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
        run(&["init", "-q", "--bare", remote.path().to_str().unwrap()]);
        run(&["remote", "add", "origin", remote.path().to_str().unwrap()]);
        run(&["push", "-q", "-u", "origin", "HEAD"]);
        std::fs::write(repo.path().join("src.rs"), "fn main() {}\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "unpushed work"]);

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
        let helper = crate::telemetry::test_utils::TestHelper::new("ledger-row-test");
        let attempt_id = uuid::Uuid::now_v7().to_string();
        helper.telemetry().set_attempt_id(attempt_id.clone());
        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        handler.set_attempt_context(AttemptContext::default());
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        // Route identity: handle_gate_failure released AND burned the retry
        // counter — the mirror image of the gate_error route above.
        assert_eq!(result.bead_action, BeadAction::Released);
        assert!(
            store.actions().iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "failure-count:1")
            ),
            "a shipped-work gate that ran and rejected must increment the failure count"
        );
        helper.sync().await;
        assert!(
            helper.events_by_type("gate.execution_error").is_empty(),
            "a gate that ran and rejected is a gate failure, not an execution error"
        );

        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(
            rows.len(),
            1,
            "the re-routed gate_failure must emit exactly one attempt.resolved, got {rows:?}"
        );
        assert_eq!(rows[0].data["attempt_id"], attempt_id);
        assert_row_satisfies_v1_contract("success→gate_failure re-route", &rows[0]);
    }

    /// Exit 0 means nothing on its own: when verification ran and rejected
    /// the work, the ledger must say `work_failure`, never `verified_success`.
    #[tokio::test]
    async fn attempt_resolved_exit_zero_with_failed_verification_is_work_failure() {
        let rows = ledger_rows_for(0, false, vec!["false".to_string()]).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].data["outcome"], "work_failure",
            "a rejected gate must classify as work_failure"
        );
        assert_ne!(
            rows[0].data["outcome"], "verified_success",
            "exit 0 with failed verification must never read verified_success"
        );
        // The rejecting gate is named in the row so the failure is
        // attributable without re-running it.
        let gates = rows[0].data["gate_results"]
            .as_array()
            .expect("gate_results array");
        assert!(!gates.is_empty(), "a gate ran; the row must carry it");
    }

    /// Every row is provisional and carries the dispatch's attempt ID until
    /// N-T03 resolves attempts against beads — no consumer may treat it as
    /// authoritative.
    #[tokio::test]
    async fn attempt_resolved_is_provisional_and_stamped_with_the_dispatch_attempt_id() {
        let (_guard, _home) = isolated_home();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let helper = crate::telemetry::test_utils::TestHelper::new("ledger-row-test");
        // The worker sets this at dispatch start (worker/mod.rs); the handler
        // reads it back rather than minting its own.
        let attempt_id = uuid::Uuid::now_v7().to_string();
        helper.telemetry().set_attempt_id(attempt_id.clone());

        let handler = OutcomeHandler::new(config.clone(), helper.telemetry().clone());
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);
        let _ = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();
        helper.sync().await;

        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].data["provisional"], true);
        assert_eq!(rows[0].data["attempt_id"], attempt_id);
        // The envelope carries it too, so every event of the cycle — not just
        // the ledger row — joins on the attempt.
        assert_eq!(rows[0].attempt_id.as_deref(), Some(attempt_id.as_str()));
        assert_row_satisfies_v1_contract("dispatch-cycle row", &rows[0]);

        // A handler used without a dispatch cycle still emits an identified
        // row rather than one that cannot be joined to anything.
        let handler_solo = OutcomeHandler::new(config.clone(), helper.telemetry().clone());
        helper.telemetry().clear_attempt_id();
        let store2 = test_store(BeadStatus::InProgress);
        let _ = handler_solo
            .handle(&store2, &bead, &test_output(1), false)
            .await
            .unwrap();
        helper.sync().await;
        let solo = helper.events_by_type("attempt.resolved");
        assert_eq!(solo.len(), 2);
        assert_eq!(solo[1].data["provisional"], true);
        let minted = solo[1].data["attempt_id"].as_str().expect("attempt_id");
        assert_ne!(minted, attempt_id, "no cycle: the row mints its own id");
        assert_eq!(solo[1].attempt_id.as_deref(), Some(minted));
        assert_row_satisfies_v1_contract("solo-handler row", &solo[1]);
    }

    /// A dispatch cancelled before `handle` runs is still terminal, so it
    /// still resolves to exactly one ledger row.
    #[tokio::test]
    async fn attempt_resolved_row_exists_when_handling_is_cancelled_before_it_starts() {
        let (_guard, _home) = isolated_home();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let helper = crate::telemetry::test_utils::TestHelper::new("ledger-row-test");
        let attempt_id = uuid::Uuid::now_v7().to_string();
        helper.telemetry().set_attempt_id(attempt_id.clone());

        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        // The worker records the dispatch context before entering HANDLING.
        handler.set_attempt_context(AttemptContext::default());

        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);
        let cancelled = Arc::new(AtomicBool::new(true));
        let _ = handler
            .handle_with_cancellation(&store, &bead, &test_output(0), false, cancelled)
            .await
            .unwrap();
        helper.sync().await;

        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(rows.len(), 1, "a cancelled dispatch still gets one row");
        assert_eq!(rows[0].data["outcome"], "cancelled");
        assert_eq!(
            rows[0].data["terminal_reason"], "cancelled_before_handling",
            "the row must say why no verdict was reached"
        );
        assert_eq!(rows[0].data["attempt_id"], attempt_id);
        assert_eq!(rows[0].data["gate_results"], serde_json::json!([]));
        assert_row_satisfies_v1_contract("cancelled dispatch", &rows[0]);
    }

    /// A handler that is torn down by its own timeout must not leave the
    /// dispatch without a ledger row — and if the aborted `handle` had already
    /// emitted one, the fallback must not add a second.
    #[tokio::test]
    async fn attempt_resolved_row_exists_when_the_handler_times_out() {
        let (_guard, _home) = isolated_home();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        // `handle_success` goes straight to `store.show()`, which sleeps 2s —
        // the 1s handler timeout fires and drops the `handle` future.
        config.validation.outcome_timeout_seconds = 1;
        let helper = crate::telemetry::test_utils::TestHelper::new("ledger-row-test");
        let attempt_id = uuid::Uuid::now_v7().to_string();
        helper.telemetry().set_attempt_id(attempt_id.clone());

        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        handler.set_attempt_context(AttemptContext::default());

        let store = SlowShowStore {
            inner: MockBeadStore::new(BeadStatus::Done),
        };
        let bead = test_bead(BeadStatus::InProgress);
        let cancelled = Arc::new(AtomicBool::new(false));
        let _ = handler
            .handle_with_cancellation(&store, &bead, &test_output(0), false, cancelled)
            .await
            .unwrap();
        helper.sync().await;

        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(rows.len(), 1, "a timed-out dispatch still gets one row");
        assert_eq!(
            rows[0].data["outcome"], "indeterminate",
            "no verdict was reached: the attempt is indeterminate, not a work failure"
        );
        assert_eq!(rows[0].data["terminal_reason"], "outcome_handler_timeout");
        assert_eq!(rows[0].data["attempt_id"], attempt_id);
        assert_row_satisfies_v1_contract("handler timeout", &rows[0]);
    }

    /// A sub-handler that errors — the post-completion checkpoint flush
    /// failing inside `handle_success` (completion is unverified, so the
    /// handler refuses to report success) — must still resolve the dispatch
    /// to exactly one ledger row: `handle` died before its own emission, so
    /// the `Ok(Err(_))` arm of `handle_with_cancellation` emits it. This is
    /// the third fallback, next to cancelled-before-handling and handler
    /// timeout.
    #[tokio::test]
    async fn attempt_resolved_row_exists_when_the_handler_errors() {
        let (_guard, _home) = isolated_home();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let helper = crate::telemetry::test_utils::TestHelper::new("ledger-row-test");
        let attempt_id = uuid::Uuid::now_v7().to_string();
        helper.telemetry().set_attempt_id(attempt_id.clone());

        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        // The worker records the dispatch context before entering HANDLING.
        handler.set_attempt_context(AttemptContext::default());

        // Done, so the success flow reaches its final flush — which fails.
        let mut store = test_store(BeadStatus::Done);
        store.fail_flush = true;
        let bead = test_bead(BeadStatus::InProgress);
        let cancelled = Arc::new(AtomicBool::new(false));
        let result = handler
            .handle_with_cancellation(&store, &bead, &test_output(0), false, cancelled)
            .await;
        assert!(
            result.is_err(),
            "the failing flush must surface as a handler error"
        );
        helper.sync().await;

        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(
            rows.len(),
            1,
            "an errored dispatch still gets exactly one row"
        );
        assert_eq!(
            rows[0].data["outcome"], "infrastructure_failure",
            "nothing judged the work: the attempt died inside the handler"
        );
        assert_eq!(rows[0].data["terminal_reason"], "outcome_handler_error");
        assert_eq!(
            rows[0].data["requested_action"], "Errored",
            "the fallback requests the release-recovery action"
        );
        assert_eq!(rows[0].data["attempt_id"], attempt_id);
        assert_row_satisfies_v1_contract("handler error", &rows[0]);
    }

    /// The emit-failure fallthrough: when the ledger row's own emission is
    /// lost (the telemetry writer died mid-dispatch), the dispatch still
    /// completes, and a wrapper fallback firing afterwards must NOT
    /// compensate with a second row. The guard is recorded before the
    /// emission, so a dispatch that resolved — even to a row that was then
    /// lost — can never emit twice. Compensation would be worse than the
    /// loss: the fallback's `infrastructure_failure` verdict would misreport
    /// a dispatch whose work WAS judged. Also pins the guard's other half on
    /// a live transport (no fallback emission after a resolved dispatch) and
    /// that the next dispatch emits normally — a lost row must not wedge the
    /// ledger.
    #[tokio::test]
    async fn attempt_resolved_fallthrough_keeps_exactly_once_when_the_emit_fails() {
        let (_guard, _home) = isolated_home();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let helper = crate::telemetry::test_utils::TestHelper::new("ledger-row-test");
        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        handler.set_attempt_context(AttemptContext::default());
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        // Dispatch A: the transport dies before the row is written. The
        // handler itself must not fail — losing the row surfaces in the
        // telemetry layer, not as a dispatch error.
        let result = {
            let _dead = helper.telemetry().disconnected_transport_for_testing();
            handler
                .handle(&store, &bead, &test_output(1), false)
                .await
                .unwrap()
        };
        assert_eq!(result.bead_action, BeadAction::Released);
        helper.sync().await;
        assert_eq!(
            helper.events_by_type("attempt.resolved").len(),
            0,
            "the transport was down: dispatch A's row was lost"
        );

        // The wrapper fallback for dispatch A fires anyway (the handler-
        // timeout race can land in exactly this state: the primary emission
        // ran, the fallback cannot know whether its row survived). It must
        // stay silent — the dispatch already resolved.
        handler.emit_unresolved_terminal_row(
            &bead,
            &test_output(1),
            "Errored",
            "infrastructure_failure",
            "outcome_handler_error",
        );
        helper.sync().await;
        assert_eq!(
            helper.events_by_type("attempt.resolved").len(),
            0,
            "a dispatch that resolved must never emit a second row, not even \
             after its first row was lost"
        );

        // Dispatch B, transport restored: the ledger emits normally again.
        let result = handler
            .handle(&store, &bead, &test_output(1), false)
            .await
            .unwrap();
        assert_eq!(result.bead_action, BeadAction::Released);
        helper.sync().await;
        assert_eq!(
            helper.events_by_type("attempt.resolved").len(),
            1,
            "the next dispatch after a lost row emits exactly one row"
        );

        // And the fallback stays silent for a dispatch whose row is alive —
        // the guard-after-success half of the same pin.
        handler.emit_unresolved_terminal_row(
            &bead,
            &test_output(1),
            "Errored",
            "infrastructure_failure",
            "outcome_handler_error",
        );
        helper.sync().await;
        assert_eq!(
            helper.events_by_type("attempt.resolved").len(),
            1,
            "no fallback emission may follow a dispatch that resolved to a live row"
        );
    }

    /// The exactly-once guard is per dispatch, not per handler. The worker
    /// reuses one `OutcomeHandler` across every cycle it serves, and records
    /// the next dispatch's context (`set_attempt_context`) once the previous
    /// cycle is long gone — so a guard left set by a resolved dispatch must
    /// not silence the wrapper fallbacks of the dispatches after it. Before
    /// the guard reset lived in `set_attempt_context`, the first resolved
    /// dispatch permanently disarmed all three wrapper paths: every later
    /// cancelled-before-start, handler-error or handler-timeout dispatch in
    /// that worker process ended with NO ledger row at all. The same pin in
    /// reverse — a wrapper-path dispatch must not double when its own row
    /// already fired — is held by the fallthrough test above.
    #[tokio::test]
    async fn a_resolved_dispatch_does_not_silence_the_next_dispatchs_wrapper_paths() {
        let (_guard, _home) = isolated_home();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        // Long enough that the first three cycles never race it, short enough
        // that the fourth cycle's 2s `show()` triggers the handler timeout.
        config.validation.outcome_timeout_seconds = 1;
        let helper = crate::telemetry::test_utils::TestHelper::new("ledger-row-test");
        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        let bead = test_bead(BeadStatus::InProgress);
        let not_cancelled = Arc::new(AtomicBool::new(false));

        // Cycle 1 resolves normally: a work failure releases the bead and
        // emits the row — and sets the guard this test exists to exonerate.
        handler.set_attempt_context(AttemptContext::default());
        let store = test_store(BeadStatus::InProgress);
        let result = handler
            .handle(&store, &bead, &test_output(1), false)
            .await
            .unwrap();
        assert_eq!(result.bead_action, BeadAction::Released);
        helper.sync().await;
        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(rows.len(), 1, "cycle 1 resolved to one row, got {rows:?}");
        assert_eq!(rows[0].data["outcome"], "work_failure");

        // Cycle 2 is cancelled before its handler starts. It is still
        // terminal: it must get its own row despite cycle 1 having resolved.
        handler.set_attempt_context(AttemptContext::default());
        let cancelled = Arc::new(AtomicBool::new(true));
        let _ = handler
            .handle_with_cancellation(
                &test_store(BeadStatus::InProgress),
                &bead,
                &test_output(0),
                false,
                cancelled,
            )
            .await
            .unwrap();
        helper.sync().await;
        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(
            rows.len(),
            2,
            "a cancelled dispatch after a resolved one still gets its own row, got {rows:?}"
        );
        assert_eq!(rows[1].data["outcome"], "cancelled");
        assert_row_satisfies_v1_contract("cancelled after resolved", &rows[1]);

        // Cycle 3 dies inside its handler (the completion flush fails). Its
        // `handle` never reached its own emission, so the wrapper fallback
        // must fire — the stale guard must not eat it.
        handler.set_attempt_context(AttemptContext::default());
        let mut store = test_store(BeadStatus::Done);
        store.fail_flush = true;
        let result = handler
            .handle_with_cancellation(&store, &bead, &test_output(0), false, not_cancelled.clone())
            .await;
        assert!(
            result.is_err(),
            "the failing flush must surface as an error"
        );
        helper.sync().await;
        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(
            rows.len(),
            3,
            "an errored dispatch after a resolved one still gets its own row, got {rows:?}"
        );
        assert_eq!(rows[2].data["outcome"], "infrastructure_failure");
        assert_eq!(rows[2].data["terminal_reason"], "outcome_handler_error");
        assert_row_satisfies_v1_contract("errored after resolved", &rows[2]);

        // Cycle 4 is torn down by the handler timeout. The aborted `handle`
        // never emitted, so the timeout fallback must fire for it.
        handler.set_attempt_context(AttemptContext::default());
        let slow = SlowShowStore {
            inner: MockBeadStore::new(BeadStatus::Done),
        };
        let _ = handler
            .handle_with_cancellation(&slow, &bead, &test_output(0), false, not_cancelled)
            .await
            .unwrap();
        helper.sync().await;
        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(
            rows.len(),
            4,
            "a timed-out dispatch after a resolved one still gets its own row, got {rows:?}"
        );
        assert_eq!(rows[3].data["outcome"], "indeterminate");
        assert_eq!(rows[3].data["terminal_reason"], "outcome_handler_timeout");
        assert_row_satisfies_v1_contract("timed out after resolved", &rows[3]);
    }
}
