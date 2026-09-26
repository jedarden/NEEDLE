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
use crate::telemetry::{EventKind, FalseCloseClass, Telemetry};
use crate::types::{
    AgentOutcome, Bead, BeadAction, BeadId, BeadStatus, HandlerResult, Outcome, ReleaseReason,
};
use crate::validation::{
    dod_bypass, predispatch, verify_shipped_work, GateConfig, GateReport, GateResult,
    ValidationGate,
};

pub(crate) mod close_verification;
pub(crate) mod fallback_verification;

/// Fleet-wide cooling period after an unsuccessful attempt.  The window grows
/// across consecutive failures so another ready bead can run instead of every
/// worker immediately reclaiming the same deterministic frontier entry.
const RETRY_COOLDOWN_BASE_SECS: u64 = 5 * 60;
const RETRY_COOLDOWN_MAX_SECS: u64 = 30 * 60;

/// Provenance captured when validation gates are resolved for one bead.
///
/// The summary travels with every gate state transition so telemetry can
/// distinguish a gate run resolved from the bead's workspace from a dispatch
/// where no command gates were resolved at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GateResolutionTelemetry {
    pub(crate) gates_source: &'static str,
    pub(crate) command_gates_resolved: u32,
}

impl GateResolutionTelemetry {
    fn none() -> Self {
        Self {
            gates_source: "none",
            command_gates_resolved: 0,
        }
    }

    fn bead_workspace(command_gates_resolved: usize) -> Self {
        if command_gates_resolved == 0 {
            return Self::none();
        }
        Self {
            gates_source: "bead_workspace",
            command_gates_resolved: command_gates_resolved as u32,
        }
    }
}

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

/// Classify the validation failure that caused a closed bead to be reopened.
/// The gate name is more reliable than free-form output for the built-in
/// clean-extraction paths; command text fills in the named-test case for
/// explicit close evidence and configured gates.
fn false_close_class(gate: &str, reason: &str) -> FalseCloseClass {
    let reason = reason.to_ascii_lowercase();
    if gate == fallback_verification::CLEAN_TREE_GATE_NAME
        || reason.contains("uncommitted")
        || reason.contains("dirty tree")
    {
        return FalseCloseClass::UncommittedDependency;
    }
    if gate == close_verification::GATE_NAME
        && (reason.contains("no verification evidence")
            || reason.contains("no close reason")
            || reason.contains("without evidence"))
    {
        return FalseCloseClass::NoEvidence;
    }
    if gate == "shipped_work"
        || reason.contains("deliverable")
        || reason.contains("not done")
        || reason.contains("blocked")
    {
        return FalseCloseClass::DeliverableBlocked;
    }
    if gate == close_verification::GATE_NAME
        && ["cargo test", "go test", "npm test", "pytest", "make test"]
            .iter()
            .any(|command| reason.contains(command))
    {
        return FalseCloseClass::NamedTestRed;
    }
    if gate.starts_with("fallback_") {
        return FalseCloseClass::NeverCompiled;
    }
    if reason.contains("test") {
        return FalseCloseClass::NamedTestRed;
    }
    FalseCloseClass::NeverCompiled
}

// ──────────────────────────────────────────────────────────────────────────────
// classify (convenience re-export)
// ──────────────────────────────────────────────────────────────────────────────

/// Classify an agent result into an `Outcome`, with verification and shutdown
/// signal support.
///
/// Interruption takes precedence. An abnormal negative exit is retained as an
/// infrastructure outcome even without a verification verdict; otherwise,
/// failed verification is a failure and verified results are delegated to the
/// exit-code classifier.
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
    // A negative exit code is the dispatcher's sentinel for a process that
    // never produced a normal exit status (including the -1/0ms spawn-error
    // shape). It is infrastructure evidence, not a verification failure.
    // Classify it before consulting `verified`, otherwise a missing agent
    // process can be turned into `Outcome::Failure`, incrementing the bead's
    // failure counter and feeding mitosis with evidence that the task is too
    // large. A genuine fast task failure still has a normal exit code and
    // follows the verification path below.
    if exit_code < 0 {
        return Outcome::classify(exit_code, false);
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

/// What the adapter-health detector concluded about one failure (N-T23).
#[derive(Debug, Clone, PartialEq, Eq)]
enum AdapterJudgement {
    /// The failure carries the fingerprint the adapter is degraded for (or
    /// just tripped it): the provider's outage, released without penalty.
    Infrastructure { fingerprint: String },
    /// Recorded in the window; judged as the bead's own failure.
    Judged,
}

/// The adapter-level failure signal in an agent's output, if it has one.
///
/// A stream envelope that reports an API error names the error and its
/// status; an exit code names the process-level failure signal. Non-zero exits
/// remain attributable to the bead until the adapter-level detector observes
/// the same signal across enough distinct beads to prove a shared dispatch
/// problem. This is important for adapters whose transport reports a generic
/// exit 1 for both task failures and provider/CLI failures.
fn adapter_failure_signal(output: &AgentOutcome) -> Option<String> {
    if let Some(envelope) = crate::trace::parse_result_envelope(&output.stdout) {
        if envelope.indicates_failure() {
            return Some(format!(
                "terminal_reason={} api_error_status={}",
                envelope.terminal_reason.as_deref().unwrap_or("error"),
                envelope
                    .api_error_status
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "none".to_string())
            ));
        }
    }
    (output.exit_code != 0).then(|| format!("exit_code:{}", output.exit_code))
}

/// The adapter-health failure signal for one outcome (N-T23), or `None` when
/// the outcome must not be recorded against the provider's health window.
///
/// A crash death by SIGTERM (exit 143) is excluded: SIGTERM is sent by the
/// fleet's own machinery — a freshness re-exec, a supervisor drain, the
/// orphan reaper, an operator — never by a provider, and a synchronized
/// burst of them (as when an upgrade storm replaces every worker at once)
/// must not fingerprint as `provider.degraded` (needle-f1efbb0a). The kill
/// stays visible everywhere else: the attempt ledger still records
/// `infrastructure_failure` with `signal:15`.
fn adapter_health_failure_reason(
    outcome: &Outcome,
    output: &AgentOutcome,
    gate_report: Option<&GateReport>,
) -> Option<String> {
    match outcome {
        Outcome::Failure => {
            if gate_report.is_some_and(|r| !r.all_passed) {
                return None;
            }
            adapter_failure_signal(output)
        }
        Outcome::Crash(code) => {
            let signal = if *code > 128 { code - 128 } else { *code };
            if signal == 15 {
                return None;
            }
            Some(format!("signal:{signal}"))
        }
        Outcome::AgentNotFound => Some(format!("agent_not_found exit_code:{}", output.exit_code)),
        Outcome::Success
        | Outcome::Timeout
        | Outcome::Interrupted
        | Outcome::GateError
        | Outcome::GateUnsatisfiable => None,
    }
}

/// The bounded failure text recorded against the attempt for the next
/// attempt to read (R3). `None` on a verified success.
///
/// A rejecting gate contributes its name and its captured output; a plain
/// process failure contributes the stream's terminal envelope (an API error
/// that exited 0 is not a hard task) and the tail of stderr. Everything is
/// cut to [`crate::attempt_history::LOCAL_SUMMARY_BYTES`] head-and-tail so
/// both the leading diagnostic and the closing summary survive.
fn attempt_failure_summary(
    outcome: &Outcome,
    output: &AgentOutcome,
    gate_report: Option<&GateReport>,
) -> Option<String> {
    use crate::attempt_history::{truncate_head_tail, LOCAL_SUMMARY_BYTES};

    if matches!(outcome, Outcome::Success) {
        return None;
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(report) = gate_report {
        // Stable order: the report is a HashMap.
        let mut results: Vec<(&String, &GateResult)> = report.results.iter().collect();
        results.sort_by(|a, b| a.0.cmp(b.0));
        for (name, result) in results {
            match result {
                GateResult::Fail(reason) => {
                    parts.push(format!("gate `{name}` failed:\n{}", reason.trim_end()));
                }
                GateResult::Unsatisfiable(reason) => {
                    parts.push(format!(
                        "gate `{name}` has an unsatisfiable precondition:\n{}",
                        reason.trim_end()
                    ));
                }
                GateResult::ExecutionError { command, reason } => {
                    parts.push(format!(
                        "gate `{name}` could not run ({reason}) — command: {command}"
                    ));
                }
                GateResult::Pass => {}
            }
        }
    }
    if parts.is_empty() {
        if let Some(envelope) = crate::trace::parse_result_envelope(&output.stdout) {
            if let Some(reason) = envelope.terminal_reason.as_deref() {
                parts.push(format!(
                    "agent stream ended with terminal_reason={reason}{}",
                    envelope
                        .api_error_status
                        .map(|s| format!(" api_error_status={s}"))
                        .unwrap_or_default()
                ));
            }
        }
        let stderr = output.stderr.trim();
        if !stderr.is_empty() {
            let tail: String = stderr
                .lines()
                .rev()
                .take(40)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            parts.push(format!("stderr (tail):\n{tail}"));
        }
        match outcome {
            Outcome::Timeout => parts.push("the agent hit the hard timeout".to_string()),
            Outcome::Crash(code) => parts.push(format!("the agent crashed (code {code})")),
            _ => {}
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(truncate_head_tail(&parts.join("\n\n"), LOCAL_SUMMARY_BYTES))
}

/// Build the sanitizer used for N-T45 evidence. This mirrors the dispatch
/// trace sanitizer's configured custom rules, but is kept here so evidence is
/// protected even when trace capture is disabled.
fn build_failure_evidence_sanitizer(config: &Config) -> Result<crate::sanitize::Sanitizer> {
    let custom_patterns = config
        .strands
        .learning
        .trace_sanitization
        .custom_patterns
        .iter()
        .map(|pattern| crate::sanitize::CustomPattern {
            id: pattern.id.clone(),
            pattern: pattern.pattern.clone(),
            entropy: pattern.entropy,
        })
        .collect::<Vec<_>>();
    crate::sanitize::Sanitizer::new(&custom_patterns)
}

/// Gather the small, observable intervention summary used by N-T50.
///
/// Git is invoked read-only with optional locks disabled. A missing baseline
/// or failed command leaves that part of the summary unknown rather than
/// inventing paths or commit subjects.
async fn candidate_intervention_summary(
    workspace: &std::path::Path,
    baseline: Option<&str>,
    commits: &[String],
    failed: &crate::attempt_history::AttemptRecord,
    succeeding_gates: &[crate::telemetry::GateResultEntry],
) -> crate::learning::InterventionSummary {
    let (mut changed_paths, mut commit_subjects) = match baseline {
        Some(baseline) if !baseline.is_empty() => {
            let changed_paths = candidate_git_lines(workspace, &["diff", "--name-only", baseline])
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|path| !path.starts_with(".beads/") && path != ".needle-predispatch-sha")
                .collect();
            let range = format!("{baseline}..HEAD");
            let commit_subjects =
                candidate_git_lines(workspace, &["log", "--format=%s", "--reverse", &range])
                    .await
                    .unwrap_or_default();
            (changed_paths, commit_subjects)
        }
        _ => (Vec::new(), Vec::new()),
    };
    if baseline.is_none() {
        for commit in commits
            .iter()
            .filter(|commit| !commit.trim().is_empty())
            .take(20)
        {
            if let Some(subject) =
                candidate_git_lines(workspace, &["show", "-s", "--format=%s", commit.as_str()])
                    .await
                    .and_then(|lines| lines.into_iter().next())
            {
                commit_subjects.push(subject);
            }
            if let Some(paths) = candidate_git_lines(
                workspace,
                &[
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    commit.as_str(),
                ],
            )
            .await
            {
                changed_paths.extend(paths);
            }
        }
    }

    let mut failed_gates = failed
        .failure_evidence
        .as_ref()
        .map(|evidence| {
            evidence
                .gate_diagnostics
                .iter()
                .map(|diagnostic| diagnostic.gate_name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if failed_gates.is_empty() {
        if let Some(reason) = failed.terminal_reason.as_deref() {
            if let Some(gates) = reason.strip_prefix("gate:") {
                failed_gates.extend(
                    gates
                        .split(',')
                        .filter(|gate| !gate.trim().is_empty())
                        .map(str::to_string),
                );
            }
        }
    }
    failed_gates.sort();
    failed_gates.dedup();
    let gate_deltas = failed_gates
        .into_iter()
        .map(|gate| {
            let after = succeeding_gates
                .iter()
                .find(|result| result.name == gate)
                .map(|result| result.status.as_str())
                .unwrap_or("pass");
            format!("{gate}: fail -> {after}")
        })
        .collect();

    crate::learning::InterventionSummary {
        changed_paths,
        commit_subjects,
        gate_deltas,
    }
}

/// Run one bounded, read-only git query for candidate evidence.
async fn candidate_git_lines(workspace: &std::path::Path, args: &[&str]) -> Option<Vec<String>> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new("git")
            .args(args)
            .current_dir(workspace)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn is_build_gate_failure(gate: &str, reason: &str) -> bool {
    let gate = gate.to_ascii_lowercase();
    let reason = reason.to_ascii_lowercase();
    gate.contains("cargo check")
        || gate.contains("default_rust")
        || gate.contains("definition_of_done")
        || gate.contains("definition-of-done")
        || reason.contains("cargo check")
        || reason.contains("definition-of-done")
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
                GateResult::Unsatisfiable(_) => "unsatisfiable",
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
    /// The claimant identity the bead is assigned to (the worker's qualified
    /// id), passed to the backend as the resolving actor.
    pub actor: String,
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
    /// Paths that were already dirty when this dispatch began. They may only
    /// enter a recovery capture when the attempt's transcript names them.
    pub predispatch_dirty_paths: Vec<String>,
    /// Wall-clock start of the attempt, used to attribute mtime-only edits.
    pub started_at_wall: Option<chrono::DateTime<Utc>>,
    /// Input tokens reported by the agent's token extractor.
    pub tokens_in: Option<u64>,
    /// Output tokens reported by the agent's token extractor.
    pub tokens_out: Option<u64>,
    /// Estimated cost in USD (None when no pricing is configured).
    pub estimated_cost_usd: Option<f64>,
    /// Whether the cost was established (N-T47): from the result envelope,
    /// or from the stream's per-turn usage when a killed attempt wrote none.
    /// `false` is unknown, never free.
    pub costed: bool,
    /// Durable recovery patch captured by timeout/crash/interruption paths.
    pub wip_patch: Option<crate::wip::WipPatch>,
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
    /// Claim, backend, adapter, and context identity captured for the active
    /// attempt. Kept separate from `AttemptContext` so legacy direct handler
    /// callers remain source-compatible while real worker dispatches carry
    /// the stronger claim-time provenance.
    attempt_provenance: Arc<std::sync::Mutex<Option<crate::attempt::AttemptProvenance>>>,
    /// Re-runs the verification commands a close reason claims, in a clean
    /// extraction of committed state, before the close is honoured
    /// ([`Self::verify_close_evidence`]).
    close_verification: close_verification::CloseVerificationRuntime,
    /// Judges a workspace that declares no gates: selects the verifier its
    /// own files imply and runs it in the clean extraction, or checks the
    /// tree is clean when nothing applies
    /// ([`Self::run_fallback_gate`]).
    fallback_verification: fallback_verification::FallbackVerificationRuntime,
}

/// Close reason recorded when the shipped-work gate confirms an agent's work
/// landed but the agent did not close the bead itself. Operator-visible in the
/// bead's history, so it must say why the fleet closed something on the agent's
/// behalf.
const SHIPPED_WORK_CLOSE_REASON: &str =
    "closed by NEEDLE: shipped-work gate confirmed the work landed but the agent did not close it";

impl OutcomeHandler {
    pub fn new(config: Config, telemetry: Telemetry) -> Self {
        OutcomeHandler {
            config,
            telemetry,
            attempt_context: Arc::new(std::sync::Mutex::new(None)),
            ledger_row_emitted: Arc::new(std::sync::Mutex::new(None)),
            attempt_provenance: Arc::new(std::sync::Mutex::new(None)),
            close_verification: close_verification::CloseVerificationRuntime::production(),
            fallback_verification: fallback_verification::FallbackVerificationRuntime::production(),
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

    /// Bind claim-time provenance to the next ledger row.
    pub fn set_attempt_provenance(&self, provenance: crate::attempt::AttemptProvenance) {
        *self
            .attempt_provenance
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(provenance);
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

    fn set_wip_patch(&self, patch: Option<crate::wip::WipPatch>) {
        if let Some(context) = self
            .attempt_context
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            context.wip_patch = patch;
        }
    }

    /// Preserve only this attempt's working-tree edits before a release. The
    /// helper is best-effort so a Git failure cannot strand the bead in
    /// HANDLING; the attempt ledger still records the ordinary terminal row.
    async fn capture_wip_patch(&self, bead: &Bead, output: &AgentOutcome) {
        let Some(attempt_id) = self.attempt_id() else {
            tracing::warn!(
                bead_id = %bead.id,
                "cannot capture WIP patch without an attempt ID"
            );
            return;
        };
        let context = self.peek_attempt_context();
        let workspace = if bead.workspace.as_os_str().is_empty()
            || bead.workspace == std::path::Path::new(".")
        {
            self.config.workspace.default.clone()
        } else {
            bead.workspace.clone()
        };
        let transcript = if output.stderr.is_empty() {
            output.stdout.clone()
        } else if output.stdout.is_empty() {
            output.stderr.clone()
        } else {
            format!("{}\n{}", output.stdout, output.stderr)
        };
        match crate::wip::capture(crate::wip::Capture {
            workspace: &workspace,
            bead_id: bead.id.as_ref(),
            attempt_id: &attempt_id,
            started_at: context.started_at_wall,
            preexisting_paths: &context.predispatch_dirty_paths,
            transcript: &transcript,
        })
        .await
        {
            Ok(Some(patch)) => {
                tracing::info!(
                    bead_id = %bead.id,
                    path = %patch.path,
                    bytes = patch.bytes,
                    "captured WIP patch before releasing attempt"
                );
                self.set_wip_patch(Some(patch));
            }
            Ok(None) => tracing::debug!(bead_id = %bead.id, "attempt left no capturable WIP"),
            Err(error) => tracing::warn!(
                bead_id = %bead.id,
                error = %error,
                "failed to capture WIP patch before release"
            ),
        }
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
    pub(crate) async fn run_verification_gates(
        &self,
        bead: &Bead,
    ) -> Result<(bool, Option<GateReport>, GateResolutionTelemetry)> {
        if bead.workspace.as_os_str().is_empty() || bead.workspace.is_relative() {
            tracing::debug!(
                bead_id = %bead.id,
                workspace = %bead.workspace.display(),
                "bead workspace is unset or relative — no workspace config to resolve gates from, running none"
            );
            return Ok((true, None, GateResolutionTelemetry::none()));
        }
        let GatesConfig {
            gates: mut workspace_gates,
            verification: workspace_verification,
            declared,
            fallback_gate: workspace_fallback_gate,
        } = crate::config::gates_for_workspace(&bead.workspace).with_context(|| {
            format!(
                "failed to load validation gates from the bead's workspace config {}",
                bead.workspace.join(".needle.yaml").display()
            )
        })?;

        // Plan section 4.4 step 6: a workspace that says nothing about gates
        // is judged by the language default its build files imply. An
        // explicit `gates: []` is the opt-out and still runs none.
        let mut default_gate_name: Option<String> = None;
        if workspace_gates.is_empty() && workspace_verification.is_empty() && !declared {
            if let Some(detected) = crate::validation::default_gates::detect(
                &bead.workspace,
                &self.config.validation.default_gates,
            ) {
                tracing::info!(
                    bead_id = %bead.id,
                    workspace = %bead.workspace.display(),
                    language = detected.language,
                    evidence = %detected.evidence,
                    commands = ?detected.commands,
                    "bead's workspace declares no gates — applying the language default gate"
                );
                default_gate_name = Some(detected.gate_name());
                workspace_gates.push(GateConfig::Command {
                    commands: detected.commands,
                    stderr_cap_bytes: None,
                    run_in: crate::validation::RunIn::Clean,
                });
            }
        }

        if workspace_gates.is_empty() && workspace_verification.is_empty() {
            // An explicit empty declaration is an existing workspace opt-out;
            // leave that path unchanged. The fallback belongs only to a
            // workspace that declares neither gate format. The dedicated
            // `validation.fallback_gate` opt-out is a later split-child and
            // must not be inferred from the older language-default switch.
            if declared {
                tracing::debug!(
                    bead_id = %bead.id,
                    workspace = %bead.workspace.display(),
                    "bead's workspace declares no validation gates — running none"
                );
                return Ok((true, None, GateResolutionTelemetry::none()));
            }
            // needle-66b015d6 part 3: the workspace's own
            // `validation.fallback_gate` decides whether the built-in gate is
            // armed; absent falls back to the host-level default, which is
            // armed. The decision is logged on every dispatch so the ledger's
            // silence for a gate-less workspace is always explainable from the
            // logs: armed means the built-in gate ran, opted out means the
            // workspace asked NEEDLE not to.
            let fallback_armed =
                workspace_fallback_gate.unwrap_or(self.config.validation.fallback_gate);
            if !fallback_armed {
                tracing::warn!(
                    bead_id = %bead.id,
                    workspace = %bead.workspace.display(),
                    "validation.fallback_gate is false — built-in fallback gate opted out; \
                     the dispatch is judged on the agent's exit code alone"
                );
                return Ok((true, None, GateResolutionTelemetry::none()));
            }
            tracing::info!(
                bead_id = %bead.id,
                workspace = %bead.workspace.display(),
                "bead's workspace declares no gates — built-in fallback gate armed"
            );
            // Nothing opted out and no language default applied — the
            // workspace's own files still pick a verifier (needle-66b015d6
            // part 2): run the built-in fallback gate in the clean extraction
            // instead of waving the dispatch through. A workspace whose files
            // select nothing is judged by the clean-tree check inside, which
            // passes with the counted `not_detected` WARN.
            return self.run_fallback_gate(bead).await;
        }
        let gate_telemetry = GateResolutionTelemetry::bead_workspace(
            workspace_gates.len() + workspace_verification.len(),
        );
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
                    // A language default carries its provenance in its name
                    // so the ledger's gate_results say `default_rust`, not
                    // `gate_0`.
                    let name = match &default_gate_name {
                        Some(name) => name.clone(),
                        None => format!("gate_{}", i),
                    };
                    (name, config)
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
        Ok((all_passed, Some(report), gate_telemetry))
    }

    /// Run the built-in fallback gate for a workspace that declares no gates
    /// and implies no language default (needle-66b015d6 part 2).
    ///
    /// The verifier the workspace's own files select runs in the clean
    /// extraction of committed state under the standard gate timeout and
    /// stderr cap. The Node marker is policy-gated like the `default_*`
    /// gates above (needle-bb1052d4): the host's `validation.default_gates.
    /// node` commands when configured, no verifier at all when not — never
    /// the builtin `npm test`, which cannot pass in an extraction without
    /// `node_modules`. A workspace whose files select nothing passes only when
    /// its tree is clean — the extraction would otherwise silently drop the
    /// uncommitted remainder of the dispatch — and that pass is counted by
    /// `gate.no_verifier`. A failed verdict flows into the ordinary
    /// failed-verification path; a check that could not run surfaces as a
    /// `GateResult::ExecutionError`, which routes to the GateError path
    /// (release, failure-count untouched) per needle-4aaa010c.
    async fn run_fallback_gate(
        &self,
        bead: &Bead,
    ) -> Result<(bool, Option<GateReport>, GateResolutionTelemetry)> {
        let timeout =
            std::time::Duration::from_secs(self.config.validation.outcome_timeout_seconds);
        let stderr_cap_bytes = self.config.validation.stderr_cap_bytes;
        match self
            .fallback_verification
            .verify(
                bead,
                timeout,
                stderr_cap_bytes,
                &self.config.validation.default_gates,
            )
            .await
        {
            fallback_verification::FallbackVerdict::Pass(report) => {
                Ok((true, Some(report), GateResolutionTelemetry::none()))
            }
            fallback_verification::FallbackVerdict::NoVerifierPass(reason) => {
                // needle-bb1052d4: the reason distinguishes an undetected
                // workspace from a Node workspace whose gate was withheld
                // for want of a configured extraction-safe command — a host
                // debugging a repeat of the sun-sim releases has to be able
                // to tell them apart in the `gate.no_verifier` stream.
                let reason = reason.as_str();
                tracing::warn!(
                    bead_id = %bead.id,
                    workspace = %bead.workspace.display(),
                    reason,
                    "no verifier for this workspace — dispatch passes on the agent's exit code alone"
                );
                if let Err(error) = self.telemetry.emit(
                    EventKind::GateNoVerifier {
                        workspace: bead.workspace.display().to_string(),
                        reason: reason.to_string(),
                        gates_source: "none".to_string(),
                        command_gates_resolved: 0,
                    },
                    chrono::Utc::now(),
                ) {
                    tracing::warn!(error = %error, "failed to emit gate.no_verifier");
                }
                Ok((true, None, GateResolutionTelemetry::none()))
            }
            fallback_verification::FallbackVerdict::Fail(report) => {
                Ok((false, Some(report), GateResolutionTelemetry::none()))
            }
            fallback_verification::FallbackVerdict::ExecutionError(report) => {
                Ok((false, Some(report), GateResolutionTelemetry::none()))
            }
        }
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
        let (verified, gate_report, gate_telemetry) = if output.exit_code == 0 && !was_interrupted {
            self.run_verification_gates(bead).await?
        } else {
            // Non-zero exit or interrupted — verification irrelevant.
            (true, None, GateResolutionTelemetry::none())
        };

        // Classification consults the stream's result envelope in addition to
        // the exit code: a terminal API error exits 0 but is not a success.
        let mut outcome =
            classify_with_stream(output.exit_code, was_interrupted, verified, &output.stdout);

        // N-T23: judge the adapter's own failure signal before the bead is
        // penalised. A fingerprint that dominates the adapter's recent
        // failures across distinct beads is the provider's outage, not this
        // bead's result — 341 claude-print watchdog kills on 2026-09-10 were
        // booked as bead failures before this existed.
        let attempt_context = self.peek_attempt_context();
        let attempt_provider = attempt_context.provider.clone();
        let adapter_name = attempt_context.adapter;
        let attempt_actor = attempt_context.actor;
        let adapter_judgement = self.judge_adapter_health(
            &adapter_name,
            attempt_provider.as_deref(),
            bead,
            &outcome,
            output,
            gate_report.as_ref(),
        );
        let infra_fingerprint = match &adapter_judgement {
            Some(AdapterJudgement::Infrastructure { fingerprint }) => Some(fingerprint.clone()),
            _ => None,
        };

        // Gate evidence is captured before the routing match below, which
        // moves the report into the terminal handlers (N-T16 ledger row).
        let gate_results = gate_result_entries(gate_report.as_ref());
        let (resolved_outcome, resolved_reason) = match infra_fingerprint.as_deref() {
            Some(fingerprint) => (
                "infrastructure_failure".to_string(),
                Some(format!("provider_degraded:{fingerprint}")),
            ),
            None => (
                semantic_outcome(&outcome).to_string(),
                terminal_reason(&outcome, output.exit_code, gate_report.as_ref()),
            ),
        };
        // The bounded failure text the *next* attempt is shown (R3). Taken
        // here, before the report moves, from the same evidence the ledger
        // summarizes as a name.
        let failure_summary = attempt_failure_summary(&outcome, output, gate_report.as_ref());
        // N-T45: retain only a small, sanitized slice of the transcript and
        // gate output. The complete transcript remains owned by trace/archive
        // storage. Sanitizer construction is fail-closed for this prompt
        // evidence: a blocked sanitizer produces an explicit marker rather
        // than silently omitting the evidence or leaking raw content.
        let failure_evidence = if !matches!(outcome, Outcome::Success)
            && self
                .config
                .strands
                .learning
                .failure_history
                .evidence
                .enabled
        {
            let transcript = if output.stderr.is_empty() {
                output.stdout.clone()
            } else if output.stdout.is_empty() {
                output.stderr.clone()
            } else {
                format!("{}\n{}", output.stdout, output.stderr)
            };
            let max_bytes = self
                .config
                .strands
                .learning
                .failure_history
                .evidence
                .max_bytes;
            match build_failure_evidence_sanitizer(&self.config) {
                Ok(sanitizer) => crate::attempt_history::capture_failure_evidence_with_limit(
                    &transcript,
                    gate_report.as_ref(),
                    Some(&sanitizer),
                    max_bytes,
                    false,
                ),
                Err(error) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %error,
                        "failure evidence sanitizer unavailable; recording blocked markers"
                    );
                    crate::attempt_history::capture_failure_evidence_with_limit(
                        &transcript,
                        gate_report.as_ref(),
                        None,
                        max_bytes,
                        true,
                    )
                }
            }
            .or_else(|| Some(crate::attempt_history::FailureEvidence::unavailable()))
        } else {
            None
        };

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

        if matches!(outcome, Outcome::Success) {
            self.note_adapter_success(&adapter_name, attempt_provider.as_deref(), bead);
        }

        let (bead_action, telemetry_events) =
            if let Some(fingerprint) = infra_fingerprint.as_deref() {
                self.handle_infrastructure_failure(store, bead, fingerprint)
                    .await?
            } else {
                match outcome.clone() {
                    Outcome::Success => {
                        self.handle_success(store, bead, gate_report, gate_telemetry, &mut outcome)
                            .await?
                    }
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
                                            gate_telemetry,
                                        )
                                        .await?
                                    } else {
                                        unreachable!() // We already checked is_execution_error()
                                    }
                                } else {
                                    self.handle_gate_failure(store, bead, &report, gate_telemetry)
                                        .await?
                                }
                            } else {
                                self.handle_failure(store, bead).await?
                            }
                        } else {
                            self.handle_failure(store, bead).await?
                        }
                    }
                    Outcome::Timeout => self.handle_timeout(store, bead, output).await?,
                    Outcome::AgentNotFound => self.handle_agent_not_found(store, bead).await?,
                    Outcome::Interrupted => self.handle_interrupted(store, bead, output).await?,
                    Outcome::Crash(code) => self.handle_crash(store, bead, code, output).await?,
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
                        let reason = gate_report
                            .as_ref()
                            .and_then(|report| {
                                report.results.values().find_map(|result| match result {
                                    GateResult::Unsatisfiable(reason) => Some(reason.as_str()),
                                    _ => None,
                                })
                            })
                            .unwrap_or("gate precondition is unsatisfiable");
                        self.handle_gate_unsatisfiable(
                            store,
                            bead,
                            &bead.workspace.display().to_string(),
                            "validation",
                            reason,
                        )
                        .await?
                    }
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

        // N-T46 (ADR-030): a decomposition earns no verified credit. The
        // verdict above judged the process and its gates; whether the attempt
        // delivered the work or only split the bead is decided here, once the
        // agent's own bead mutations are visible. Gate evidence is kept.
        let (resolved_outcome, resolved_reason) = if matches!(outcome, Outcome::GateUnsatisfiable) {
            (
                semantic_outcome(&outcome).to_string(),
                terminal_reason(&outcome, output.exit_code, None),
            )
        } else {
            match self
                .classify_decomposition(
                    store,
                    bead,
                    &attempt_context.prompt_template,
                    !attempt_context.commits.is_empty(),
                    &resolved_outcome,
                )
                .await
            {
                Some(decomposition) => (
                    crate::attempt_accounting::DECOMPOSED.to_string(),
                    Some(decomposition.terminal_reason().to_string()),
                ),
                None => (resolved_outcome, resolved_reason),
            }
        };

        let ledger = self.emit_attempt_resolved(
            bead,
            output,
            bead_action.to_string(),
            &resolved_outcome,
            resolved_reason,
            gate_results,
        );

        // R3: what this attempt was and why it ended becomes the next
        // attempt's context. Best-effort and bounded; it never changes the
        // action decided above.
        self.record_attempt_history(store, bead, &ledger, failure_summary, failure_evidence)
            .await;

        // Plan section 4.4 step 4 / ADR-024: the backend's own attempt ledger
        // gets the same outcome (bead-rs `resolve --action none`), which is
        // what its failure-tier scheduling and cross-host attempt history
        // read. Idempotent per attempt ID; best-effort.
        self.record_backend_resolution(store, bead, &ledger, &attempt_actor)
            .await;

        // Plan section 4.4 step 1: the durable copy of the attempt lives off
        // the worker host. Bundle the attempt's trace into the spool the
        // external drain uploads from (attempt_archive.enabled).
        self.spool_attempt_archive(bead, &ledger).await;

        Ok(HandlerResult {
            outcome,
            bead_action,
            telemetry_events,
            budget_exhausted: false,
        })
    }

    /// Hand the resolved attempt's trace to the attempt-archive spool.
    async fn spool_attempt_archive(
        &self,
        bead: &Bead,
        ledger: &crate::telemetry::AttemptResolvedFields,
    ) {
        let archive = self.config.attempt_archive.clone();
        if !archive.enabled {
            return;
        }
        let workspace = if bead.workspace.as_os_str().is_empty()
            || bead.workspace == std::path::Path::new(".")
        {
            self.config.workspace.default.clone()
        } else {
            bead.workspace.clone()
        };
        let trace_dir = workspace
            .join(".beads")
            .join("traces")
            .join(bead.id.as_ref());
        let input = crate::attempt_archive::AttemptArchiveInput {
            attempt_id: ledger.attempt_id.clone(),
            bead_id: bead.id.to_string(),
            workspace: workspace.display().to_string(),
            worker: ledger.worker.clone(),
            adapter: ledger.adapter.clone(),
            model: ledger.model.clone(),
            outcome: ledger.outcome.clone(),
            terminal_reason: ledger.terminal_reason.clone(),
            recorded_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        };
        let bead_id = bead.id.clone();
        let work = tokio::task::spawn_blocking(move || {
            crate::attempt_archive::spool_attempt(&archive, &input, Some(&trace_dir))
        });
        match tokio::time::timeout(std::time::Duration::from_secs(60), work).await {
            Ok(Ok(Ok(Some(receipt)))) => tracing::info!(
                bead_id = %bead_id,
                bundle = %receipt.bundle.display(),
                bundle_bytes = receipt.bundle_bytes,
                "attempt archived to spool"
            ),
            Ok(Ok(Ok(None))) => {}
            Ok(Ok(Err(e))) => tracing::warn!(
                bead_id = %bead_id,
                error = %e,
                "attempt archive spool failed"
            ),
            Ok(Err(e)) => {
                tracing::warn!(bead_id = %bead_id, error = %e, "attempt archive task failed")
            }
            Err(_) => tracing::warn!(bead_id = %bead_id, "attempt archive spool timed out"),
        }
    }

    /// Hand the resolved attempt to the backend's attempt ledger.
    async fn record_backend_resolution(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        ledger: &crate::telemetry::AttemptResolvedFields,
        actor: &str,
    ) {
        if !self.config.outcome.resolve_attempts_in_backend {
            return;
        }
        let resolution = crate::bead_store::AttemptResolution {
            bead_id: bead.id.clone(),
            attempt_id: ledger.attempt_id.clone(),
            // bead-rs has no decomposition class; the mapping keeps a split
            // from resetting or extending the bead's failure run there.
            outcome: crate::attempt_accounting::backend_outcome(&ledger.outcome).to_string(),
            actor: if actor.is_empty() {
                ledger.worker.clone()
            } else {
                actor.to_string()
            },
            reason: ledger.terminal_reason.clone(),
            evidence_ref: ledger.commits.first().map(|sha| format!("commit:{sha}")),
        };
        match tokio::time::timeout(
            std::time::Duration::from_secs(20),
            store.resolve_attempt(&resolution),
        )
        .await
        {
            Ok(Ok(Some(receipt))) => tracing::info!(
                bead_id = %bead.id,
                attempt_id = %resolution.attempt_id,
                outcome = %resolution.outcome,
                receipt_id = %receipt.receipt_id,
                attempt_tier = ?receipt.resulting_attempt_tier,
                is_replay = receipt.is_replay,
                "attempt outcome recorded in the bead backend"
            ),
            Ok(Ok(None)) => tracing::debug!(
                bead_id = %bead.id,
                "backend does not advertise attempt outcomes; NEEDLE ledger row is the record"
            ),
            Ok(Err(e)) => tracing::warn!(
                bead_id = %bead.id,
                attempt_id = %resolution.attempt_id,
                error = %e,
                "backend refused the attempt resolution; NEEDLE ledger row is the record"
            ),
            Err(_) => tracing::warn!(
                bead_id = %bead.id,
                "backend attempt resolution timed out; NEEDLE ledger row is the record"
            ),
        }
    }

    /// Whether this attempt decomposed its bead instead of delivering it
    /// (N-T46, ADR-030; the rules are [`crate::attempt_accounting`]).
    ///
    /// Reads the bead back only for a commit-less verified success outside
    /// the split template, the one shape whose answer depends on labels the
    /// agent may have just written. A failed read decides nothing: the row
    /// keeps its verdict rather than guessing.
    async fn classify_decomposition(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        prompt_template: &str,
        delivered_commits: bool,
        resolved_outcome: &str,
    ) -> Option<crate::attempt_accounting::Decomposition> {
        use crate::attempt_accounting as accounting;
        let labels_after =
            if accounting::needs_labels_after(resolved_outcome, prompt_template, delivered_commits)
            {
                match self.timeout_op(|| store.show(&bead.id), "show").await {
                    Ok(Some(current)) => Some(current.labels),
                    Ok(None) | Err(_) => None,
                }
            } else {
                None
            };
        accounting::classify_decomposition(&accounting::AttemptShape {
            outcome: resolved_outcome,
            prompt_template,
            delivered_commits,
            labels_before: &bead.labels,
            labels_after: labels_after.as_deref(),
        })
    }

    /// Record the adapter's own failure signal against its host-wide health
    /// window (N-T23) and say whether this failure is the provider's, not the
    /// bead's. `None` when adapter health is off, the adapter is unknown, or
    /// the outcome carries no adapter-level signal (a rejecting gate is the
    /// bead's result; a plain timeout follows the ADR-022 ladder).
    ///
    /// Adapter-level signals include stream API errors, every non-zero exit
    /// code, crashes, and missing binaries. A normal non-zero exit is still
    /// judged as the bead's own failure until the aggregate detector sees the
    /// same signal across enough distinct beads; one hard task therefore
    /// cannot be misjudged as an outage.
    fn judge_adapter_health(
        &self,
        adapter: &str,
        provider: Option<&str>,
        bead: &Bead,
        outcome: &Outcome,
        output: &AgentOutcome,
        gate_report: Option<&GateReport>,
    ) -> Option<AdapterJudgement> {
        if !self.config.workspace_health.adapter_health_enabled || adapter.is_empty() {
            return None;
        }
        // N-T51: key the health by the adapter's provider only once the
        // configuration turns gateway keying on; until then the provider is
        // dropped and every adapter keeps its own state.
        let keyed_provider = self
            .config
            .workspace_health
            .provider_keyed_health
            .then_some(provider)
            .flatten();
        let reason = adapter_health_failure_reason(outcome, output, gate_report)?;

        match crate::provider_health::record_adapter_failure(
            adapter,
            keyed_provider,
            bead.id.as_ref(),
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
                        adapter = %adapter,
                        bead_id = %bead.id,
                        fingerprint = %fingerprint,
                        failures = failures,
                        distinct_beads = distinct_beads,
                        summary = %summary,
                        "adapter failure fingerprint dominates its window — provider degraded"
                    );
                    let (provider_name, affected_adapters) =
                        crate::provider_health::degraded_state(adapter, keyed_provider)
                            .ok()
                            .flatten()
                            .map(|state| {
                                let provider_name = state.health_key().to_string();
                                let affected_adapters = self
                                    .known_provider_adapters(&state)
                                    .unwrap_or_else(|| state.affected_adapters());
                                (provider_name, affected_adapters)
                            })
                            .unwrap_or_else(|| {
                                (
                                    crate::provider_health::resolve_key(keyed_provider, adapter)
                                        .to_string(),
                                    vec![adapter.to_string()],
                                )
                            });
                    let _ = self.telemetry.emit_try_lock(
                        EventKind::ProviderDegraded {
                            adapter: adapter.to_string(),
                            provider: provider_name,
                            adapters: affected_adapters,
                            fingerprint: fingerprint.clone(),
                            summary: summary.clone(),
                            failures: *failures as u32,
                            distinct_beads: *distinct_beads as u32,
                            bead_id: bead.id.clone(),
                        },
                        Utc::now(),
                    );
                }
                if recording.is_infra() {
                    Some(AdapterJudgement::Infrastructure {
                        fingerprint: recording.fingerprint().to_string(),
                    })
                } else {
                    Some(AdapterJudgement::Judged)
                }
            }
            Err(e) => {
                tracing::warn!(
                    adapter = %adapter,
                    bead_id = %bead.id,
                    error = %e,
                    "could not record adapter health — judging the failure as the bead's"
                );
                None
            }
        }
    }

    /// Resolve the configured adapter roster for a provider-health event.
    /// The state file records members that have reported health, but a
    /// degradation affects every configured adapter behind the gateway,
    /// including siblings that have not failed yet. Loading the same built-in
    /// plus user adapter set as the dispatcher keeps the event's affected list
    /// complete; an unreadable adapter directory leaves the durable state
    /// membership as a safe fallback.
    fn known_provider_adapters(
        &self,
        state: &crate::provider_health::ProviderHealthState,
    ) -> Option<Vec<String>> {
        let adapters = crate::dispatch::load_adapters(
            &self.config.agent.adapters_dir,
            &crate::dispatch::builtin_adapters(),
        )
        .ok()?;
        Some(crate::provider_health::expand_degraded_adapters(
            std::slice::from_ref(state),
            adapters
                .into_values()
                .map(|adapter| (adapter.name, adapter.provider)),
        ))
    }

    /// A verified success lifts the adapter's degradation (N-T23) — and, with
    /// gateway keying on, the degradation of every adapter behind the same
    /// provider (N-T51).
    fn note_adapter_success(&self, adapter: &str, provider: Option<&str>, bead: &Bead) {
        if !self.config.workspace_health.adapter_health_enabled || adapter.is_empty() {
            return;
        }
        let keyed_provider = self
            .config
            .workspace_health
            .provider_keyed_health
            .then_some(provider)
            .flatten();
        match crate::provider_health::record_adapter_success(adapter, keyed_provider) {
            Ok(Some(prior)) => {
                tracing::info!(
                    adapter = %adapter,
                    bead_id = %bead.id,
                    "adapter produced a verified success — provider restored"
                );
                let _ = self.telemetry.emit_try_lock(
                    EventKind::ProviderRestored {
                        adapter: adapter.to_string(),
                        provider: prior.health_key().to_string(),
                        adapters: self
                            .known_provider_adapters(&prior)
                            .unwrap_or_else(|| prior.affected_adapters()),
                        bead_id: bead.id.clone(),
                        degraded_duration_secs: prior.degraded_for_secs().unwrap_or(0),
                    },
                    Utc::now(),
                );
            }
            Ok(None) => {}
            Err(e) => tracing::debug!(
                adapter = %adapter,
                error = %e,
                "could not clear adapter health state"
            ),
        }
    }

    /// Infrastructure failure: release the bead with no failure count. The
    /// provider, not the work, is what failed (N-T23; ADR-023 for gates).
    async fn handle_infrastructure_failure(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        fingerprint: &str,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::warn!(
            bead_id = %bead.id,
            fingerprint = %fingerprint,
            "adapter is degraded for this failure — releasing bead without penalty"
        );
        let mut events = self.prepare_release_events(store, bead).await?;
        events.push(EventKind::BeadReleased {
            bead_id: bead.id.clone(),
            reason: format!("infrastructure:{fingerprint}"),
        });
        Ok((
            BeadAction::Released(ReleaseReason::InfrastructureFailure),
            events,
        ))
    }

    /// Persist this attempt into the bead's history (local journal plus the
    /// bead-rs structured-data mirror) so the next dispatch prompt can show
    /// it. See [`crate::attempt_history`].
    async fn record_attempt_history(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        ledger: &crate::telemetry::AttemptResolvedFields,
        failure_summary: Option<String>,
        failure_evidence: Option<crate::attempt_history::FailureEvidence>,
    ) {
        let history = &self.config.strands.learning.failure_history;
        let candidates_enabled = self.config.strands.learning.candidate_lessons.enabled;
        if !history.enabled && !candidates_enabled {
            return;
        }
        let workspace = if bead.workspace.as_os_str().is_empty()
            || bead.workspace == std::path::Path::new(".")
        {
            self.config.workspace.default.clone()
        } else {
            bead.workspace.clone()
        };
        let record = crate::attempt_history::AttemptRecord {
            schema_version: crate::attempt_history::SCHEMA_VERSION,
            attempt_id: ledger.attempt_id.clone(),
            recorded_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            worker: ledger.worker.clone(),
            adapter: ledger.adapter.clone(),
            model: ledger.model.clone(),
            outcome: ledger.outcome.clone(),
            terminal_reason: ledger.terminal_reason.clone(),
            exit_code: ledger.exit_code,
            requested_action: ledger.requested_action.clone(),
            commits: ledger.commits.clone(),
            duration_ms: ledger.duration_ms,
            failure_summary,
            failure_evidence,
            wip_patch: ledger.wip_patch.clone(),
        };
        crate::attempt_history::record(
            &workspace,
            &bead.id,
            record,
            store,
            history.enabled && history.sync_to_bead_data,
        )
        .await;

        if self.config.strands.learning.candidate_lessons.enabled
            && ledger.outcome == "verified_success"
        {
            self.record_candidate_lesson(store, bead, &workspace, ledger)
                .await;
        }
    }

    /// Produce the N-T50 candidate only after the current attempt has been
    /// recorded as a verified success. Candidate construction is pure; this
    /// method only gathers read-only git facts and hands the result to the
    /// attempt-history storage boundary.
    async fn record_candidate_lesson(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        workspace: &std::path::Path,
        ledger: &crate::telemetry::AttemptResolvedFields,
    ) {
        let records = match crate::attempt_history::load_local(workspace, &bead.id) {
            Ok(records) => records,
            Err(error) => {
                tracing::debug!(
                    bead_id = %bead.id,
                    %error,
                    "candidate lesson: could not load attempt history"
                );
                return;
            }
        };
        let Some(failed) = records
            .iter()
            .rev()
            .skip(1)
            .find(|record| record.outcome == "work_failure")
        else {
            return;
        };

        let intervention = candidate_intervention_summary(
            workspace,
            ledger.bead_revision_start.as_deref(),
            &ledger.commits,
            failed,
            &ledger.gate_results,
        )
        .await;
        let sanitizer = match build_failure_evidence_sanitizer(&self.config) {
            Ok(sanitizer) => sanitizer,
            Err(error) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    %error,
                    "candidate lesson: sanitizer unavailable; refusing candidate"
                );
                return;
            }
        };
        let Some(lesson) = crate::learning::build_candidate_lesson(
            &records,
            intervention,
            bead.id.as_ref(),
            &workspace.display().to_string(),
            &ledger.adapter,
            &sanitizer,
        ) else {
            return;
        };
        crate::attempt_history::record_candidate_lesson(
            workspace,
            &bead.id,
            lesson,
            store,
            self.config
                .strands
                .learning
                .candidate_lessons
                .sync_to_bead_data,
        )
        .await;
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
    ) -> crate::telemetry::AttemptResolvedFields {
        let attempt = self.take_attempt_context();
        let provenance = self
            .attempt_provenance
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_default();
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

        let fields = crate::telemetry::AttemptResolvedFields {
            attempt_id,
            // Direct handler callers without claim provenance remain
            // provisional; worker-owned attempts are authoritative even when
            // a legacy backend cannot expose a numeric revision.
            provisional: provenance.assignee.is_none(),
            bead_id: bead.id.clone(),
            workspace: bead.workspace.display().to_string(),
            bead_revision_start: attempt.bead_revision_start,
            claim_revision: provenance.claim_revision,
            assignee: provenance.assignee,
            claim_epoch: provenance.claim_epoch,
            backend_capabilities: provenance.backend_capabilities,
            worker: self.telemetry.worker_id().to_string(),
            adapter: provenance
                .adapter
                .unwrap_or_else(|| attempt.adapter.clone()),
            harness: provenance.harness,
            model: attempt.model,
            provider: attempt.provider,
            prompt_template: attempt.prompt_template,
            template_version: attempt.template_version,
            // ContextManifest hashing is N-T10; the hash is absent until then.
            context_manifest_hash: provenance.context_manifest_hash,
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
            costed: attempt.costed,
            commits: attempt.commits,
            duration_ms,
            terminal_reason: resolved_reason,
            // The exit code is observation only: it says what the process did,
            // not whether the work was accepted — `outcome` carries that.
            exit_code: output.exit_code,
            wip_patch: attempt.wip_patch,
        };
        let event = EventKind::AttemptResolved(Box::new(fields.clone()));

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
        fields
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
                budget_exhausted: false,
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
                    budget_exhausted: false,
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
        gate_telemetry: GateResolutionTelemetry,
        outcome: &mut Outcome,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::info!(bead_id = %bead.id, "agent completed successfully");

        // Emit telemetry even when zero gates were resolved: an explicit
        // `gates_source=none` distinguishes that case from a gate that could
        // not be resolved or executed.
        self.telemetry.emit(
            EventKind::VerificationPassed {
                bead_id: bead.id.clone(),
                gates_run: gate_report
                    .as_ref()
                    .map(|report| report.results.len() as u32)
                    .unwrap_or(0),
                gates_source: gate_telemetry.gates_source.to_string(),
                command_gates_resolved: gate_telemetry.command_gates_resolved,
            },
            chrono::Utc::now(),
        )?;

        if let Some(report) = gate_report {
            let gates_run = report.results.len() as u32;
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
                    .restore_degraded_workspace(store, workspace_path, &bead.id, gate_telemetry)
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

                // Close-evidence gate: a close reason must carry a `verified:`
                // block, and every verifiable command it claims — plus any
                // `go test`/`cargo test` acceptance command the bead's own
                // description names — is re-run in a clean extraction of
                // committed state before the close is honoured. Runs before
                // shipped-work's failure-count reset so a rejected close never
                // benefits from one.
                match self
                    .verify_close_evidence(store, bead, current.body.as_deref())
                    .await?
                {
                    close_verification::CloseEvidenceVerdict::Skipped
                    | close_verification::CloseEvidenceVerdict::Pass => {}
                    close_verification::CloseEvidenceVerdict::Fail(report) => {
                        return self
                            .handle_gate_failure(store, bead, &report, gate_telemetry)
                            .await;
                    }
                    close_verification::CloseEvidenceVerdict::ExecutionError {
                        command,
                        reason,
                    } => {
                        return self
                            .handle_gate_error(
                                store,
                                bead,
                                &bead.workspace.display().to_string(),
                                close_verification::GATE_NAME,
                                &command,
                                &reason,
                                gate_telemetry,
                            )
                            .await;
                    }
                }

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
                            return self
                                .handle_gate_failure(store, bead, &report, gate_telemetry)
                                .await;
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
                                    return self
                                        .handle_gate_failure(store, bead, &report, gate_telemetry)
                                        .await;
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
                        Ok(crate::validation::GateResult::Unsatisfiable(reason)) => {
                            *outcome = Outcome::GateUnsatisfiable;
                            tracing::warn!(
                                bead_id = %bead.id,
                                reason = %reason,
                                "shipped-work precondition is unsatisfiable — releasing without incrementing failure count"
                            );
                            return self
                                .handle_gate_unsatisfiable(
                                    store,
                                    bead,
                                    &bead.workspace.display().to_string(),
                                    "shipped_work",
                                    &reason,
                                )
                                .await;
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
                                    gate_telemetry,
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
                            // The gate CONFIRMED the work landed; the agent merely failed to
                            // close the bead. Close it here.
                            //
                            // This previously released instead, on the stated grounds that the
                            // store had "no close method". It has one — BeadStore::close,
                            // implemented by cli_store against `bead close --id --reason` — so
                            // the comment was stale. Releasing a bead whose work is finished puts
                            // it straight back on the ready frontier, where another worker claims
                            // and redoes it: the subscription pays twice for one unit of output
                            // while the fleet looks busy. It also emitted BeadCompleted AND
                            // BeadReleased for the same bead, which is why completed and released
                            // counts overlapped and deliberate releases read as failures.
                            events.push(EventKind::BeadCompleted {
                                bead_id: bead.id.clone(),
                                duration_ms: 0,
                            });
                            match self
                                .timeout_op(
                                    || store.close(&bead.id, SHIPPED_WORK_CLOSE_REASON),
                                    "close",
                                )
                                .await
                            {
                                Ok(Some(())) => return Ok((BeadAction::Closed, events)),
                                other => {
                                    // A backend that cannot close, or a timeout. Fall back to the
                                    // previous behaviour rather than leaving the claim dangling —
                                    // an open bead is recoverable, a stuck claim is not.
                                    tracing::warn!(
                                        bead_id = %bead.id,
                                        close_result = ?other.as_ref().map(|o| o.is_some()),
                                        "shipped work confirmed but close failed; releasing \
                                         instead (the bead will be re-worked)"
                                    );
                                    let mut release_events =
                                        self.prepare_release_events(store, bead).await?;
                                    events.append(&mut release_events);
                                    return Ok((
                                        BeadAction::Released(ReleaseReason::ShippedWorkCloseFailed),
                                        events,
                                    ));
                                }
                            }
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
                            return Ok((BeadAction::Released(ReleaseReason::GateFailed), events));
                        }
                        Ok(crate::validation::GateResult::Unsatisfiable(reason)) => {
                            *outcome = Outcome::GateUnsatisfiable;
                            tracing::warn!(
                                bead_id = %bead.id,
                                reason = %reason,
                                "shipped-work precondition is unsatisfiable — releasing orphaned bead without incrementing failure count"
                            );
                            let mut release_events =
                                self.prepare_release_events(store, bead).await?;
                            release_events.push(EventKind::BeadReleased {
                                bead_id: bead.id.clone(),
                                reason: "gate_unsatisfiable".to_string(),
                            });
                            return Ok((
                                BeadAction::Released(ReleaseReason::GateExecutionError),
                                release_events,
                            ));
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
                            return Ok((
                                BeadAction::Released(ReleaseReason::GateExecutionError),
                                events,
                            ));
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
                            return Ok((BeadAction::Released(ReleaseReason::GateError), events));
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
                    return Ok((
                        BeadAction::Released(ReleaseReason::EnforcementDisabled),
                        events,
                    ));
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
                return Ok((
                    BeadAction::Released(ReleaseReason::ClosureVerificationTimeout),
                    events,
                ));
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
                return Ok((
                    BeadAction::Released(ReleaseReason::ClosureVerificationError),
                    events,
                ));
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

    /// Judge the close reason of a bead the agent has closed.
    ///
    /// The pluck prompt requires every close reason to end with a fenced
    /// `verified:` block listing the commands the agent ran; this re-runs the
    /// verifiable ones (plus the bead description's own acceptance commands)
    /// in a clean extraction before the close is honoured. The gate applies
    /// only where the backend can expose the close reason at all — everywhere
    /// else it skips rather than judging closes it cannot see. A close reason
    /// that cannot be *fetched* is a store hiccup, not missing evidence, so
    /// it also fails open; a reason that plainly carries no block does not.
    async fn verify_close_evidence(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        description: Option<&str>,
    ) -> Result<close_verification::CloseEvidenceVerdict> {
        if !store.exposes_close_reason() {
            tracing::debug!(
                bead_id = %bead.id,
                "backend cannot expose close reasons — close-evidence gate skipped"
            );
            return Ok(close_verification::CloseEvidenceVerdict::Skipped);
        }

        // Not `timeout_op`: its Ok(None) folds "store timed out" and "store
        // returned nothing" into one value, and the two mean opposite things
        // here — a timeout fails open, a fetched-empty reason is the missing
        // evidence the gate exists to reject.
        let close_reason = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            store.close_reason(&bead.id),
        )
        .await
        {
            Ok(Ok(reason)) => reason,
            Ok(Err(e)) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "close reason could not be fetched — failing open, not rejecting the close"
                );
                return Ok(close_verification::CloseEvidenceVerdict::Skipped);
            }
            Err(_) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    "close_reason timed out after 30s — failing open, not rejecting the close"
                );
                return Ok(close_verification::CloseEvidenceVerdict::Skipped);
            }
        };

        let Some(reason) = close_reason else {
            tracing::warn!(
                bead_id = %bead.id,
                "bead closed with no close reason — rejecting the close"
            );
            return Ok(close_verification::CloseEvidenceVerdict::Fail(
                GateReport::single_failure(
                    close_verification::GATE_NAME,
                    close_verification::MISSING_EVIDENCE_REASON,
                ),
            ));
        };

        let Some(claimed) = close_verification::parse_verified_block(&reason) else {
            tracing::warn!(
                bead_id = %bead.id,
                "close reason carries no `verified:` block — rejecting the close"
            );
            return Ok(close_verification::CloseEvidenceVerdict::Fail(
                GateReport::single_failure(
                    close_verification::GATE_NAME,
                    close_verification::MISSING_EVIDENCE_REASON,
                ),
            ));
        };

        let commands = close_verification::collect_rerun_commands(
            bead.id.as_ref(),
            &claimed,
            description,
            &bead.workspace,
        );
        if commands.is_empty() {
            tracing::info!(
                bead_id = %bead.id,
                "close reason carries a verified block but no re-runnable command — honouring the close"
            );
            return Ok(close_verification::CloseEvidenceVerdict::Pass);
        }

        tracing::info!(
            bead_id = %bead.id,
            commands = commands.len(),
            "re-running close-evidence commands in the clean extraction"
        );
        Ok(self.close_verification.verify(bead, &commands).await)
    }

    /// File the single repair bead that lets a red build circuit recover.
    async fn ensure_build_repair_bead(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        gate: &str,
        reason: &str,
    ) {
        if !is_build_gate_failure(gate, reason) {
            return;
        }
        let workspace = if bead.workspace.as_os_str().is_empty()
            || bead.workspace == std::path::Path::new(".")
        {
            self.config.workspace.default.clone()
        } else {
            bead.workspace.clone()
        };
        let commit_sha = candidate_git_lines(&workspace, &["rev-parse", "HEAD"])
            .await
            .and_then(|lines| lines.into_iter().next())
            .unwrap_or_else(|| "unknown".to_string());
        if let Err(error) =
            crate::strand::splice::ensure_fix_build_bead(store, &workspace, &commit_sha, reason)
                .await
        {
            tracing::error!(
                bead_id = %bead.id,
                workspace = %workspace.display(),
                error = %error,
                "failed to create the fix-build bead for a red build"
            );
        }
    }

    /// Handle gate failure: reopen the bead if it was closed, then release it.
    async fn handle_gate_failure(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        report: &crate::validation::GateReport,
        gate_telemetry: GateResolutionTelemetry,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        // Find the first failing gate for telemetry.
        let (failed_gate, reason) = report
            .results
            .iter()
            .find(|(_, r)| !r.passed())
            .map(|(name, r)| {
                let reason = match r {
                    GateResult::Fail(reason) => reason.clone(),
                    GateResult::Unsatisfiable(reason) => reason.clone(),
                    GateResult::ExecutionError { reason, .. } => reason.clone(),
                    GateResult::Pass => String::new(),
                };
                (name.clone(), reason)
            })
            .unwrap_or_else(|| ("unknown".to_string(), "unknown error".to_string()));

        tracing::warn!(
            bead_id = %bead.id,
            gate = %failed_gate,
            reason = %reason,
            "validation gate failed — releasing bead"
        );

        self.ensure_build_repair_bead(store, bead, &failed_gate, &reason)
            .await;

        // Emit verification failure telemetry.
        self.telemetry.emit(
            EventKind::VerificationFailed {
                bead_id: bead.id.clone(),
                command: failed_gate.clone(),
                exit_code: None,
                output: reason.clone(),
                gates_source: gate_telemetry.gates_source.to_string(),
                command_gates_resolved: gate_telemetry.command_gates_resolved,
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
                            gate_telemetry,
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
        let mut false_close_detected = false;

        // If the agent already closed the bead, reopen it before releasing.
        // Use timeout to prevent indefinite hang in HANDLING state.
        match self.timeout_op(|| store.show(&bead.id), "show").await {
            Ok(Some(current)) if current.status.is_done() => {
                tracing::info!(
                    bead_id = %bead.id,
                    "reopening bead closed by agent (verification failed)"
                );
                match self.timeout_op(|| store.reopen(&bead.id), "reopen").await {
                    Ok(Some(_)) => {
                        false_close_detected = true;
                        let attempt = self.peek_attempt_context();
                        events.push(EventKind::FalseCloseDetected {
                            bead_id: bead.id.clone(),
                            workspace: bead.workspace.display().to_string(),
                            adapter: if attempt.adapter.is_empty() {
                                "unknown".to_string()
                            } else {
                                attempt.adapter
                            },
                            model: attempt.model,
                            class: false_close_class(&failed_gate, &reason),
                        });
                    }
                    Ok(None) => {
                        events.push(EventKind::WorkerHandlingTimeout {
                            bead_id: bead.id.clone(),
                            outcome: "gate_failure".to_string(),
                            operation: "reopen".to_string(),
                            error: "timeout after 30s".to_string(),
                        });
                    }
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
        let mut action = BeadAction::Released(ReleaseReason::GateFailed);
        if infra_failure && !false_close_detected {
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
        if !infra_failure || false_close_detected {
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
        gate_telemetry: GateResolutionTelemetry,
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
                    gates_source: gate_telemetry.gates_source.to_string(),
                    command_gates_resolved: gate_telemetry.command_gates_resolved,
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
                gates_source: gate_telemetry.gates_source.to_string(),
                command_gates_resolved: gate_telemetry.command_gates_resolved,
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
    #[allow(clippy::too_many_arguments)] // gate execution context plus provenance
    async fn handle_gate_error(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        workspace: &str,
        gate_name: &str,
        command: &str,
        reason: &str,
        gate_telemetry: GateResolutionTelemetry,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::warn!(
            bead_id = %bead.id,
            workspace,
            gate = %gate_name,
            command = %command,
            reason = %reason,
            "gate execution error — releasing bead without incrementing failure count"
        );

        self.ensure_build_repair_bead(store, bead, gate_name, reason)
            .await;

        let mut events = Vec::new();

        // Emit gate execution error telemetry.
        self.telemetry.emit(
            EventKind::GateExecutionError {
                bead_id: bead.id.clone(),
                workspace: workspace.to_string(),
                gate: gate_name.to_string(),
                command: command.to_string(),
                reason: reason.to_string(),
                gates_source: gate_telemetry.gates_source.to_string(),
                command_gates_resolved: gate_telemetry.command_gates_resolved,
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
                            gate_telemetry,
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

        Ok((BeadAction::Released(ReleaseReason::AgentNotFound), events))
    }

    /// Handle a gate whose precondition is impossible for this workspace.
    ///
    /// Unlike a normal gate failure, this is not evidence against the bead and
    /// must not increment its failure count or create a work-failure signal.
    /// The caller has already classified the attempt as
    /// `Outcome::GateUnsatisfiable`; this helper only performs the safe release.
    async fn handle_gate_unsatisfiable(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        workspace: &str,
        gate_name: &str,
        reason: &str,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::warn!(
            bead_id = %bead.id,
            workspace,
            gate = %gate_name,
            reason = %reason,
            "gate precondition is unsatisfiable — releasing bead without incrementing failure count"
        );

        let mut events = self.prepare_release_events(store, bead).await?;
        events.push(EventKind::BeadReleased {
            bead_id: bead.id.clone(),
            reason: "gate_unsatisfiable".to_string(),
        });
        Ok((
            BeadAction::Released(ReleaseReason::GateExecutionError),
            events,
        ))
    }

    /// Create a "Gate broken" alert bead when workspace degrades.
    ///
    /// This method creates a P0 bead with fingerprinting to prevent duplicates.
    /// The bead remains claimable - fixing a gate is verified by running it.
    #[allow(clippy::too_many_arguments)] // gate-health alert shape; see fingerprint sibling
    async fn create_gate_broken_bead(
        &self,
        store: &dyn BeadStore,
        workspace: &str,
        gate_name: &str,
        command: &str,
        reason: &str,
        previous_state: Option<&crate::gate_health::GateHealthState>,
        gate_telemetry: GateResolutionTelemetry,
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
                        gates_source: gate_telemetry.gates_source.to_string(),
                        command_gates_resolved: gate_telemetry.command_gates_resolved,
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
                        gates_source: gate_telemetry.gates_source.to_string(),
                        command_gates_resolved: gate_telemetry.command_gates_resolved,
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
                        gates_source: gate_telemetry.gates_source.to_string(),
                        command_gates_resolved: gate_telemetry.command_gates_resolved,
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
        gate_telemetry: GateResolutionTelemetry,
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

        // `store` is already scoped to `workspace_path`. Do not use the
        // record's `workspace` field as a second ownership check: bead-rs
        // stores workspace-native issues with a NULL `source_repo`, which the
        // CLI omits and `Bead` represents as an empty path.
        let gate_broken_beads: Vec<&Bead> = all_beads
            .iter()
            .filter(|b| b.title.starts_with("Gate broken:") && b.status != BeadStatus::Closed)
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
        }

        // Restoration is a workspace-level outcome, not one event per alert
        // bead. Emit exactly once after every matching alert is closed. The
        // successful bead identifies the gate run that proved recovery, and
        // the event remains observable when the scoped store had no matching
        // alert bead at all.
        self.telemetry.emit(
            EventKind::WorkspaceGateRestored {
                workspace: workspace.clone(),
                bead_id: success_bead_id.clone(),
                degraded_duration_secs,
                gates_source: gate_telemetry.gates_source.to_string(),
                command_gates_resolved: gate_telemetry.command_gates_resolved,
            },
            Utc::now(),
        )?;

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
        match self.undo_degraded_window_penalties(store).await {
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
    async fn undo_degraded_window_penalties(&self, store: &dyn BeadStore) -> Result<usize> {
        let all_beads = store
            .list_all()
            .await
            .context("failed to list beads while undoing degraded-window penalties")?;

        // Like alert lookup above, this store is already workspace-scoped and
        // workspace-native bead-rs records may have no `source_repo` value.
        let marked: Vec<&Bead> = all_beads
            .iter()
            .filter(|b| {
                b.labels
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
        let mut action = BeadAction::Released(ReleaseReason::DispatchFailed);
        if release_succeeded
            && self
                .count_failure_toward_quarantine(store, bead, &mut events)
                .await
        {
            action = BeadAction::Quarantined;
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

    /// Increment a released bead's failure count and quarantine it once the
    /// count reaches `outcome.quarantine_after_failures`.
    ///
    /// Returns whether the bead was quarantined; the quarantine events are
    /// appended to `events`. Store errors are logged and leave the bead
    /// released rather than failing the handler.
    async fn count_failure_toward_quarantine(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        events: &mut Vec<EventKind>,
    ) -> bool {
        let new_count = match self.increment_failure_count(store, bead).await {
            Ok(new_count) => new_count,
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "failed to increment failure count after release"
                );
                return false;
            }
        };
        let threshold = self.config.outcome.quarantine_after_failures;
        if threshold == 0 || new_count < threshold {
            return false;
        }
        match self
            .quarantine_bead(store, bead, new_count, threshold)
            .await
        {
            Ok(quarantine_events) => {
                events.extend(quarantine_events);
                true
            }
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "failed to quarantine bead after exceeding failure threshold"
                );
                false
            }
        }
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
        output: &AgentOutcome,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::warn!(bead_id = %bead.id, "agent timed out — releasing bead as deferred");

        self.capture_wip_patch(bead, output).await;
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
        output: &AgentOutcome,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::error!(
            bead_id = %bead.id,
            signal_code,
            agent = %self.config.agent.default,
            "agent crashed — releasing bead and creating alert"
        );

        self.capture_wip_patch(bead, output).await;
        let mut events = self.prepare_release_events(store, bead).await?;

        // A crash on this bead counts toward its quarantine ceiling exactly
        // like an ordinary failure, including the retry cooldown that the
        // count installs. An adapter-wide crash storm never reaches this
        // handler: judge_adapter_health routes it to
        // handle_infrastructure_failure once the fingerprint spans distinct
        // beads. Without the count, an agent that dies before doing any work
        // (exit=-1 in 0ms, GitHub #22) is re-claimed forever.
        let release_succeeded = !events
            .iter()
            .any(|e| matches!(e, EventKind::WorkerHandlingTimeout { .. }));
        let quarantined = release_succeeded
            && self
                .count_failure_toward_quarantine(store, bead, &mut events)
                .await;

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

        let action = if quarantined {
            BeadAction::Quarantined
        } else {
            BeadAction::Alerted
        };
        Ok((action, events))
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
        Ok((BeadAction::Released(ReleaseReason::AgentNotFound), events))
    }

    /// Interrupted: release bead for graceful shutdown.
    async fn handle_interrupted(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        output: &AgentOutcome,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::info!(bead_id = %bead.id, "agent interrupted — releasing bead for clean shutdown");

        self.capture_wip_patch(bead, output).await;
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
    pub(crate) async fn increment_failure_count(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<u32> {
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
    pub(crate) async fn reset_failure_count(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<()> {
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
    /// Emits the BeadQuarantined telemetry event. False-close telemetry is
    /// emitted at the reopen boundary in `handle_gate_failure`, so ordinary
    /// failed dispatches that reach quarantine are not mislabeled.
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

        let events = vec![EventKind::BeadQuarantined {
            bead_id: bead.id.clone(),
            round,
            until: until.to_rfc3339(),
            failure_count,
        }];

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
    use std::ops::{Deref, DerefMut};
    use std::path::PathBuf;
    use std::sync::Mutex;

    // ── Test environment isolation ──

    /// Pin `$HOME` to a private directory for the whole test body.
    ///
    /// Gate-health state and predispatch snapshots live under
    /// `$HOME/.needle/state`, so a test whose flow reaches
    /// `handle_gate_failure` or `handle_gate_error` reads and writes the
    /// *fleet's* state files unless HOME is private — the same failure that
    /// accumulated `degraded: true` on the former shared `/tmp` fixture. The
    /// guard serializes with every other test that
    /// swaps HOME; hold it for the whole body.
    fn isolated_home() -> (crate::util::test_env::EnvGuard, tempfile::TempDir) {
        let guard = crate::util::test_env::isolate_env();
        let home = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());
        (guard, home)
    }

    /// A bead fixture that owns its workspace for the fixture's full lifetime.
    ///
    /// Keeping the [`tempfile::TempDir`] here is important: returning only its
    /// path would remove the directory before the outcome handler used it,
    /// while constructing a synthetic name below `/tmp` would leak it and
    /// would not establish per-test ownership.
    struct TestBead {
        bead: Bead,
        _workspace: tempfile::TempDir,
    }

    impl Deref for TestBead {
        type Target = Bead;

        fn deref(&self) -> &Self::Target {
            &self.bead
        }
    }

    impl DerefMut for TestBead {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.bead
        }
    }

    // ── Mock BeadStore ──

    #[derive(Debug, Clone)]
    #[allow(dead_code)] // Fields read via pattern matching in test assertions
    enum StoreAction {
        Release(String),
        Close(String, String),
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
        /// Owns the workspace returned by `show()` for as long as the store
        /// can return beads that name it.
        workspace: tempfile::TempDir,
        /// Dependencies `show()` should report. Needed by any test whose
        /// behaviour depends on the POST-dispatch bead rather than the one
        /// passed in: the handler re-reads state through `show()`, so a bead
        /// constructed in the test body is NOT what the gates see.
        dependencies: Vec<crate::types::BrDependency>,
        /// Notes the shipped-work gate should read. `Bead` carries no notes, so
        /// the gate fetches them through the store; with no predispatch snapshot
        /// a non-empty note is what makes the gate Pass.
        notes: Option<String>,
        /// Workspace-scoped records returned by `list_all()`.
        all_beads: Vec<Bead>,
        /// Close reason the store reports for the closed bead. Setting it
        /// also turns on `exposes_close_reason`, mirroring the real
        /// capability split (bead-rs exposes close reasons, bf does not).
        close_reason: Option<String>,
        /// Whether this fake backend can expose close reasons at all.
        exposes_close_reason: bool,
        /// Description `show()` reports for the bead (defaults to the
        /// `bead_in_workspace` fixture body).
        description: Option<String>,
        fail_flush: bool,
    }

    impl MockBeadStore {
        fn new(show_status: BeadStatus) -> Self {
            MockBeadStore {
                actions: Mutex::new(Vec::new()),
                show_status,
                labels: Vec::new(),
                workspace: tempfile::TempDir::new().unwrap(),
                dependencies: Vec::new(),
                notes: None,
                all_beads: Vec::new(),
                close_reason: None,
                exposes_close_reason: false,
                description: None,
                fail_flush: false,
            }
        }

        fn with_notes(mut self, notes: &str) -> Self {
            self.notes = Some(notes.to_string());
            self
        }

        fn with_labels(mut self, labels: Vec<String>) -> Self {
            self.labels = labels;
            self
        }

        fn with_all_beads(mut self, all_beads: Vec<Bead>) -> Self {
            self.all_beads = all_beads;
            self
        }

        /// Report this close reason through a backend that exposes close
        /// reasons (the close-evidence gate only applies to such backends).
        fn with_close_reason(mut self, reason: &str) -> Self {
            self.close_reason = Some(reason.to_string());
            self.exposes_close_reason = true;
            self
        }

        /// Make `show()` report this description for the bead.
        fn with_description(mut self, description: &str) -> Self {
            self.description = Some(description.to_string());
            self
        }

        fn actions(&self) -> Vec<StoreAction> {
            self.actions.lock().unwrap().clone()
        }
    }

    fn bead_in_workspace(status: BeadStatus, workspace: &std::path::Path) -> Bead {
        Bead {
            id: BeadId::from("needle-test"),
            title: "Test bead".to_string(),
            body: Some("Test body".to_string()),
            priority: 1,
            status,
            assignee: Some("worker-01".to_string()),
            labels: vec![],
            workspace: workspace.to_path_buf(),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn test_bead(status: BeadStatus) -> TestBead {
        let workspace = tempfile::TempDir::new().unwrap();
        let bead = bead_in_workspace(status, workspace.path());
        TestBead {
            bead,
            _workspace: workspace,
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
        let (_guard, _home) = isolated_home();
        let a = test_bead(BeadStatus::Open);
        let b = test_bead(BeadStatus::Open);
        let a_path = a.workspace.clone();
        let b_path = b.workspace.clone();

        assert!(a_path.is_dir(), "the fixture must own a live workspace");
        assert!(b_path.is_dir(), "the fixture must own a live workspace");
        assert_ne!(
            a_path, b_path,
            "each test bead needs its own gate-health state key"
        );
        assert_ne!(
            a_path,
            std::env::temp_dir(),
            "the temp root itself is a real path with a fleet-visible state key"
        );

        crate::gate_health::record_error(
            &a.workspace,
            "fixture-command".to_string(),
            "fixture-error".to_string(),
        )
        .unwrap();
        let state = crate::gate_health::load_state(&a.workspace)
            .unwrap()
            .expect("the synthetic HOME should contain the fixture's state");
        assert_eq!(state.workspace, a_path);

        drop(a);
        drop(b);
        assert!(
            !a_path.exists(),
            "dropping the fixture removes its workspace"
        );
        assert!(
            !b_path.exists(),
            "dropping the fixture removes its workspace"
        );
    }

    #[async_trait]
    impl BeadStore for MockBeadStore {
        async fn notes(&self, _id: &BeadId) -> Result<Option<String>> {
            Ok(self.notes.clone())
        }

        fn exposes_close_reason(&self) -> bool {
            self.exposes_close_reason
        }

        async fn close_reason(&self, _id: &BeadId) -> Result<Option<String>> {
            Ok(self.close_reason.clone())
        }

        async fn close(&self, id: &BeadId, reason: &str) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::Close(id.to_string(), reason.to_string()));
            Ok(())
        }

        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.all_beads.clone())
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::Show(id.to_string()));
            let mut bead = bead_in_workspace(self.show_status.clone(), self.workspace.path());
            bead.labels = self.labels.clone();
            bead.dependencies = self.dependencies.clone();
            if let Some(description) = &self.description {
                bead.body = Some(description.clone());
            }
            Ok(bead)
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

    #[tokio::test]
    async fn restore_degraded_workspace_closes_bead_rs_null_source_repo_alert() {
        let (_guard, _home) = isolated_home();
        let workspace = tempfile::TempDir::new().unwrap();

        // bead-rs persists workspace-native issues with source_repo IS NULL;
        // its CLI omits that null field, so Bead's serde default is an empty
        // path. Store scoping, rather than this provenance field, establishes
        // that the alert belongs to `workspace`.
        let mut alert = bead_in_workspace(BeadStatus::Open, std::path::Path::new(""));
        alert.id = BeadId::from("needle-gate-broken");
        alert.title = "Gate broken: cargo test — command failed".to_string();
        assert!(alert.workspace.as_os_str().is_empty());

        let store = MockBeadStore::new(BeadStatus::Done).with_all_beads(vec![alert]);
        let helper = crate::telemetry::test_utils::TestHelper::new("gate-restore-test");
        let handler = OutcomeHandler::new(Config::default(), helper.telemetry().clone());
        let success_bead_id = BeadId::from("needle-success");

        handler
            .restore_degraded_workspace(
                &store,
                workspace.path(),
                &success_bead_id,
                GateResolutionTelemetry::none(),
            )
            .await
            .unwrap();
        helper.sync().await;

        let actions = store.actions();
        assert!(actions.iter().any(|action| {
            matches!(
                action,
                StoreAction::Close(id, reason)
                    if id == "needle-gate-broken" && reason.contains("needle-success")
            )
        }));

        let restored = helper.events_by_type("workspace.gate_restored");
        assert_eq!(restored.len(), 1, "restoration emits one aggregate event");
        assert_eq!(restored[0].bead_id.as_ref(), Some(&success_bead_id));
        assert_eq!(
            restored[0].data["workspace"],
            workspace.path().to_string_lossy().as_ref()
        );
    }

    #[tokio::test]
    async fn restore_degraded_workspace_emits_aggregate_event_with_no_alert_bead() {
        let (_guard, _home) = isolated_home();
        let workspace = tempfile::TempDir::new().unwrap();
        let store = MockBeadStore::new(BeadStatus::Done);
        let helper = crate::telemetry::test_utils::TestHelper::new("gate-restore-empty-test");
        let handler = OutcomeHandler::new(Config::default(), helper.telemetry().clone());
        let success_bead_id = BeadId::from("needle-success-without-alert");

        handler
            .restore_degraded_workspace(
                &store,
                workspace.path(),
                &success_bead_id,
                GateResolutionTelemetry::none(),
            )
            .await
            .unwrap();
        helper.sync().await;

        assert!(
            store
                .actions()
                .iter()
                .all(|action| !matches!(action, StoreAction::Close(_, _))),
            "an empty scoped store has no alert bead to close"
        );
        let restored = helper.events_by_type("workspace.gate_restored");
        assert_eq!(
            restored.len(),
            1,
            "restoration must remain observable when zero alert beads match"
        );
        assert_eq!(restored[0].bead_id.as_ref(), Some(&success_bead_id));
        assert_eq!(restored[0].data["degraded_duration_secs"], 0);
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

    fn signal_outcome(exit_code: i32) -> (Outcome, AgentOutcome) {
        (
            Outcome::Crash(exit_code),
            AgentOutcome {
                exit_code,
                stdout: String::new(),
                stderr: String::new(),
            },
        )
    }

    /// needle-f1efbb0a acceptance: a SIGTERM death (exit 143) is fleet
    /// machinery — a re-exec, a drain, an operator — and must never enter the
    /// provider-health window, or an upgrade storm fingerprinting as
    /// `signal:15` across distinct beads trips a false `provider.degraded`.
    #[test]
    fn crash_by_sigterm_is_not_an_adapter_health_signal() {
        let (outcome, output) = signal_outcome(143);
        assert_eq!(adapter_health_failure_reason(&outcome, &output, None), None);
    }

    /// Every other crash signal still fingerprints: SIGKILL and SEGV deaths
    /// are real adapter signals the detector must keep seeing.
    #[test]
    fn crash_by_other_signals_still_reach_adapter_health() {
        for (exit_code, expected) in [(137, "signal:9"), (139, "signal:11")] {
            let (outcome, output) = signal_outcome(exit_code);
            assert_eq!(
                adapter_health_failure_reason(&outcome, &output, None),
                Some(expected.to_string())
            );
        }
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

    #[test]
    fn classify_never_started_negative_exit_is_not_work_attributable() {
        // A failed spawn is reported as -1 with a zero-duration dispatch. It
        // must retain its infrastructure classification even when the caller
        // has no successful verification verdict to report.
        let outcome = classify_with_stream(-1, false, false, "");
        assert_eq!(outcome, Outcome::Crash(-1));
        assert!(!outcome.is_work_attributable());
    }

    #[test]
    fn classify_fast_normal_exit_failure_remains_work_attributable() {
        // Duration alone must not make a real process failure non-attributable:
        // a normal non-zero exit is still evidence from the agent.
        let outcome = classify_with_stream(1, false, false, "");
        assert_eq!(outcome, Outcome::Failure);
        assert!(outcome.is_work_attributable());
    }

    // ── handle tests ──

    /// Confirmed shipped work must CLOSE the bead, not release it.
    ///
    /// Releasing a bead whose work the gate has already verified puts it back on
    /// the ready frontier, where another worker claims and redoes it — the
    /// subscription pays twice for one unit of output while the fleet looks
    /// busy. It also emitted BeadCompleted and BeadReleased for the same bead,
    /// which is why completed and released counts overlapped and deliberate
    /// releases read as failures.
    ///
    /// Reaching the confirmed-shipped-work branch in a unit test uses the gate's
    /// documented conservative path: with no predispatch snapshot available it
    /// judges "the closure on its note alone", so a non-empty note Passes rather
    /// than failing a bead on a comparison the gate could not make. The mock
    /// provides no snapshot, so supplying notes is sufficient.
    #[tokio::test]
    async fn confirmed_shipped_work_closes_rather_than_releasing() {
        // Enforcement ON — this is the gate under test.
        let handler = test_handler();

        // The evidence must be on the STORE, not on the bead constructed here:
        // the gate reads notes through the store, and judges the POST-dispatch
        // bead the handler re-reads via show(). Setting either locally silently
        // has no effect and the gate sees a bead with no shipped work.
        let store = test_store(BeadStatus::Open)
            .with_notes("evidence: implemented and verified the change");

        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(
            result.bead_action,
            BeadAction::Closed,
            "shipped work confirmed by the gate must close the bead"
        );
        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Close(_, _))),
            "the store must be asked to close: {actions:?}"
        );
        assert!(
            !actions.iter().any(|a| matches!(a, StoreAction::Release(_))),
            "a finished bead must not be returned to the ready frontier: {actions:?}"
        );
    }

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

    // ── close-evidence verification: re-run what the close reason claims ──

    /// A handler judging closes through a fake runner and a pre-made
    /// "extraction" directory, so no real child and no git checkout is
    /// involved. Shipped-work enforcement is off: the mock store has no
    /// predispatch snapshot and the close-evidence gate must be judged on
    /// its own here.
    fn close_evidence_handler(
        helper: &crate::telemetry::test_utils::TestHelper,
        runner: Arc<crate::process_runner::FakeProcessRunner>,
    ) -> (OutcomeHandler, tempfile::TempDir) {
        use crate::process_runner::ProcessRunner;

        let extraction = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        config.validation.default_gates.enabled = false;
        config.validation.fallback_gate = false;
        let mut handler = OutcomeHandler::new(config, helper.telemetry().clone());
        handler.close_verification = close_verification::CloseVerificationRuntime::for_tests(
            runner as Arc<dyn ProcessRunner>,
            Some(extraction.path().to_path_buf()),
        );
        (handler, extraction)
    }

    #[tokio::test]
    async fn close_without_verified_block_is_reopened_and_released() {
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("close-evidence-missing");
        let (handler, _extraction) = close_evidence_handler(
            &helper,
            Arc::new(crate::process_runner::FakeProcessRunner::new()),
        );
        let store =
            test_store(BeadStatus::Done).with_close_reason("did the work, everything is green");
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert!(
            matches!(result.bead_action, BeadAction::Released(_)),
            "a close without evidence must be released, got {:?}",
            result.bead_action
        );
        helper.sync().await;
        let failed = helper.events_by_type("verification.failed");
        assert_eq!(failed.len(), 1, "the rejection must be reported");
        assert_eq!(
            failed[0].data["output"],
            close_verification::MISSING_EVIDENCE_REASON,
            "the rejection reason must say the close carried no evidence"
        );
        assert!(
            store
                .actions()
                .iter()
                .any(|a| matches!(a, StoreAction::Reopen(_))),
            "the closed bead must be reopened, got: {:?}",
            store.actions()
        );
    }

    #[tokio::test]
    async fn failing_claimed_command_reopens_with_the_command_in_the_reason() {
        use crate::process_runner::{FakeProcessRunner, ProcessOutput};

        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("close-evidence-failing");
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_output(ProcessOutput {
            success: false,
            exit_code: Some(1),
            stdout: b"running 3 tests\n".to_vec(),
            stderr: b"error: test failed, to rerun pass `mod::test`\n".to_vec(),
        });
        let (handler, _extraction) = close_evidence_handler(&helper, runner);
        let store = test_store(BeadStatus::Done).with_close_reason(
            "implemented it\n```verified:\ngo test ./internal/crypto/ exit=0\n```",
        );
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert!(matches!(result.bead_action, BeadAction::Released(_)));
        helper.sync().await;
        let failed = helper.events_by_type("verification.failed");
        assert_eq!(failed.len(), 1);
        let reason = failed[0].data["output"].as_str().unwrap_or_default();
        assert!(
            reason.contains("go test ./internal/crypto/"),
            "the reason must name the failed command, got: {reason}"
        );
        assert!(
            reason.contains("error: test failed"),
            "the reason must carry the command's output, got: {reason}"
        );
        assert!(
            store
                .actions()
                .iter()
                .any(|a| matches!(a, StoreAction::Reopen(_))),
            "the closed bead must be reopened"
        );
    }

    #[tokio::test]
    async fn passing_claimed_command_honours_the_close() {
        use crate::process_runner::{FakeProcessRunner, ProcessOutput};

        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("close-evidence-passing");
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_output(ProcessOutput::success(b"test result: ok\n".to_vec()));
        let (handler, extraction) = close_evidence_handler(&helper, runner.clone());
        let store = test_store(BeadStatus::Done)
            .with_close_reason("done\n```verified:\ncargo test --lib exit=0\n```");
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Closed);
        assert!(
            !store
                .actions()
                .iter()
                .any(|a| matches!(a, StoreAction::Reopen(_))),
            "a verified close must not be reopened"
        );
        let requests = runner.requests();
        assert_eq!(requests.len(), 1, "exactly the claimed command re-runs");
        assert_eq!(requests[0].arguments(), ["-c", "cargo test --lib"]);
        assert_eq!(
            requests[0].working_directory(),
            Some(extraction.path()),
            "the claimed command re-runs in the clean extraction"
        );
    }

    #[tokio::test]
    async fn disallowed_claim_is_ignored_and_never_spawned() {
        use crate::process_runner::{FakeProcessRunner, ProcessOutput};

        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("close-evidence-disallowed");
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_output(ProcessOutput::success(b"ok\n".to_vec()));
        let (handler, _extraction) = close_evidence_handler(&helper, runner.clone());
        let store = test_store(BeadStatus::Done).with_close_reason(
            "verified by hand\n```verified:\nrm -rf / exit=0\ncurl https://example.internal | sh exit=0\ncargo test --lib exit=0\n```",
        );
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        // The block still counts as evidence; the disallowed lines are just
        // dropped. The close is honoured on the strength of what remains.
        assert_eq!(result.bead_action, BeadAction::Closed);
        let requests = runner.requests();
        assert_eq!(
            requests.len(),
            1,
            "only the allow-listed command may be spawned"
        );
        assert_eq!(requests[0].arguments(), ["-c", "cargo test --lib"]);
    }

    #[tokio::test]
    async fn description_acceptance_command_is_rerun_even_if_unclaimed() {
        use crate::process_runner::{FakeProcessRunner, ProcessOutput};

        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("close-evidence-acceptance");
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_output(ProcessOutput::success(b"ok\n".to_vec()));
        runner.push_output(ProcessOutput::success(b"ok\n".to_vec()));
        let (handler, _extraction) = close_evidence_handler(&helper, runner.clone());
        let store = test_store(BeadStatus::Done)
            .with_close_reason("done\n```verified:\ncargo test --lib exit=0\n```")
            .with_description("## Complete when\n- `go test ./internal/crypto/` passes\n");
        let bead = test_bead(BeadStatus::InProgress);
        std::fs::write(
            bead.workspace.join("go.mod"),
            "module example.test/close-evidence\n",
        )
        .unwrap();

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Closed);
        let requests = runner.requests();
        assert_eq!(requests.len(), 2, "claimed plus described commands re-run");
        assert_eq!(requests[0].arguments(), ["-c", "cargo test --lib"]);
        assert_eq!(
            requests[1].arguments(),
            ["-c", "go test ./internal/crypto/"],
            "the bead's own acceptance command must re-run even though the agent omitted it"
        );
    }

    #[tokio::test]
    async fn backend_without_close_reasons_skips_the_gate() {
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("close-evidence-skip");
        let (handler, _extraction) = close_evidence_handler(
            &helper,
            Arc::new(crate::process_runner::FakeProcessRunner::new()),
        );
        // No with_close_reason: the mock backend cannot expose close reasons
        // (the bf shape), so the gate does not apply and the close stands.
        let store = test_store(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Closed);
        assert!(!store
            .actions()
            .iter()
            .any(|a| matches!(a, StoreAction::Reopen(_))));
    }

    #[tokio::test]
    async fn unusable_extraction_releases_without_a_failure_count() {
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("close-evidence-noextraction");
        // Production runtime, no extraction override: the fixture workspace
        // is not a git repository, so extracting committed state must fail.
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        let store = test_store(BeadStatus::Done)
            .with_close_reason("done\n```verified:\ncargo test --lib exit=0\n```");
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert!(
            matches!(result.bead_action, BeadAction::Released(_)),
            "no extraction means no verdict, got {:?}",
            result.bead_action
        );
        assert!(
            !store.actions().iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, l) if l.starts_with("failure-count:"))
            ),
            "an execution error is not the bead's failure — no failure-count increment"
        );
        assert!(
            !store
                .actions()
                .iter()
                .any(|a| matches!(a, StoreAction::Reopen(_))),
            "the bead was never closed in this store's view; no reopen expected"
        );
    }

    #[tokio::test]
    async fn documented_terminal_outcomes_emit_classified_and_handled_events() {
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("outcome-telemetry-contract");
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        let cases = [
            ("success", 0, false),
            ("failure", 1, false),
            ("timeout", 124, false),
            ("agent_not_found", 127, false),
            ("crash", 137, false),
            ("interrupted", 130, true),
        ];
        let mut failure_actions = None;

        for (index, (expected_outcome, exit_code, interrupted)) in cases.iter().enumerate() {
            let status = if *expected_outcome == "success" {
                BeadStatus::Done
            } else {
                BeadStatus::InProgress
            };
            let store = MockBeadStore::new(status);
            let mut bead = test_bead(BeadStatus::InProgress);
            bead.bead.id = BeadId::from(format!("needle-outcome-{index}"));

            let result = handler
                .handle(&store, &bead, &test_output(*exit_code), *interrupted)
                .await
                .unwrap();
            assert_eq!(
                result.outcome.as_str(),
                *expected_outcome,
                "fixture should exercise the documented {expected_outcome} outcome"
            );
            if *expected_outcome == "failure" {
                assert!(matches!(result.bead_action, BeadAction::Released(_)));
                failure_actions = Some(store.actions());
            }
            helper.sync().await;
        }

        let classified = helper.events_by_type("outcome.classified");
        let handled = helper.events_by_type("outcome.handled");
        assert_eq!(classified.len(), cases.len());
        assert_eq!(handled.len(), cases.len());
        for ((expected_outcome, _, _), event) in cases.iter().zip(classified.iter()) {
            assert_eq!(event.data["outcome"], *expected_outcome);
        }
        for ((expected_outcome, _, _), event) in cases.iter().zip(handled.iter()) {
            assert_eq!(event.data["outcome"], *expected_outcome);
            assert!(
                event.data["action"].as_str().is_some(),
                "terminal handling must record its action"
            );
        }
        let actions = failure_actions.expect("failure case ran");
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

        assert!(matches!(result.bead_action, BeadAction::Released(_)));
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

        assert!(matches!(result.bead_action, BeadAction::Released(_)));
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

        assert!(matches!(result.bead_action, BeadAction::Released(_)));
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

        for prior_count in 0..3 {
            let labels = (prior_count > 0)
                .then(|| format!("failure-count:{prior_count}"))
                .into_iter()
                .collect();
            let store = MockBeadStore::new(BeadStatus::Done).with_labels(labels);
            let bead = test_bead(BeadStatus::InProgress);
            let result = handler
                .handle(&store, &bead, &test_output(0), false)
                .await
                .unwrap();

            assert!(result.telemetry_events.iter().any(|event| matches!(
                event,
                EventKind::FalseCloseDetected {
                    class: FalseCloseClass::DeliverableBlocked,
                    ..
                }
            )));
            if prior_count == 2 {
                assert_eq!(result.bead_action, BeadAction::Quarantined);
                assert!(store.actions().iter().any(
                    |action| matches!(action, StoreAction::AddLabel(_, label) if label == "failure-count:3")
                ));
                assert!(store.actions().iter().any(
                    |action| matches!(action, StoreAction::AddLabel(_, label) if label == "quarantined")
                ));
            } else {
                assert!(matches!(result.bead_action, BeadAction::Released(_)));
            }
        }
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
        assert!(matches!(result.bead_action, BeadAction::Released(_)));
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

        // GitHub #22: an agent that dies before doing any work must still
        // count toward the bead's quarantine ceiling, or it is re-claimed
        // forever.
        assert!(
            store.actions().iter().any(|action| matches!(
                action,
                StoreAction::AddLabel(_, label) if label.starts_with("failure-count:")
            )),
            "a crash must increment the bead's failure count"
        );
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
        assert!(matches!(result.bead_action, BeadAction::Released(_)));

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
    async fn handle_failure_records_the_attempt_for_the_next_prompt() {
        // R3 (needle-60163eac): a failed attempt leaves a bounded record the
        // next dispatch prompt renders — outcome, reason and the failure text.
        let handler = test_handler();
        let store = test_store(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);
        let output = AgentOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: "error[E0308]: mismatched types\n --> src/lib.rs:3:5\n".to_string(),
        };

        let first = handler.handle(&store, &bead, &output, false).await.unwrap();
        assert!(matches!(first.bead_action, BeadAction::Released(_)));

        let records = crate::attempt_history::load_local(&bead.workspace, &bead.id).unwrap();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.outcome, "work_failure");
        assert_eq!(record.terminal_reason.as_deref(), Some("exit_code:1"));
        assert_eq!(record.exit_code, 1);
        // BeadAction::Released now carries WHY, and Display renders it as
        // released:<reason>, so the attempt history tells the next agent what
        // sent the bead back. Nothing parses this field -- it is emitted to
        // telemetry and rendered into the prompt -- so the richer value is a
        // gain, not a format break.
        assert_eq!(record.requested_action, "released:dispatch_failed");
        let summary = record.failure_summary.as_deref().unwrap();
        assert!(summary.contains("error[E0308]"), "{summary}");
        assert!(summary.contains("stderr (tail)"));

        // The rendered history names the failure so the next agent reads it.
        let rendered = crate::attempt_history::render(
            &records,
            crate::attempt_history::HistoryLimits::default(),
        );
        assert!(rendered.contains("## Previous attempts on this bead"));
        assert!(rendered.contains("error[E0308]"));

        // A second failure appends rather than replaces.
        let second = handler.handle(&store, &bead, &output, false).await.unwrap();
        assert!(matches!(second.bead_action, BeadAction::Released(_)));
        assert_eq!(
            crate::attempt_history::load_local(&bead.workspace, &bead.id)
                .unwrap()
                .len(),
            2
        );
        let _ = std::fs::remove_dir_all(&bead.workspace);
    }

    #[tokio::test]
    async fn adapter_failure_storm_resolves_as_infrastructure_without_penalty() {
        // N-T23: four distinct beads failing with the same infrastructure-shaped
        // signal on one adapter trip the detector; the tripping failure and
        // every later one with that fingerprint release the bead with no
        // failure count and resolve as infrastructure_failure.
        //
        // The detector's window is persisted per adapter under
        // $HOME/.needle/state/provider-health (provider_health::state_dir), so
        // this test READS $HOME even though it never sets it. Other tests
        // remove HOME process-globally under the ENV_LOCK guard; without
        // taking that guard here, such a test can land mid-sequence, send
        // these four failures to a different state file, and leave the window
        // too short to trip. That is precisely how this passed locally at
        // --test-threads=4 and failed in CI at full parallelism.
        let (_env_guard, _home) = isolated_home();
        let adapter = format!(
            "test-storm-adapter-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let mut config = Config::default();
        config.workspace_health.fingerprint_min_window_failures = 4;
        config.workspace_health.fingerprint_min_distinct_beads = 3;
        let handler = test_handler_with_config(config);
        let store = test_store(BeadStatus::InProgress);
        // The 2026-09-02 zai-proxy outage shape: the CLI exits 0 but its
        // result envelope reports a terminal API error.
        let api_outage = AgentOutcome {
            exit_code: 0,
            stdout: "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}\n\
                     {\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"api_error_status\":503,\"terminal_reason\":\"api_error\",\"result\":\"API Error: 503 no available server\",\"session_id\":\"s1\"}\n"
                .to_string(),
            stderr: String::new(),
        };

        let mut last = None;
        for n in 0..4 {
            let mut bead = test_bead(BeadStatus::InProgress);
            bead.id = BeadId::from(format!("needle-storm-{n}").as_str());
            handler.set_attempt_context(AttemptContext {
                adapter: adapter.clone(),
                ..AttemptContext::default()
            });
            last = Some(
                handler
                    .handle(&store, &bead, &api_outage, false)
                    .await
                    .unwrap(),
            );
        }
        let tripping = last.unwrap();
        assert!(matches!(
            tripping.bead_action,
            BeadAction::Released(ReleaseReason::InfrastructureFailure)
        ));
        assert!(
            tripping.telemetry_events.iter().any(|e| matches!(
                e,
                EventKind::BeadReleased { reason, .. } if reason.starts_with("infrastructure:")
            )),
            "{:?}",
            tripping.telemetry_events
        );
        // The first three failures were judged as the beads' own (label added);
        // the tripping one was not penalised.
        let label_adds = store
            .actions()
            .iter()
            .filter(|a| matches!(a, StoreAction::AddLabel(_, l) if l.starts_with("failure-count:")))
            .count();
        assert_eq!(label_adds, 3, "{:?}", store.actions());
        let state = crate::provider_health::degraded_state(&adapter, None)
            .unwrap()
            .expect("adapter degraded");
        assert!(state.degraded_fingerprint.is_some());

        // A verified success on the adapter restores it.
        let mut ok_bead = test_bead(BeadStatus::InProgress);
        ok_bead.id = BeadId::from("needle-storm-ok");
        let ok_store = test_store(BeadStatus::Done);
        let ok_handler = test_handler_without_shipped_work();
        ok_handler.set_attempt_context(AttemptContext {
            adapter: adapter.clone(),
            ..AttemptContext::default()
        });
        let ok = ok_handler
            .handle(&ok_store, &ok_bead, &test_output(0), false)
            .await
            .unwrap();
        assert_eq!(ok.outcome, Outcome::Success);
        assert!(crate::provider_health::degraded_state(&adapter, None)
            .unwrap()
            .is_none());
        crate::provider_health::clear_state(&adapter, None).unwrap();
    }

    #[tokio::test]
    async fn generic_exit_one_storm_is_not_applied_as_a_bead_failure() {
        // A Codex/CLI invocation can report both task failures and dispatch
        // failures as exit 1. The first attempts remain bead-scoped; once the
        // same adapter emits exit 1 across unrelated beads, provider health
        // must release the tripping attempt without another quarantine count.
        // This is the regression for the uniform failure-count wave observed
        // on 2026-09-21, where no validation gate ran for the non-zero exits.
        let (_env_guard, _home) = isolated_home();
        let adapter = format!(
            "test-exit-one-storm-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let mut config = Config::default();
        config.workspace_health.fingerprint_min_window_failures = 4;
        config.workspace_health.fingerprint_min_distinct_beads = 3;
        let handler = test_handler_with_config(config);
        let store = test_store(BeadStatus::InProgress);

        for n in 0..4 {
            let mut bead = test_bead(BeadStatus::InProgress);
            bead.id = BeadId::from(format!("needle-exit-one-{n}").as_str());
            handler.set_attempt_context(AttemptContext {
                adapter: adapter.clone(),
                ..AttemptContext::default()
            });
            let result = handler
                .handle(&store, &bead, &test_output(1), false)
                .await
                .unwrap();

            if n < 3 {
                assert_eq!(result.outcome, Outcome::Failure);
                assert!(matches!(result.bead_action, BeadAction::Released(_)));
            } else {
                assert_eq!(result.outcome, Outcome::Failure);
                assert_eq!(
                    result.bead_action,
                    BeadAction::Released(ReleaseReason::InfrastructureFailure)
                );
            }
        }

        let failure_labels = store
            .actions()
            .iter()
            .filter(|action| matches!(action, StoreAction::AddLabel(_, label) if label.starts_with("failure-count:")))
            .count();
        assert_eq!(
            failure_labels, 3,
            "the aggregate detector must stop the fourth exit-1 penalty"
        );
        assert!(
            store.actions().iter().all(|action| {
                !matches!(action, StoreAction::AddLabel(_, label) if label == "verification-failed")
            }),
            "non-zero dispatch failures have no shared gate verdict"
        );

        crate::provider_health::clear_state(&adapter, None).unwrap();
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
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
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
            let mut bead = test_bead(BeadStatus::InProgress);
            bead.workspace = workspace;
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
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
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

    // ── the no-verifier count (plan 4.4 step 6, needle-66b015d6) ──
    //
    // A dispatch whose workspace resolves no verifier still passes — there
    // is nothing to fail it on — but it must never be silently waved
    // through: every such dispatch WARNs and emits `gate.no_verifier` with
    // the reason nothing ran.

    async fn no_verifier_outcome_for(
        handler: &OutcomeHandler,
        workspace: &tempfile::TempDir,
    ) -> HandlerResult {
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);
        handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap()
    }

    async fn assert_one_no_verifier_event(
        helper: &crate::telemetry::test_utils::TestHelper,
        reason: &str,
    ) {
        helper.sync().await;
        let events = helper.events_by_type("gate.no_verifier");
        assert_eq!(events.len(), 1, "expected exactly one gate.no_verifier");
        assert_eq!(
            events[0].data["reason"], reason,
            "the event must say why nothing ran"
        );
    }

    #[tokio::test]
    async fn workspace_with_no_verifier_passes_and_is_counted() {
        // An empty workspace — no `.needle.yaml`, no build file — resolves
        // no verifier: the dispatch passes on the exit code alone and the
        // gap is counted as `not_detected`.
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("no-verifier-test");
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let handler = OutcomeHandler::new(config, helper.telemetry().clone());

        let workspace = tempfile::TempDir::new().unwrap();
        let result = no_verifier_outcome_for(&handler, &workspace).await;

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        assert_one_no_verifier_event(&helper, "not_detected").await;
    }

    #[tokio::test]
    async fn explicit_empty_gates_remain_untouched() {
        // `gates: []` is an existing workspace-level opt-out. This fallback
        // only applies when neither gate format is declared, so the explicit
        // opt-out remains a plain successful dispatch with no fallback event.
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("no-verifier-empty-test");
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let handler = OutcomeHandler::new(config, helper.telemetry().clone());

        let workspace = tempfile::TempDir::new().unwrap();
        std::fs::write(workspace.path().join(".needle.yaml"), "gates: []\n").unwrap();
        let result = no_verifier_outcome_for(&handler, &workspace).await;

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        helper.sync().await;
        assert!(helper.events_by_type("gate.no_verifier").is_empty());
    }

    // ── the fallback gate (needle-66b015d6 part 2) ──
    //
    // A workspace that declares neither `gates:` nor `verification:`, opted
    // out of nothing, and whose files select a verifier — or select nothing,
    // in which case the tree the clean extraction is cut from must be clean.

    /// Commit `files` in a fresh git repo so a dispatch against it has a
    /// committed state to extract and a tree that `git status` can judge.
    fn committed_git_workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, contents) in files {
            let path = dir.path().join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.email=needle@example.test"])
                .args(["-c", "user.name=needle-test"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap()
        };
        let init = git(&["init", "-q"]);
        assert!(
            init.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        let add = git(&["add", "-A"]);
        assert!(
            add.status.success(),
            "git add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        let commit = git(&["commit", "-q", "-m", "fixture"]);
        assert!(
            commit.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
        dir
    }

    /// A handler whose fallback runtime runs in the given extraction seam
    /// with the given runner, so wiring tests need no real child and no real
    /// git archive.
    fn fallback_handler_with(
        helper: &crate::telemetry::test_utils::TestHelper,
        runner: Arc<dyn crate::process_runner::ProcessRunner>,
        extraction_dir: Option<std::path::PathBuf>,
    ) -> OutcomeHandler {
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let mut handler = OutcomeHandler::new(config, helper.telemetry().clone());
        handler.fallback_verification =
            fallback_verification::FallbackVerificationRuntime::for_tests(runner, extraction_dir);
        handler
    }

    #[tokio::test]
    async fn failing_fallback_verifier_reopens_and_releases_with_verification_failed() {
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("fallback-verifier-fails");
        let runner = Arc::new(crate::process_runner::FakeProcessRunner::new());
        runner.push_output(crate::process_runner::ProcessOutput {
            success: false,
            exit_code: Some(3),
            stdout: b"running\n".to_vec(),
            stderr: b"definition of done failed\n".to_vec(),
        });
        let extraction = tempfile::tempdir().unwrap();
        let handler = fallback_handler_with(&helper, runner, Some(extraction.path().to_path_buf()));

        // A Python-marker workspace: `default_gates::detect` declines (no
        // builtin for Python without host config), so the fallback picks
        // `pytest -q` — which the fake runner fails.
        let workspace = tempfile::TempDir::new().unwrap();
        std::fs::write(workspace.path().join("pyproject.toml"), "[project]\n").unwrap();
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failure);
        assert!(
            matches!(result.bead_action, BeadAction::Released(_)),
            "a failed fallback verifier must release the bead, got {:?}",
            result.bead_action
        );
        assert!(
            store
                .actions()
                .iter()
                .any(|a| matches!(a, StoreAction::Reopen(_))),
            "the bead must be reopened, got: {:?}",
            store.actions()
        );
        assert!(
            store.actions().iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "verification-failed")
            ),
            "the release must carry the verification-failed label, got: {:?}",
            store.actions()
        );
        helper.sync().await;
        let failed = helper.events_by_type("verification.failed");
        assert_eq!(failed.len(), 1, "the failure must be reported");
        assert_eq!(
            failed[0].data["command"], "fallback_python",
            "the report must name the fallback gate"
        );
    }

    #[tokio::test]
    async fn gate_less_workspace_with_clean_tree_passes_and_is_counted() {
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("fallback-clean-tree");
        let handler = fallback_handler_with(
            &helper,
            Arc::new(crate::process_runner::FakeProcessRunner::new()),
            None,
        );

        // Everything committed, no markers anywhere: nothing to run, and the
        // extraction would carry the whole dispatch — pass, counted.
        let workspace = committed_git_workspace(&[("README.md", "docs only\n")]);
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        assert_one_no_verifier_event(&helper, "not_detected").await;
        assert!(
            helper.events_by_type("verification.failed").is_empty(),
            "a clean pass must not be reported as a verification failure"
        );
    }

    #[tokio::test]
    async fn gate_less_workspace_with_dirty_tree_fails() {
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("fallback-dirty-tree");
        let handler = fallback_handler_with(
            &helper,
            Arc::new(crate::process_runner::FakeProcessRunner::new()),
            None,
        );

        let workspace = committed_git_workspace(&[("README.md", "docs only\n")]);
        std::fs::write(workspace.path().join("uncommitted.md"), "never committed\n").unwrap();
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failure);
        assert!(
            matches!(result.bead_action, BeadAction::Released(_)),
            "a dirty tree must release the bead, got {:?}",
            result.bead_action
        );
        assert!(
            store
                .actions()
                .iter()
                .any(|a| matches!(a, StoreAction::Reopen(_))),
            "the bead must be reopened, got: {:?}",
            store.actions()
        );
        helper.sync().await;
        let failed = helper.events_by_type("verification.failed");
        assert_eq!(failed.len(), 1);
        assert_eq!(
            failed[0].data["command"], "fallback_clean_tree",
            "the report must name the clean-tree gate"
        );
        assert!(
            failed[0].data["output"]
                .as_str()
                .is_some_and(|output| output.contains("uncommitted.md")),
            "the failure must name the dirty path: {:?}",
            failed[0].data["output"]
        );
    }

    #[tokio::test]
    async fn fallback_verifier_spawn_failure_takes_the_gate_error_route() {
        // A verifier that cannot run is not a verdict: release without the
        // verification-failed label and without a failure-count increment
        // (needle-4aaa010c).
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("fallback-spawn-error");
        let runner = Arc::new(crate::process_runner::FakeProcessRunner::new());
        runner.push_error("spawn failed");
        let extraction = tempfile::tempdir().unwrap();
        let handler = fallback_handler_with(&helper, runner, Some(extraction.path().to_path_buf()));

        let workspace = tempfile::TempDir::new().unwrap();
        std::fs::write(workspace.path().join("pytest.ini"), "[pytest]\n").unwrap();
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failure);
        assert!(
            matches!(
                result.bead_action,
                BeadAction::Released(ReleaseReason::AgentNotFound)
            ),
            "a could-not-run gate releases without a failure-count penalty (the \
             GateError route's release reason is AgentNotFound by name), got {:?}",
            result.bead_action
        );
        assert!(
            !store.actions().iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "verification-failed")
            ),
            "an unjudged dispatch must not carry the verification-failed label, got: {:?}",
            store.actions()
        );
    }

    #[tokio::test]
    async fn fallback_verifier_timeout_and_stderr_cap_are_enforced() {
        // Real runtime, real child, real extraction: a verifier that outruns
        // the standard gate timeout (`validation.outcome_timeout_seconds`) is
        // killed, and the run is a could-not-run GateError, not a verdict.
        //
        // `pytest.ini` selects `pytest -q` and no host pytest is depended on:
        // a stub `pytest` at the front of PATH hangs, so the only way this
        // dispatch terminates is the timeout. `pytest.ini` is invisible to
        // `default_gates::detect`, so the dispatch reaches the fallback gate
        // whichever way that module's marker list is shaped.
        let (_guard, _home) = isolated_home();
        let stub_dir = tempfile::tempdir().unwrap();
        let stub = stub_dir.path().join("pytest");
        std::fs::write(&stub, "#!/bin/sh\nexec sleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&stub).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).unwrap();
        let original_path = std::env::var_os("PATH").unwrap_or_default();
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}",
                stub_dir.path().display(),
                original_path.to_string_lossy()
            ),
        );

        let helper = crate::telemetry::test_utils::TestHelper::new("fallback-timeout");
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        config.validation.outcome_timeout_seconds = 1;
        let mut handler = OutcomeHandler::new(config, helper.telemetry().clone());
        handler.fallback_verification =
            fallback_verification::FallbackVerificationRuntime::production();

        let workspace = committed_git_workspace(&[("pytest.ini", "[pytest]\n")]);

        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failure);
        assert!(
            matches!(
                result.bead_action,
                BeadAction::Released(ReleaseReason::AgentNotFound)
            ),
            "a timed-out verifier could not produce a verdict — the GateError \
             route releases without a penalty, got {:?}",
            result.bead_action
        );
        helper.sync().await;
        let errors = helper.events_by_type("gate.execution_error");
        assert!(
            errors.iter().any(|e| e.data["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("timed out"))),
            "the gate execution error must say the verifier timed out: {:?}",
            errors.iter().map(|e| &e.data).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn gate_less_workspace_with_passing_verifier_closes() {
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("fallback-verifier-passes");
        let runner = Arc::new(crate::process_runner::FakeProcessRunner::new());
        runner.push_output(crate::process_runner::ProcessOutput::success(
            b"ok\n".to_vec(),
        ));
        let extraction = tempfile::tempdir().unwrap();
        let handler = fallback_handler_with(&helper, runner, Some(extraction.path().to_path_buf()));

        let workspace = tempfile::TempDir::new().unwrap();
        std::fs::write(workspace.path().join("pyproject.toml"), "[project]\n").unwrap();
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        assert!(
            helper.events_by_type("gate.no_verifier").is_empty(),
            "a workspace whose files select a verifier is verified, not counted as unverified"
        );
    }

    #[tokio::test]
    async fn explicit_opt_out_workspace_never_runs_the_fallback_gate() {
        // `gates: []` is the workspace's own opt-out: it passes on the exit
        // code alone even when its files would select a verifier.
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("fallback-opt-out");
        let handler = fallback_handler_with(
            &helper,
            Arc::new(crate::process_runner::FakeProcessRunner::new()),
            None,
        );

        let workspace = committed_git_workspace(&[
            (".needle.yaml", "gates: []\n"),
            ("pyproject.toml", "[project]\n"),
        ]);
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        helper.sync().await;
        assert!(helper.events_by_type("gate.no_verifier").is_empty());
    }

    /// Run `future` under a capturing tracing subscriber and return its
    /// output plus everything logged while it ran (needle-66b015d6 part 3:
    /// the armed/opted-out fallback-gate decision must be visible in the
    /// dispatch logs, so the wiring tests assert on the log lines).
    ///
    /// `#[tokio::test]` polls on the calling thread, so the thread-local
    /// default subscriber covers everything the future logs.
    async fn with_captured_logs<F>(future: F) -> (F::Output, String)
    where
        F: std::future::Future,
    {
        use std::io::Write;

        #[derive(Clone, Default)]
        struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

        struct CapturedLogWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

        impl Write for CapturedLogWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
            type Writer = CapturedLogWriter;

            fn make_writer(&'a self) -> Self::Writer {
                CapturedLogWriter(self.0.clone())
            }
        }

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .without_time()
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let output = future.await;
        drop(_guard);
        let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        (output, logs)
    }

    // ── validation.fallback_gate opt-out (needle-66b015d6 part 3) ──

    #[tokio::test]
    async fn fallback_gate_false_workspace_skips_builtin_gate_and_logs() {
        // `validation.fallback_gate: false` opts a gate-less workspace out of
        // the built-in gate. The fake runner has NO queued response, so a
        // verifier that ran would bail and fail the dispatch — closing clean
        // proves the gate never ran, and the captured log shows why.
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("fallback-gate-opt-out");
        let runner = Arc::new(crate::process_runner::FakeProcessRunner::new());
        let handler = fallback_handler_with(&helper, runner.clone(), None);

        let workspace = tempfile::TempDir::new().unwrap();
        std::fs::write(
            workspace.path().join(".needle.yaml"),
            "validation:\n  fallback_gate: false\n",
        )
        .unwrap();
        std::fs::write(workspace.path().join("pyproject.toml"), "[project]\n").unwrap();
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);

        let (result, logs) =
            with_captured_logs(handler.handle(&store, &bead, &test_output(0), false)).await;
        let result = result.unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        assert!(
            runner.requests().is_empty(),
            "the opted-out workspace must not run any fallback verifier"
        );
        helper.sync().await;
        assert!(helper.events_by_type("gate.no_verifier").is_empty());
        assert!(
            logs.contains("validation.fallback_gate is false"),
            "the opt-out must be visible in the dispatch logs. Got: {logs}"
        );
    }

    #[tokio::test]
    async fn host_fallback_gate_false_skips_builtin_gate_for_unguarded_workspaces() {
        // The host-level default arms the gate; turning it off covers every
        // workspace that does not set the key for itself.
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("fallback-host-off");
        let runner = Arc::new(crate::process_runner::FakeProcessRunner::new());
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        config.validation.fallback_gate = false;
        let mut handler = OutcomeHandler::new(config, helper.telemetry().clone());
        handler.fallback_verification =
            fallback_verification::FallbackVerificationRuntime::for_tests(runner.clone(), None);

        let workspace = tempfile::TempDir::new().unwrap();
        std::fs::write(workspace.path().join("pyproject.toml"), "[project]\n").unwrap();
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);

        let (result, logs) =
            with_captured_logs(handler.handle(&store, &bead, &test_output(0), false)).await;
        let result = result.unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        assert!(runner.requests().is_empty());
        assert!(
            logs.contains("validation.fallback_gate is false"),
            "the host-level opt-out must log per dispatch. Got: {logs}"
        );
    }

    #[tokio::test]
    async fn fallback_gate_armed_is_logged_and_verifier_runs() {
        // Absent key + default host config => the gate is armed: the dispatch
        // log says so and the workspace's verifier actually runs.
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("fallback-armed-log");
        let runner = Arc::new(crate::process_runner::FakeProcessRunner::new());
        runner.push_output(crate::process_runner::ProcessOutput::success(
            b"ok\n".to_vec(),
        ));
        let extraction = tempfile::tempdir().unwrap();
        let handler = fallback_handler_with(
            &helper,
            runner.clone(),
            Some(extraction.path().to_path_buf()),
        );

        let workspace = tempfile::TempDir::new().unwrap();
        std::fs::write(workspace.path().join("pyproject.toml"), "[project]\n").unwrap();
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);

        let (result, logs) =
            with_captured_logs(handler.handle(&store, &bead, &test_output(0), false)).await;
        let result = result.unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        assert_eq!(
            runner.requests().len(),
            1,
            "the armed fallback gate must run the workspace's verifier"
        );
        assert!(
            logs.contains("built-in fallback gate armed"),
            "an armed gate must say so in the dispatch logs. Got: {logs}"
        );
        assert!(
            !logs.contains("validation.fallback_gate is false"),
            "an armed gate must not log an opt-out. Got: {logs}"
        );
    }

    #[tokio::test]
    async fn workspace_fallback_gate_true_beats_host_false() {
        // Per-workspace resolution wins over the host default in both
        // directions: a workspace that explicitly arms the gate keeps it
        // even when the host turned it off.
        let (_guard, _home) = isolated_home();
        let helper = crate::telemetry::test_utils::TestHelper::new("fallback-ws-true");
        let runner = Arc::new(crate::process_runner::FakeProcessRunner::new());
        runner.push_output(crate::process_runner::ProcessOutput::success(
            b"ok\n".to_vec(),
        ));
        let extraction = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        config.validation.fallback_gate = false;
        let mut handler = OutcomeHandler::new(config, helper.telemetry().clone());
        handler.fallback_verification =
            fallback_verification::FallbackVerificationRuntime::for_tests(
                runner.clone(),
                Some(extraction.path().to_path_buf()),
            );

        let workspace = tempfile::TempDir::new().unwrap();
        std::fs::write(
            workspace.path().join(".needle.yaml"),
            "validation:\n  fallback_gate: true\n",
        )
        .unwrap();
        std::fs::write(workspace.path().join("pyproject.toml"), "[project]\n").unwrap();
        let mut bead = test_bead(BeadStatus::InProgress);
        bead.workspace = workspace.path().to_path_buf();
        let store = test_store(BeadStatus::Done);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        assert_eq!(
            runner.requests().len(),
            1,
            "an explicitly armed workspace must run the verifier despite the host default"
        );
    }

    // ── timeout and resilience tests ──

    #[tokio::test(start_paused = true)]
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

        let started = tokio::time::Instant::now();
        let result = handler
            .handle(store.as_ref(), &bead, &test_output(1), false)
            .await;

        assert_eq!(started.elapsed(), std::time::Duration::from_secs(30));
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
        assert!(matches!(result.bead_action, BeadAction::Released(_)));
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

    #[tokio::test(start_paused = true)]
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

        let start = tokio::time::Instant::now();
        let result = handler
            .handle_with_cancellation(store.as_ref(), &bead, &test_output(0), false, cancelled)
            .await
            .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(elapsed, std::time::Duration::from_secs(1));
        assert_eq!(result.bead_action, BeadAction::Errored);
        assert!(result.telemetry_events.is_empty());
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
        assert!(matches!(result.bead_action, BeadAction::Released(_)));
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
                GateResolutionTelemetry::none(),
            )
            .await
            .unwrap();
        let result = HandlerResult {
            bead_action,
            telemetry_events,
            outcome: Outcome::GateError,
            budget_exhausted: false,
        };

        // The bead should be released
        assert!(matches!(result.bead_action, BeadAction::Released(_)));

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
        assert!(matches!(result.bead_action, BeadAction::Released(_)));

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

    /// Every emitted row satisfies the versioned fixture (v2 since N-T46; the
    /// helper's name predates the bump), and carries the
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
        assert_eq!(row.data["schema_version"], 2, "{label}: schema_version");
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

    /// N-T46 (ADR-030): the auto-split template's success is a decomposition,
    /// not a verified success. The process verdict and the bead action are
    /// unchanged; only the ledger's semantic outcome moves.
    #[tokio::test]
    async fn nt46_split_template_success_resolves_decomposed() {
        let (_guard, _home) = isolated_home();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let helper = crate::telemetry::test_utils::TestHelper::new("nt46-split-template");
        let handler = OutcomeHandler::new(config, helper.telemetry().clone());
        handler.set_attempt_context(AttemptContext {
            adapter: "claude-code-glm-5.3-flash".to_string(),
            prompt_template: crate::attempt_accounting::SPLIT_TEMPLATE.to_string(),
            template_version: "split-default".to_string(),
            ..Default::default()
        });
        let store = test_store(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();
        helper.sync().await;

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Closed);
        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].data["outcome"], "decomposed");
        assert_eq!(rows[0].data["terminal_reason"], "decomposed:split_template");
        assert_row_satisfies_v1_contract("decomposed row", &rows[0]);
    }

    /// N-T46: an attempt that made its bead an auto-split parent and delivered
    /// no commit decomposed it, whatever template dispatched it. The same
    /// label beside a delivered commit is still a verified success.
    #[tokio::test]
    async fn nt46_new_split_parent_without_commits_resolves_decomposed() {
        let (_guard, _home) = isolated_home();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        let helper = crate::telemetry::test_utils::TestHelper::new("nt46-split-parent");
        let bead = test_bead(BeadStatus::InProgress);
        let parent_labels = vec![
            "umbrella".to_string(),
            crate::mitosis::AUTO_SPLIT_PARENT_LABEL.to_string(),
        ];

        for commits in [Vec::new(), vec!["deadbee".to_string()]] {
            let handler = OutcomeHandler::new(config.clone(), helper.telemetry().clone());
            handler.set_attempt_context(AttemptContext {
                prompt_template: "pluck".to_string(),
                commits,
                ..Default::default()
            });
            let store = test_store(BeadStatus::Done).with_labels(parent_labels.clone());
            let result = handler
                .handle(&store, &bead, &test_output(0), false)
                .await
                .unwrap();
            assert_eq!(result.outcome, Outcome::Success);
        }
        helper.sync().await;

        let rows = helper.events_by_type("attempt.resolved");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].data["outcome"], "decomposed");
        assert_eq!(
            rows[0].data["terminal_reason"],
            "decomposed:split_parent_without_commits"
        );
        assert_eq!(rows[1].data["outcome"], "verified_success");
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
    #[tokio::test(start_paused = true)]
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
        assert!(matches!(result.bead_action, BeadAction::Released(_)));
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
        assert!(matches!(result.bead_action, BeadAction::Released(_)));
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
    #[tokio::test(start_paused = true)]
    async fn a_resolved_dispatch_does_not_silence_the_next_dispatchs_wrapper_paths() {
        let (_guard, _home) = isolated_home();
        let mut config = Config::default();
        config.worker.enforce_shipped_work = false;
        // This fixture exercises the wrapper-path ledger guard, not the
        // default verification gates. Keep the success paths synchronous so
        // only the deliberately slow `show()` in cycle 4 can hit the 1s
        // handler timeout.
        config.validation.default_gates.enabled = false;
        config.validation.fallback_gate = false;
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
        assert!(matches!(result.bead_action, BeadAction::Released(_)));
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
