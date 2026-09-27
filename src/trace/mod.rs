//! Trace capture: adapter-specific structured trace collection.
//!
//! This module captures full execution traces from agent runs including
//! tool calls, agent reasoning, and verifier output. Each dispatch attempt
//! writes its capture to `.beads/traces/<bead-id>/<attempt-id>/` with
//! structured metadata, so a redispatch of the same bead adds a new attempt
//! directory instead of overwriting the previous attempt's files.
//!
//! ## Trace Retention Policy
//!
//! - **Failed beads**: 7 days (full trace retained)
//! - **Successful beads**: metadata-only after 1 day (trace data pruned)
//!
//! Both rules are applied per directory: to each attempt directory
//! independently, and to a bead directory that still holds a capture
//! directly (the legacy flat layout written by older binaries — recognised
//! and pruned by the same rules, never migrated). A legacy flat capture
//! that ages out takes only its own files when the bead directory also
//! holds attempt directories; a flat-only bead directory is removed whole,
//! exactly as before.
//!
//! ## Directory Structure
//!
//! ```text
//! .beads/traces/<bead-id>/
//! ├── attempts.jsonl    # Per-bead attempt journal (see attempt_history)
//! └── <attempt-id>/     # One directory per dispatch attempt
//!     ├── trace.jsonl     # Structured trace events (one JSON object per line)
//!     ├── stdout.txt      # Raw stdout from agent process
//!     ├── stderr.txt      # Raw stderr from agent process
//!     ├── test-output.txt # Processed test output (for test runs)
//!     └── metadata.json   # Timing, tokens, cost, template version
//! ```
//!
//! Older binaries wrote the capture files directly into
//! `.beads/traces/<bead-id>/`. Those legacy flat directories remain
//! recognised by retention cleanup and by readers, and nothing migrates
//! them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::cargo_test::TestMetrics;
use crate::claim::ClaimHandleMetadata;
use crate::dispatch::TimeoutReason;
use crate::sanitize::Sanitizer;
use crate::types::{BeadId, Outcome};

// ──────────────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────────────

/// Stdout file name.
pub const STDOUT_FILE: &str = "stdout.txt";

/// Stderr file name.
pub const STDERR_FILE: &str = "stderr.txt";

/// Test output file name.
pub const TEST_OUTPUT_FILE: &str = "test-output.txt";

// ──────────────────────────────────────────────────────────────────────────────
// Trace metadata
// ──────────────────────────────────────────────────────────────────────────────

/// Metadata stored in `metadata.json` for each trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceMetadata {
    /// Bead ID this trace belongs to.
    pub bead_id: BeadId,
    /// Agent adapter name (e.g., "claude-sonnet").
    pub agent: String,
    /// AI provider (e.g., "anthropic", "openai").
    pub provider: Option<String>,
    /// Compatibility alias for `requested_model`; never provider-returned.
    pub model: Option<String>,
    /// Model identifier configured on the adapter.
    #[serde(default)]
    pub requested_model: Option<String>,
    /// Model identifier returned by provider metadata; unknown stays absent.
    #[serde(default)]
    pub effective_model: Option<String>,
    /// Provider response field that supplied `effective_model`.
    #[serde(default)]
    pub model_resolution_source: Option<String>,
    /// Process exit code.
    pub exit_code: i32,
    /// Classified outcome.
    pub outcome: String,
    /// Wall-clock execution time in milliseconds.
    pub duration_ms: u64,
    /// Input tokens consumed (if available).
    pub input_tokens: Option<u64>,
    /// Output tokens consumed (if available).
    pub output_tokens: Option<u64>,
    /// Estimated cost in USD (if pricing available).
    pub cost_usd: Option<f64>,
    /// Trace capture timestamp.
    pub captured_at: DateTime<Utc>,
    /// Adapter-specific trace format.
    pub trace_format: TraceFormat,
    /// Whether the trace data has been pruned (retention policy).
    pub pruned: bool,
    /// SHA-256 hex digest of the rendered prompt (identifies template version).
    pub template_version: Option<String>,
    /// Structured timeout reason if terminated by timeout (exit_code 124).
    pub timeout_reason: Option<TimeoutReason>,
    /// Envelope `terminal_reason` from the claude_json result envelope, when the
    /// CLI reported why the session ended (e.g. `"api_error"`). Distinguishes an
    /// infrastructure casualty from a genuinely failed task without reading the
    /// raw trace.
    pub terminal_reason: Option<String>,
    /// Envelope `api_error_status` — the HTTP status of the terminal API error
    /// (e.g. 503 during the 2026-09-02 zai-proxy outage). Absent for every
    /// outcome that was not an API error.
    pub api_error_status: Option<u16>,
}

/// Adapter-specific trace format identifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceFormat {
    /// Claude Code JSON output format.
    ClaudeJson,
    /// ZCode desktop CLI stream-json output.
    ZcodeJsonl,
    /// OpenAI/Codex JSONL format.
    OpenaiJsonl,
    /// Aider markdown chat history.
    AiderMarkdown,
    /// Generic raw text capture.
    RawText,
}

// ──────────────────────────────────────────────────────────────────────────────
// Trace storage
// ──────────────────────────────────────────────────────────────────────────────

/// Manages trace storage for a bead execution.
pub struct TraceCapture {
    /// Trace directory for this attempt
    /// (`.beads/traces/<bead-id>/<attempt-id>`, or the legacy flat
    /// `.beads/traces/<bead-id>` when no attempt id was supplied).
    trace_dir: PathBuf,
    /// Whether trace capture is enabled.
    enabled: bool,
    /// Optional sanitizer applied to all content before writing to disk.
    sanitizer: Option<Arc<Sanitizer>>,
    /// Attempt identity scoping this capture, bound at construction.
    attempt_id: Option<String>,
    /// Credential-free claim facts carried from the worker's retained handle.
    claim_handle: Option<ClaimHandleMetadata>,
}

/// Whether `id` can serve as one path segment of a trace directory.
///
/// Attempt ids are UUIDv7 strings minted at dispatch start, which always
/// qualify; the check exists so a malformed id can never steer writes
/// outside the bead's trace directory.
fn is_safe_attempt_component(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\']) && id != "." && id != ".."
}

/// Normalize a caller-supplied attempt id into a usable directory component.
///
/// `None` when no id was supplied or the id is not a safe component; an
/// unusable non-empty id is dropped with a warning rather than trusted as a
/// path segment, falling back to the legacy flat layout.
fn normalized_attempt_component(attempt_id: Option<&str>) -> Option<String> {
    let id = attempt_id?.trim();
    if is_safe_attempt_component(id) {
        Some(id.to_string())
    } else {
        tracing::warn!(
            attempt_id = %id,
            "attempt id is not a usable trace directory component; using the legacy flat trace layout"
        );
        None
    }
}

/// Scope a bead's trace directory to one attempt.
///
/// Appends the attempt segment when `attempt_id` is a usable component and
/// returns the bead directory unchanged otherwise, so callers without an
/// attempt identity (and legacy layouts on disk) keep resolving exactly as
/// before.
pub fn attempt_scoped_dir(bead_trace_dir: &Path, attempt_id: Option<&str>) -> PathBuf {
    match normalized_attempt_component(attempt_id) {
        Some(id) => bead_trace_dir.join(id),
        None => bead_trace_dir.to_path_buf(),
    }
}

impl TraceCapture {
    /// Create a new trace capture for one dispatch attempt of a bead, without
    /// sanitization.
    ///
    /// `beads_root` is the workspace directory containing `.beads/`. With an
    /// attempt id the capture is scoped to
    /// `.beads/traces/<bead-id>/<attempt-id>/` so a redispatch writes a fresh
    /// directory instead of overwriting the previous attempt; without one the
    /// legacy flat `.beads/traces/<bead-id>/` layout is used.
    /// Returns `None` if trace capture is disabled.
    pub fn new(bead_id: &BeadId, beads_root: &Path, attempt_id: Option<&str>) -> Option<Self> {
        Self::new_with_sanitizer(bead_id, beads_root, attempt_id, None)
    }

    /// Create a new trace capture for one dispatch attempt of a bead with an
    /// optional sanitizer.
    ///
    /// When `sanitizer` is `Some`, all trace content is sanitized synchronously
    /// before writing to disk (no unsanitized window on disk).
    pub fn new_with_sanitizer(
        bead_id: &BeadId,
        beads_root: &Path,
        attempt_id: Option<&str>,
        sanitizer: Option<Arc<Sanitizer>>,
    ) -> Option<Self> {
        // Traces live inside the workspace's existing store. Creating
        // `.beads/` where there is none leaves a half-workspace behind that
        // bead-rs's discovery refuses to init past (see hoop_hooks).
        if !beads_root.join(".beads").is_dir() {
            tracing::warn!(
                workspace = %beads_root.display(),
                bead_id = %bead_id,
                "trace capture skipped: workspace has no .beads/ store"
            );
            return None;
        }
        let attempt_id = normalized_attempt_component(attempt_id);
        let mut trace_dir = beads_root.join(".beads").join("traces");
        trace_dir.push(bead_id.as_ref());
        if let Some(ref attempt_id) = attempt_id {
            trace_dir.push(attempt_id);
        }

        // Create the trace directory.
        if let Err(e) = std::fs::create_dir_all(&trace_dir) {
            tracing::warn!(
                bead_id = %bead_id,
                path = %trace_dir.display(),
                error = %e,
                "failed to create trace directory, trace capture disabled"
            );
            return None;
        }

        Some(TraceCapture {
            trace_dir,
            enabled: true,
            sanitizer,
            attempt_id,
            claim_handle: None,
        })
    }

    /// Get the trace directory path.
    pub fn trace_dir(&self) -> &Path {
        &self.trace_dir
    }

    /// Bind only the redacted identity view; the fencing credential is never
    /// serialized into trace files.
    pub fn bind_claim_handle(&mut self, handle: ClaimHandleMetadata) {
        self.claim_handle = Some(handle);
    }

    /// Write stdout to `stdout.txt`.
    ///
    /// Content is sanitized before writing if a sanitizer is configured.
    /// Write errors are logged with tracing::warn and returned in the Result.
    pub fn write_stdout(&self, stdout: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let content = self.sanitize(stdout);
        let path = self.trace_dir.join(STDOUT_FILE);
        match std::fs::write(&path, content.as_bytes()) {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to write stdout trace"
                );
                Err(e).with_context(|| format!("failed to write stdout trace: {}", path.display()))
            }
        }
    }

    /// Write stderr to `stderr.txt`.
    ///
    /// Content is sanitized before writing if a sanitizer is configured.
    /// Write errors are logged with tracing::warn and returned in the Result.
    pub fn write_stderr(&self, stderr: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let content = self.sanitize(stderr);
        let path = self.trace_dir.join(STDERR_FILE);
        match std::fs::write(&path, content.as_bytes()) {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to write stderr trace"
                );
                Err(e).with_context(|| format!("failed to write stderr trace: {}", path.display()))
            }
        }
    }

    /// Write test output to `test-output.txt`.
    ///
    /// This stores processed/formatted test output (e.g., parsed cargo test results).
    /// Content is sanitized before writing if a sanitizer is configured.
    pub fn write_test_output(&self, output: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let content = self.sanitize(output);
        let path = self.trace_dir.join(TEST_OUTPUT_FILE);
        std::fs::write(&path, content.as_bytes())
            .with_context(|| format!("failed to write test output: {}", path.display()))
    }

    /// Write structured trace JSONL to `trace.jsonl`.
    ///
    /// Each line should be a valid JSON object. Lines are sanitized before
    /// writing if a sanitizer is configured.
    pub fn write_trace_jsonl(&self, trace_lines: &[String]) -> Result<()> {
        if !self.enabled || trace_lines.is_empty() {
            return Ok(());
        }
        let path = self.trace_dir.join("trace.jsonl");
        let joined = trace_lines.join("\n");
        let content = self.sanitize(&joined);
        std::fs::write(&path, content.as_bytes())
            .with_context(|| format!("failed to write trace JSONL: {}", path.display()))
    }

    /// Sanitize text if a sanitizer is configured; otherwise return as-is.
    fn sanitize<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        match &self.sanitizer {
            Some(s) => std::borrow::Cow::Owned(s.sanitize(text)),
            None => std::borrow::Cow::Borrowed(text),
        }
    }

    /// Write metadata to `metadata.json`.
    pub fn write_metadata(&self, metadata: &TraceMetadata) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self.trace_dir.join("metadata.json");
        let mut value =
            serde_json::to_value(metadata).context("failed to serialize trace metadata")?;
        if let Some(attempt_id) = &self.attempt_id {
            value["attempt_id"] = serde_json::json!(attempt_id);
        }
        if let Some(claim_handle) = &self.claim_handle {
            value["claim_handle"] = serde_json::to_value(claim_handle)
                .context("failed to serialize redacted claim metadata")?;
        }
        let json =
            serde_json::to_string_pretty(&value).context("failed to serialize trace metadata")?;
        std::fs::write(&path, json)
            .with_context(|| format!("failed to write metadata: {}", path.display()))
    }

    /// Write test metrics to `test_metrics.json`.
    ///
    /// This stores cargo test execution metrics including exit code,
    /// duration, and output sizes for later analysis.
    pub fn write_test_metrics(&self, metrics: &TestMetrics) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self.trace_dir.join("test_metrics.json");
        let json =
            serde_json::to_string_pretty(metrics).context("failed to serialize test metrics")?;
        std::fs::write(&path, json)
            .with_context(|| format!("failed to write test metrics: {}", path.display()))
    }

    /// Write compilation errors to `compilation_errors.json`.
    ///
    /// This stores detailed compilation error information including error codes,
    /// variant classifications, and file locations for later analysis.
    pub fn write_compilation_errors(
        &self,
        errors: &[crate::cargo_test::CompilationError],
    ) -> Result<()> {
        if !self.enabled || errors.is_empty() {
            return Ok(());
        }
        let path = self.trace_dir.join("compilation_errors.json");
        let json = serde_json::to_string_pretty(errors)
            .context("failed to serialize compilation errors")?;
        std::fs::write(&path, json)
            .with_context(|| format!("failed to write compilation errors: {}", path.display()))
    }

    /// Finalize the trace and return the trace directory path.
    ///
    /// Returns `None` if trace capture was disabled.
    pub fn finalize(self) -> Option<PathBuf> {
        if self.enabled {
            Some(self.trace_dir)
        } else {
            None
        }
    }

    /// Delete the entire trace directory.
    pub fn delete(&self) -> Result<()> {
        if self.trace_dir.exists() {
            std::fs::remove_dir_all(&self.trace_dir).with_context(|| {
                format!(
                    "failed to delete trace directory: {}",
                    self.trace_dir.display()
                )
            })?;
        }
        Ok(())
    }

    /// Prune trace data (keep metadata only).
    ///
    /// Deletes trace.jsonl, stdout.txt, stderr.txt, and test-output.txt, keeping only metadata.json.
    /// Updates the `pruned` flag in metadata.
    pub fn prune_trace_data(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Delete trace data files.
        for file in ["trace.jsonl", STDOUT_FILE, STDERR_FILE, TEST_OUTPUT_FILE] {
            let path = self.trace_dir.join(file);
            if path.exists() {
                match std::fs::remove_file(&path) {
                    Ok(_) => {
                        tracing::debug!(
                            path = %path.display(),
                            "successfully removed trace file"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %path.display(),
                            "failed to remove trace file during prune"
                        );
                        return Err(e).with_context(|| {
                            format!("failed to prune trace file: {}", path.display())
                        });
                    }
                }
            }
        }

        // Update metadata to mark as pruned.
        let metadata_path = self.trace_dir.join("metadata.json");
        if metadata_path.exists() {
            let content = std::fs::read_to_string(&metadata_path)?;
            if let Ok(mut metadata) = serde_json::from_str::<TraceMetadata>(&content) {
                metadata.pruned = true;
                let json = serde_json::to_string_pretty(&metadata)?;
                std::fs::write(&metadata_path, json)?;
            }
        }

        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Trace format detection
// ──────────────────────────────────────────────────────────────────────────────

/// Detect trace format from agent adapter name.
pub fn detect_trace_format(agent_name: &str) -> TraceFormat {
    match agent_name {
        // `claude` is the built-in Claude Code adapter name; the other
        // Claude variants use names such as `claude-sonnet` and
        // `claude-print`. Keep the exact name in the Claude stream family so
        // its terminal result envelope participates in classification too.
        "claude" => TraceFormat::ClaudeJson,
        n if n.starts_with("claude-") => TraceFormat::ClaudeJson,
        n if n.contains("zcode") => TraceFormat::ZcodeJsonl,
        n if n.contains("codex") || n.contains("openai") => TraceFormat::OpenaiJsonl,
        n if n.contains("aider") => TraceFormat::AiderMarkdown,
        _ => TraceFormat::RawText,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Result envelope (claude_json)
// ──────────────────────────────────────────────────────────────────────────────

/// The final `type="result"` envelope of a Claude Code stream-json run.
///
/// The claude CLI exits 0 even when the session terminated on an API error,
/// and the envelope's own `subtype` can still read `"success"` in that case
/// (observed across the commitgraph workspace during the 2026-09-02 zai-proxy
/// outage) — so `is_error` and `terminal_reason` are the only usable failure
/// signals.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeResultEnvelope {
    /// Envelope `is_error` flag.
    pub is_error: bool,
    /// Envelope `subtype`. Kept for diagnostics only — it is NOT a usable
    /// failure signal (see the struct doc).
    pub subtype: Option<String>,
    /// Envelope `terminal_reason`, when the CLI reports why the session ended
    /// (e.g. `"api_error"`).
    pub terminal_reason: Option<String>,
    /// Envelope `api_error_status` — the HTTP status behind the terminal error
    /// (e.g. 503).
    pub api_error_status: Option<u16>,
    /// `usage.input_tokens` — the session's total input tokens, as the CLI
    /// reports them on the final envelope.
    pub input_tokens: Option<u64>,
    /// `usage.output_tokens` — the session's total output tokens.
    pub output_tokens: Option<u64>,
    /// `total_cost_usd` as the CLI computed it. Subscription-billed runs
    /// (claude-print) report `0`, which callers must treat as unknown.
    pub total_cost_usd: Option<f64>,
}

impl ClaudeResultEnvelope {
    /// Whether the envelope reports a run that ended in a terminal failure.
    ///
    /// `is_error` is authoritative. `terminal_reason` is only trusted when it
    /// names a recognized error, so an unrecognized reason string can never
    /// turn a clean run into a failure — the failure path releases the bead and
    /// increments its failure count, and a false positive eventually quarantines
    /// a healthy bead.
    pub fn indicates_failure(&self) -> bool {
        if self.is_error {
            return true;
        }
        match self.terminal_reason.as_deref() {
            Some(reason) if !reason.is_empty() => is_error_terminal_reason(reason),
            _ => false,
        }
    }
}

/// Terminal reasons this code recognizes as errors. Anything unrecognized
/// (including empty) is treated as a non-error reason — see
/// [`ClaudeResultEnvelope::indicates_failure`].
fn is_error_terminal_reason(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("error") || matches!(lower.as_str(), "rate_limited" | "overloaded")
}

/// Parse the final `type="result"` envelope from a Claude stream-json stdout.
///
/// Scans backwards for the last result line, skipping lines that are not valid
/// JSON. Returns `None` when the stream carries no result envelope — other
/// trace formats, or a run killed before the envelope was emitted.
pub fn parse_result_envelope(stdout: &str) -> Option<ClaudeResultEnvelope> {
    stdout
        .lines()
        .rev()
        .filter(|line| line.contains("\"result\""))
        .find_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            if value.get("type").and_then(|t| t.as_str()) != Some("result") {
                return None;
            }
            Some(ClaudeResultEnvelope {
                is_error: value
                    .get("is_error")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(false),
                subtype: value
                    .get("subtype")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned),
                terminal_reason: value
                    .get("terminal_reason")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned),
                api_error_status: value
                    .get("api_error_status")
                    .and_then(|s| s.as_u64())
                    .and_then(|status| u16::try_from(status).ok()),
                input_tokens: value
                    .get("usage")
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(|t| t.as_u64()),
                output_tokens: value
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(|t| t.as_u64()),
                total_cost_usd: value.get("total_cost_usd").and_then(|c| c.as_f64()),
            })
        })
}

/// Whether a stream's final result envelope reports a terminal failure.
///
/// `false` when the stream carries no result envelope — callers without one
/// fall back to the exit code.
pub fn stream_indicates_failure(stdout: &str) -> bool {
    let claude_failure = parse_result_envelope(stdout)
        .map(|envelope| envelope.indicates_failure())
        .unwrap_or(false);
    claude_failure || zcode_stream_indicates_failure(stdout)
}

/// Whether the last terminal ZCode turn event reports failure.
///
/// ZCode normally preserves a failing exit code, but treating its structured
/// terminal event as authoritative prevents a future wrapper/CLI regression
/// from turning `turn.failed` plus exit 0 into a successful bead.
fn zcode_stream_indicates_failure(stdout: &str) -> bool {
    stdout.lines().rev().find_map(|line| {
        if !line.contains("\"turn.") {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        match value.get("type").and_then(|kind| kind.as_str()) {
            Some("turn.failed") => Some(true),
            Some("turn.completed") => Some(false),
            _ => None,
        }
    }) == Some(true)
}

/// Classify an outcome from the result envelope when the trace format carries
/// one, falling back to the exit code for formats that do not.
///
/// The exit code alone misclassifies claude runs that ended on a terminal API
/// error — the CLI exits 0 — so the envelope wins whenever it exists.
pub fn classify_from_stream(exit_code: i32, stdout: &str, format: &TraceFormat) -> Outcome {
    if matches!(format, TraceFormat::ClaudeJson | TraceFormat::ZcodeJsonl)
        && stream_indicates_failure(stdout)
    {
        return Outcome::Failure;
    }
    Outcome::classify(exit_code, false)
}

// ──────────────────────────────────────────────────────────────────────────────
// Trace retention cleanup
// ──────────────────────────────────────────────────────────────────────────────

/// Cleanup result for trace retention.
#[derive(Debug, Default)]
pub struct TraceCleanupSummary {
    /// Number of traces pruned (metadata kept).
    pub traces_pruned: u32,
    /// Number of traces fully deleted.
    pub traces_deleted: u32,
}

/// Clean up old traces based on retention policy.
///
/// - Failed beads (non-zero exit): delete after the configured failure retention
/// - Successful beads (exit 0): prune data after the configured success retention,
///   keeping metadata only
///
/// The rules are applied per trace directory: to the bead directory itself
/// (the legacy flat layout written by older binaries — never migrated) and to
/// each attempt subdirectory independently, so one aged-out attempt never
/// decides the fate of a newer attempt of the same bead.
pub fn cleanup_traces(
    traces_dir: &Path,
    retention_days_failed: u32,
    retention_days_success: u32,
) -> Result<TraceCleanupSummary> {
    let mut summary = TraceCleanupSummary::default();

    if !traces_dir.exists() {
        return Ok(summary);
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Iterate through bead trace directories.
    for entry in std::fs::read_dir(traces_dir)
        .with_context(|| format!("failed to read traces directory: {}", traces_dir.display()))?
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();

        // Only process directories (bead-id subdirectories).
        if !path.is_dir() {
            continue;
        }

        // Legacy flat layout: the bead directory itself holds one capture.
        apply_retention_to_dir(
            &path,
            now,
            retention_days_failed,
            retention_days_success,
            &mut summary,
        );

        // Attempt layout: each subdirectory holds one dispatch attempt's
        // capture and is retained independently of its siblings.
        if let Ok(attempts) = std::fs::read_dir(&path) {
            for attempt in attempts.flatten() {
                let attempt_path = attempt.path();
                if attempt_path.is_dir() {
                    apply_retention_to_dir(
                        &attempt_path,
                        now,
                        retention_days_failed,
                        retention_days_success,
                        &mut summary,
                    );
                }
            }
        }

        // When every attempt directory has aged out and nothing else remains
        // (the attempt journal, lessons, a WIP patch), drop the empty bead
        // directory. `remove_dir` — not `remove_dir_all` — so a concurrent
        // attempt creation between the emptiness check and the removal makes
        // this a no-op instead of deleting a live capture.
        if path.is_dir() && dir_is_empty(&path) {
            if let Err(e) = std::fs::remove_dir(&path) {
                tracing::debug!(
                    path = %path.display(),
                    error = %e,
                    "failed to remove empty bead trace directory"
                );
            }
        }
    }

    Ok(summary)
}

/// Apply the retention rules to one trace directory: a bead directory in the
/// legacy flat layout, or one attempt directory.
fn apply_retention_to_dir(
    path: &Path,
    now: u64,
    retention_days_failed: u32,
    retention_days_success: u32,
    summary: &mut TraceCleanupSummary,
) {
    // Check metadata.json to determine outcome and age.
    let metadata_path = path.join("metadata.json");
    let metadata: Option<TraceMetadata> = metadata_path
        .exists()
        .then(|| {
            let content = std::fs::read_to_string(&metadata_path).ok()?;
            serde_json::from_str(&content).ok()
        })
        .flatten();

    let age_days = metadata
        .as_ref()
        .and_then(|m| now.checked_sub(m.captured_at.timestamp() as u64))
        .map(|secs| secs / 86400)
        .unwrap_or(u64::MAX);

    let is_failed = metadata.as_ref().map(|m| m.exit_code != 0).unwrap_or(false);
    let is_pruned = metadata.as_ref().map(|m| m.pruned).unwrap_or(false);

    // Check if trace data files actually exist before attempting to prune.
    // This prevents counting a trace as "pruned" when the data files were
    // already removed in a previous run but the metadata update failed
    // or was interrupted. This check is crucial for preventing infinite
    // loops where the same trace is counted repeatedly.
    let has_data_files = ["trace.jsonl", STDOUT_FILE, STDERR_FILE, TEST_OUTPUT_FILE]
        .iter()
        .any(|file| path.join(file).exists());

    let should_delete = is_failed && age_days > retention_days_failed as u64;
    // A trace marked pruned may still contain data when a prior cleanup was
    // interrupted after updating metadata. Finish removing those files too.
    let should_prune = !is_failed && has_data_files && age_days > retention_days_success as u64;

    // Fix up metadata for traces that were partially pruned (files gone
    // but metadata not updated). This prevents infinite loops.
    if !is_failed && !is_pruned && !has_data_files && age_days > retention_days_success as u64 {
        if let Err(e) = fix_pruned_metadata(path) {
            tracing::debug!(
                path = %path.display(),
                error = %e,
                "failed to fix pruned metadata for partially-pruned trace"
            );
        }
    }

    if should_delete {
        if contains_subdirectories(path) {
            // Mixed layout: the directory also holds attempt captures (and
            // possibly the bead journal), each retained on its own age. A
            // flat capture aging out must take only its own files — the
            // bead directory is reaped by the empty-dir pass once nothing
            // else remains.
            if let Err(e) = remove_capture_files(path) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to delete legacy flat trace files"
                );
            } else {
                summary.traces_deleted += 1;
            }
        } else {
            // Delete entire trace directory.
            if let Err(e) = std::fs::remove_dir_all(path) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to delete old trace directory"
                );
            } else {
                summary.traces_deleted += 1;
            }
        }
    } else if should_prune {
        // Prune trace data, keep metadata.
        if let Err(e) = prune_trace_dir(path) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to prune trace data"
            );
        } else if !is_pruned {
            summary.traces_pruned += 1;
        }
    }
}

/// Whether a directory holds no entries (unreadable counts as non-empty).
fn dir_is_empty(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

/// Whether a directory holds at least one subdirectory. Unreadable counts
/// as true — the conservative answer that steers deletion away from a
/// wholesale directory removal.
fn contains_subdirectories(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|entries| entries.filter_map(|e| e.ok()).any(|e| e.path().is_dir()))
        .unwrap_or(true)
}

/// Remove one directory's flat capture files (data plus metadata), leaving
/// any other content — attempt directories, the bead journal — in place.
fn remove_capture_files(trace_dir: &Path) -> Result<()> {
    for file in [
        "trace.jsonl",
        STDOUT_FILE,
        STDERR_FILE,
        TEST_OUTPUT_FILE,
        "test_metrics.json",
        "compilation_errors.json",
        "metadata.json",
    ] {
        let path = trace_dir.join(file);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove trace file: {}", path.display()))?;
        }
    }
    Ok(())
}

/// Fix metadata for a trace that was partially pruned (data files gone but
/// metadata not updated). This is a recovery operation for interrupted pruning.
fn fix_pruned_metadata(trace_dir: &Path) -> Result<()> {
    let metadata_path = trace_dir.join("metadata.json");
    if !metadata_path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&metadata_path)?;
    if let Ok(mut metadata) = serde_json::from_str::<TraceMetadata>(&content) {
        if !metadata.pruned {
            metadata.pruned = true;
            let json = serde_json::to_string_pretty(&metadata)?;
            std::fs::write(&metadata_path, json)?;
            tracing::debug!(
                path = %trace_dir.display(),
                "fixed pruned metadata for partially-pruned trace"
            );
        }
    }
    Ok(())
}

/// Prune trace data files in a directory, keeping only metadata.json.
///
/// Updates metadata FIRST to mark as pruned, then removes data files.
/// This order is critical: if the process is interrupted after metadata
/// update but before file removal, the next cleanup will skip this trace
/// (because is_pruned=true) and only remove remaining files. This prevents
/// infinite loops where the same traces are counted as "pruned" repeatedly.
fn prune_trace_dir(trace_dir: &Path) -> Result<()> {
    // Step 1: Update metadata to mark as pruned BEFORE removing files.
    // This prevents the same trace from being counted as pruned multiple times
    // if the process is interrupted between metadata update and file removal.
    let metadata_path = trace_dir.join("metadata.json");
    if metadata_path.exists() {
        let content = std::fs::read_to_string(&metadata_path)?;
        if let Ok(mut metadata) = serde_json::from_str::<TraceMetadata>(&content) {
            metadata.pruned = true;
            let json = serde_json::to_string_pretty(&metadata)?;
            std::fs::write(&metadata_path, json)?;
        }
    }

    // Step 2: Remove trace data files after metadata is updated.
    // Use ? to propagate errors - if file removal fails, the operator should
    // know so they can investigate. Files that don't exist are skipped.
    for file in ["trace.jsonl", STDOUT_FILE, STDERR_FILE, TEST_OUTPUT_FILE] {
        let path = trace_dir.join(file);
        if path.exists() {
            match std::fs::remove_file(&path) {
                Ok(_) => {
                    tracing::debug!(
                        path = %path.display(),
                        "successfully removed trace file"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        "failed to remove trace file during cleanup"
                    );
                    return Err(e).with_context(|| {
                        format!("failed to prune trace file: {}", path.display())
                    });
                }
            }
        }
    }

    Ok(())
}

use std::time::SystemTime;

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_bead_id() -> BeadId {
        BeadId::from("needle-test")
    }

    #[test]
    fn trace_capture_creates_directory() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();
        assert!(capture.trace_dir().exists());
        assert!(capture.trace_dir().ends_with("traces/needle-test"));
    }

    #[test]
    fn trace_capture_returns_none_when_directory_creation_fails() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        // Create a file at the trace directory path to block directory creation.
        let blocking_path = beads_root
            .join(".beads")
            .join("traces")
            .join("blocked-bead");
        std::fs::create_dir_all(blocking_path.parent().unwrap()).unwrap();
        std::fs::write(&blocking_path, b"blocking file").unwrap();

        // Attempting to create a TraceCapture should return None gracefully.
        let bead_id = BeadId::from("blocked-bead");
        let capture = TraceCapture::new(&bead_id, beads_root, None);
        assert!(
            capture.is_none(),
            "TraceCapture should return None when directory creation fails"
        );
    }

    #[test]
    fn trace_capture_writes_stdout() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();
        capture.write_stdout("hello stdout").unwrap();

        let stdout_path = capture.trace_dir().join("stdout.txt");
        assert!(stdout_path.exists());
        let content = std::fs::read_to_string(stdout_path).unwrap();
        assert_eq!(content, "hello stdout");
    }

    #[test]
    fn trace_capture_writes_stderr() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();
        capture.write_stderr("error output").unwrap();

        let stderr_path = capture.trace_dir().join("stderr.txt");
        assert!(stderr_path.exists());
        let content = std::fs::read_to_string(stderr_path).unwrap();
        assert_eq!(content, "error output");
    }

    #[test]
    fn trace_capture_write_stderr_handles_errors_gracefully() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();

        // Remove the trace directory to force a write error. `write_stderr`
        // does not create parent directories, so the write fails with
        // NotFound. Do NOT make the directory read-only instead: CI runs the
        // build container as root, root bypasses DAC permission checks, the
        // write then succeeds and this test fails only in CI.
        std::fs::remove_dir_all(capture.trace_dir()).unwrap();

        // Attempting to write stderr should return an error gracefully.
        let result = capture.write_stderr("test stderr");

        // Verify that an error is returned (not a panic).
        assert!(result.is_err());

        // Verify the error has appropriate context.
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("failed to write stderr trace"));
    }

    #[test]
    fn trace_capture_writes_test_output() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();
        capture.write_test_output("test output content").unwrap();

        let test_output_path = capture.trace_dir().join(TEST_OUTPUT_FILE);
        assert!(test_output_path.exists());
        let content = std::fs::read_to_string(test_output_path).unwrap();
        assert_eq!(content, "test output content");
    }

    #[test]
    fn trace_capture_writes_trace_jsonl() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();
        let lines = vec![
            r#"{"event": "start"}"#.to_string(),
            r#"{"event": "tool", "name": "read_file"}"#.to_string(),
            r#"{"event": "end"}"#.to_string(),
        ];
        capture.write_trace_jsonl(&lines).unwrap();

        let trace_path = capture.trace_dir().join("trace.jsonl");
        assert!(trace_path.exists());
        let content = std::fs::read_to_string(trace_path).unwrap();
        assert_eq!(content, lines.join("\n"));
    }

    #[test]
    fn trace_capture_writes_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();
        let metadata = TraceMetadata {
            bead_id: test_bead_id(),
            agent: "claude-sonnet".to_string(),
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code: 0,
            outcome: "success".to_string(),
            duration_ms: 1234,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cost_usd: Some(0.001),
            captured_at: Utc::now(),
            trace_format: TraceFormat::ClaudeJson,
            pruned: false,
            template_version: Some("abc123".to_string()),
            timeout_reason: None,
            terminal_reason: None,
            api_error_status: None,
        };
        capture.write_metadata(&metadata).unwrap();

        let metadata_path = capture.trace_dir().join("metadata.json");
        assert!(metadata_path.exists());

        let content = std::fs::read_to_string(metadata_path).unwrap();
        let parsed: TraceMetadata = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.bead_id, test_bead_id());
        assert_eq!(parsed.agent, "claude-sonnet");
        assert_eq!(parsed.exit_code, 0);
        assert!(!parsed.pruned);
    }

    #[test]
    fn trace_capture_delete_removes_directory() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();
        assert!(capture.trace_dir().exists());

        capture.delete().unwrap();
        assert!(!capture.trace_dir().exists());
    }

    #[test]
    fn trace_capture_write_stdout_handles_errors_gracefully() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();

        // Remove the trace directory to force a write error. `write_stdout`
        // does not create parent directories, so the write fails with
        // NotFound. Do NOT make the directory read-only instead: CI runs the
        // build container as root, root bypasses DAC permission checks, the
        // write then succeeds and this test fails only in CI.
        std::fs::remove_dir_all(capture.trace_dir()).unwrap();

        // Attempting to write stdout should return an error gracefully.
        let result = capture.write_stdout("test stdout");

        // Verify that an error is returned (not a panic).
        assert!(result.is_err());

        // Verify the error has appropriate context.
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("failed to write stdout trace"));
    }

    #[test]
    fn trace_capture_prune_removes_data_keeps_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();
        capture.write_stdout("stdout").unwrap();
        capture.write_stderr("stderr").unwrap();
        capture.write_test_output("test output").unwrap();

        let metadata = TraceMetadata {
            bead_id: test_bead_id(),
            agent: "test".to_string(),
            provider: None,
            model: None,
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code: 0,
            outcome: "success".to_string(),
            duration_ms: 100,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            captured_at: Utc::now(),
            trace_format: TraceFormat::RawText,
            pruned: false,
            template_version: None,
            timeout_reason: None,
            terminal_reason: None,
            api_error_status: None,
        };
        capture.write_metadata(&metadata).unwrap();

        // Verify files exist.
        assert!(capture.trace_dir().join("stdout.txt").exists());
        assert!(capture.trace_dir().join("stderr.txt").exists());
        assert!(capture.trace_dir().join(TEST_OUTPUT_FILE).exists());
        assert!(capture.trace_dir().join("metadata.json").exists());

        // Prune.
        capture.prune_trace_data().unwrap();

        // Verify data files removed, metadata remains.
        assert!(!capture.trace_dir().join("stdout.txt").exists());
        assert!(!capture.trace_dir().join("stderr.txt").exists());
        assert!(!capture.trace_dir().join(TEST_OUTPUT_FILE).exists());
        assert!(capture.trace_dir().join("metadata.json").exists());

        // Verify metadata marked as pruned.
        let content = std::fs::read_to_string(capture.trace_dir().join("metadata.json")).unwrap();
        let parsed: TraceMetadata = serde_json::from_str(&content).unwrap();
        assert!(parsed.pruned);
    }

    #[test]
    fn detect_trace_format_claude() {
        assert_eq!(
            detect_trace_format("claude-sonnet"),
            TraceFormat::ClaudeJson
        );
        assert_eq!(detect_trace_format("claude-opus"), TraceFormat::ClaudeJson);
    }

    #[test]
    fn detect_trace_format_zcode() {
        assert_eq!(
            detect_trace_format("zcode-headless"),
            TraceFormat::ZcodeJsonl
        );
    }

    #[test]
    fn detect_trace_format_openai() {
        assert_eq!(detect_trace_format("codex"), TraceFormat::OpenaiJsonl);
        assert_eq!(detect_trace_format("openai-gpt"), TraceFormat::OpenaiJsonl);
    }

    #[test]
    fn detect_trace_format_aider() {
        assert_eq!(detect_trace_format("aider"), TraceFormat::AiderMarkdown);
    }

    #[test]
    fn detect_trace_format_generic() {
        assert_eq!(detect_trace_format("generic"), TraceFormat::RawText);
    }

    // ── Result envelope (claude_json) tests ──

    /// The exact failure shape observed during the 2026-09-02 zai-proxy
    /// outage: exit 0, subtype "success", is_error true, terminal_reason set.
    const API_ERROR_RESULT_LINE: &str = r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":503,"terminal_reason":"api_error","num_turns":1,"result":"API Error: 503 no available server","session_id":"s1"}"#;

    fn result_stream(result_line: &str) -> String {
        format!(
            "{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}}\n\
             {{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[]}}}}\n\
             {result_line}\n"
        )
    }

    #[test]
    fn parse_result_envelope_reads_usage_and_cost() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.71256,"usage":{"input_tokens":66163,"cache_read_input_tokens":667840,"output_tokens":1403},"num_turns":17,"result":"done","session_id":"s1"}"#;
        let envelope = parse_result_envelope(&result_stream(line)).expect("envelope");
        assert_eq!(envelope.input_tokens, Some(66163));
        assert_eq!(envelope.output_tokens, Some(1403));
        assert_eq!(envelope.total_cost_usd, Some(0.71256));
        assert!(!envelope.indicates_failure());

        // Subscription runs report a zero cost and no usage is optional.
        let line = r#"{"type":"result","subtype":"success","is_error":false,"cost_usd":0,"session_id":"s1"}"#;
        let envelope = parse_result_envelope(&result_stream(line)).expect("envelope");
        assert_eq!(envelope.input_tokens, None);
        assert_eq!(envelope.total_cost_usd, None);
    }

    #[test]
    fn parse_result_envelope_finds_the_final_result_line() {
        let stdout = result_stream(API_ERROR_RESULT_LINE);
        let envelope = parse_result_envelope(&stdout).expect("envelope should parse");

        assert!(envelope.is_error);
        assert_eq!(envelope.subtype.as_deref(), Some("success"));
        assert_eq!(envelope.terminal_reason.as_deref(), Some("api_error"));
        assert_eq!(envelope.api_error_status, Some(503));
    }

    #[test]
    fn parse_result_envelope_leaves_api_error_status_absent_when_unreported() {
        let stdout = result_stream(r#"{"type":"result","subtype":"success","is_error":false}"#);
        let envelope = parse_result_envelope(&stdout).expect("envelope should parse");
        assert_eq!(envelope.api_error_status, None);
    }

    #[test]
    fn parse_result_envelope_ignores_an_out_of_range_api_error_status() {
        // HTTP statuses fit u16; anything wider is malformed rather than fatal.
        let stdout = result_stream(r#"{"type":"result","is_error":true,"api_error_status":99999}"#);
        let envelope = parse_result_envelope(&stdout).expect("envelope should parse");
        assert_eq!(envelope.api_error_status, None);
        assert!(envelope.is_error);
    }

    #[test]
    fn parse_result_envelope_prefers_the_last_envelope() {
        let stdout = format!(
            "{}{}",
            result_stream(r#"{"type":"result","subtype":"success","is_error":false}"#),
            result_stream(API_ERROR_RESULT_LINE),
        );
        let envelope = parse_result_envelope(&stdout).expect("envelope should parse");
        assert!(envelope.is_error, "the final envelope must win");
    }

    #[test]
    fn parse_result_envelope_returns_none_without_an_envelope() {
        assert_eq!(parse_result_envelope(""), None);
        assert_eq!(parse_result_envelope("plain text output\n"), None);
        // stream-json lines that are not result envelopes
        assert_eq!(
            parse_result_envelope(
                r#"{"type":"assistant","message":{"role":"assistant","content":[]}}"#
            ),
            None
        );
    }

    #[test]
    fn parse_result_envelope_skips_unparseable_lines() {
        let stdout = format!("{{\"result\": truncated\n{API_ERROR_RESULT_LINE}\n");
        let envelope = parse_result_envelope(&stdout).expect("envelope should parse");
        assert!(envelope.is_error);
    }

    #[test]
    fn envelope_is_error_indicates_failure_even_with_success_subtype() {
        let envelope = parse_result_envelope(API_ERROR_RESULT_LINE).expect("envelope should parse");
        assert!(envelope.indicates_failure());
    }

    #[test]
    fn envelope_error_terminal_reason_indicates_failure() {
        let envelope = ClaudeResultEnvelope {
            is_error: false,
            subtype: Some("success".to_string()),
            terminal_reason: Some("api_error".to_string()),
            api_error_status: None,
            ..Default::default()
        };
        assert!(envelope.indicates_failure());
    }

    #[test]
    fn envelope_success_is_not_a_failure() {
        let envelope = ClaudeResultEnvelope {
            is_error: false,
            subtype: Some("success".to_string()),
            terminal_reason: None,
            api_error_status: None,
            ..Default::default()
        };
        assert!(!envelope.indicates_failure());
    }

    #[test]
    fn envelope_unrecognized_terminal_reason_is_not_a_failure() {
        // A terminal reason we do not recognize must never turn a clean run
        // into a failure: the failure path increments the bead's failure count.
        let envelope = ClaudeResultEnvelope {
            is_error: false,
            subtype: Some("success".to_string()),
            terminal_reason: Some("user_exit".to_string()),
            api_error_status: None,
            ..Default::default()
        };
        assert!(!envelope.indicates_failure());
    }

    #[test]
    fn envelope_empty_terminal_reason_is_not_a_failure() {
        let envelope = ClaudeResultEnvelope {
            is_error: false,
            subtype: Some("success".to_string()),
            terminal_reason: Some(String::new()),
            api_error_status: None,
            ..Default::default()
        };
        assert!(!envelope.indicates_failure());
    }

    #[test]
    fn stream_indicates_failure_false_without_an_envelope() {
        assert!(!stream_indicates_failure(""));
        assert!(!stream_indicates_failure(
            r#"{"type":"assistant","message":{"role":"assistant","content":[]}}"#
        ));
    }

    #[test]
    fn zcode_terminal_turn_failure_overrides_zero_exit() {
        let stdout = concat!(
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"model.streaming\",\"payload\":{\"done\":true}}\n",
            "{\"type\":\"turn.failed\",\"payload\":{\"turnPhase\":\"model\"}}\n",
        );
        assert!(stream_indicates_failure(stdout));
        assert_eq!(
            classify_from_stream(0, stdout, &TraceFormat::ZcodeJsonl),
            Outcome::Failure
        );
    }

    #[test]
    fn zcode_completed_turn_keeps_zero_exit_successful() {
        let stdout = concat!(
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"turn.completed\"}\n",
            "{\"type\":\"result\",\"response\":\"done\"}\n",
        );
        assert!(!stream_indicates_failure(stdout));
        assert_eq!(
            classify_from_stream(0, stdout, &TraceFormat::ZcodeJsonl),
            Outcome::Success
        );
    }

    #[test]
    fn classify_from_stream_overrides_a_zero_exit_code() {
        // The 2026-09-02 shape: exit 0, is_error true, terminal_reason api_error.
        let stdout = result_stream(API_ERROR_RESULT_LINE);
        assert_eq!(
            classify_from_stream(0, &stdout, &TraceFormat::ClaudeJson),
            Outcome::Failure
        );
    }

    #[test]
    fn classify_from_stream_keeps_success_for_a_clean_envelope() {
        let stdout = result_stream(r#"{"type":"result","subtype":"success","is_error":false}"#);
        assert_eq!(
            classify_from_stream(0, &stdout, &TraceFormat::ClaudeJson),
            Outcome::Success
        );
    }

    #[test]
    fn classify_from_stream_falls_back_to_exit_code_without_an_envelope() {
        // Formats with no result envelope (or a stream cut short before one
        // was emitted) keep the exit-code classification.
        assert_eq!(
            classify_from_stream(0, "", &TraceFormat::ClaudeJson),
            Outcome::Success
        );
        assert_eq!(
            classify_from_stream(1, "", &TraceFormat::ClaudeJson),
            Outcome::Failure
        );
    }

    #[test]
    fn classify_from_stream_ignores_envelope_for_non_claude_formats() {
        let stdout = result_stream(API_ERROR_RESULT_LINE);
        assert_eq!(
            classify_from_stream(0, &stdout, &TraceFormat::OpenaiJsonl),
            Outcome::Success
        );
        assert_eq!(
            classify_from_stream(0, &stdout, &TraceFormat::RawText),
            Outcome::Success
        );
    }

    #[test]
    fn classify_from_stream_nonzero_exit_code_still_wins_for_non_error_envelope() {
        // An envelope that reports no failure leaves the exit-code mapping intact.
        let stdout = result_stream(r#"{"type":"result","subtype":"error_max_turns"}"#);
        assert_eq!(
            classify_from_stream(124, &stdout, &TraceFormat::ClaudeJson),
            Outcome::Timeout
        );
    }

    #[test]
    fn trace_metadata_serde_roundtrip() {
        let metadata = TraceMetadata {
            bead_id: test_bead_id(),
            agent: "claude-sonnet".to_string(),
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code: 0,
            outcome: "success".to_string(),
            duration_ms: 1234,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cost_usd: Some(0.001),
            captured_at: Utc::now(),
            trace_format: TraceFormat::ClaudeJson,
            pruned: false,
            template_version: Some("deadbeef".to_string()),
            timeout_reason: None,
            terminal_reason: Some("api_error".to_string()),
            api_error_status: Some(503),
        };

        let json = serde_json::to_string(&metadata).unwrap();
        // Reconstructing an outage window means reading metadata.json with
        // tooling outside this struct, so the serialized key names are part of
        // the contract alongside the values.
        assert!(json.contains(r#""terminal_reason":"api_error""#));
        assert!(json.contains(r#""api_error_status":503"#));
        let parsed: TraceMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.bead_id, metadata.bead_id);
        assert_eq!(parsed.agent, metadata.agent);
        assert_eq!(parsed.provider, metadata.provider);
        assert_eq!(parsed.model, metadata.model);
        assert_eq!(parsed.exit_code, metadata.exit_code);
        assert_eq!(parsed.outcome, metadata.outcome);
        assert_eq!(parsed.duration_ms, metadata.duration_ms);
        assert_eq!(parsed.input_tokens, metadata.input_tokens);
        assert_eq!(parsed.output_tokens, metadata.output_tokens);
        assert_eq!(parsed.cost_usd, metadata.cost_usd);
        assert_eq!(parsed.trace_format, metadata.trace_format);
        assert_eq!(parsed.pruned, metadata.pruned);
        assert_eq!(parsed.template_version, metadata.template_version);
        assert_eq!(parsed.terminal_reason, metadata.terminal_reason);
        assert_eq!(parsed.api_error_status, metadata.api_error_status);
    }

    #[test]
    fn trace_metadata_records_structured_timeout_reason() {
        let metadata = TraceMetadata {
            bead_id: test_bead_id(),
            agent: "test-agent".to_string(),
            provider: None,
            model: None,
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code: 124,
            outcome: "timeout".to_string(),
            duration_ms: 6_125,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            captured_at: Utc::now(),
            trace_format: TraceFormat::RawText,
            pruned: false,
            template_version: None,
            timeout_reason: Some(TimeoutReason::Hard { timeout_secs: 30 }),
            terminal_reason: Some("timeout:hard".to_string()),
            api_error_status: None,
        };

        let value = serde_json::to_value(&metadata).unwrap();
        assert_eq!(value["exit_code"], 124);
        assert_eq!(value["duration_ms"], 6_125);
        assert_eq!(value["timeout_reason"]["hard"]["timeout_secs"], 30);

        let parsed: TraceMetadata = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.timeout_reason, metadata.timeout_reason);
    }

    #[test]
    fn metadata_written_before_the_envelope_fields_still_parses() {
        // cleanup_traces and prune_trace_data re-read metadata.json written by
        // older builds, which carry neither field. Deserialization must keep
        // working or those traces silently lose their retention classification.
        let legacy = r#"{
            "bead_id": "needle-legacy",
            "agent": "claude-sonnet",
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
            "exit_code": 0,
            "outcome": "failure",
            "duration_ms": 100,
            "captured_at": "2026-09-02T00:00:00Z",
            "trace_format": "claude_json",
            "pruned": false,
            "template_version": null
        }"#;
        let parsed: TraceMetadata = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.terminal_reason, None);
        assert_eq!(parsed.api_error_status, None);
    }

    #[test]
    fn trace_cleanup_old_failed_trace_deleted() {
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        std::fs::create_dir_all(&traces_dir).unwrap();

        // Create an old failed bead trace (more than 30 days ago).
        let bead_dir = traces_dir.join("needle-failed");
        std::fs::create_dir_all(&bead_dir).unwrap();

        let old_metadata = TraceMetadata {
            bead_id: BeadId::from("needle-failed"),
            agent: "test".to_string(),
            provider: None,
            model: None,
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code: 1, // Failed
            outcome: "failure".to_string(),
            duration_ms: 100,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            captured_at: Utc::now() - chrono::Duration::days(31),
            trace_format: TraceFormat::RawText,
            pruned: false,
            template_version: None,
            timeout_reason: None,
            terminal_reason: None,
            api_error_status: None,
        };
        let metadata_path = bead_dir.join("metadata.json");
        std::fs::write(
            &metadata_path,
            serde_json::to_string(&old_metadata).unwrap(),
        )
        .unwrap();

        // Run cleanup (30 days failed retention).
        let summary = cleanup_traces(&traces_dir, 30, 7).unwrap();

        assert_eq!(summary.traces_deleted, 1);
        assert_eq!(summary.traces_pruned, 0);
        assert!(!bead_dir.exists());
    }

    #[test]
    fn trace_cleanup_old_success_trace_pruned() {
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        std::fs::create_dir_all(&traces_dir).unwrap();

        // Create an old success bead trace (more than 7 days ago).
        let bead_dir = traces_dir.join("needle-success");
        std::fs::create_dir_all(&bead_dir).unwrap();

        // Create data files.
        std::fs::write(bead_dir.join("stdout.txt"), "stdout").unwrap();
        std::fs::write(bead_dir.join("stderr.txt"), "stderr").unwrap();
        std::fs::write(bead_dir.join("trace.jsonl"), "{\"event\":\"test\"}").unwrap();
        std::fs::write(bead_dir.join(TEST_OUTPUT_FILE), "test output").unwrap();

        let old_metadata = TraceMetadata {
            bead_id: BeadId::from("needle-success"),
            agent: "test".to_string(),
            provider: None,
            model: None,
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code: 0, // Success
            outcome: "success".to_string(),
            duration_ms: 100,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            captured_at: Utc::now() - chrono::Duration::days(8),
            trace_format: TraceFormat::RawText,
            pruned: false,
            template_version: None,
            timeout_reason: None,
            terminal_reason: None,
            api_error_status: None,
        };
        let metadata_path = bead_dir.join("metadata.json");
        std::fs::write(
            &metadata_path,
            serde_json::to_string(&old_metadata).unwrap(),
        )
        .unwrap();

        // Run cleanup (7 days success retention).
        let summary = cleanup_traces(&traces_dir, 30, 7).unwrap();

        assert_eq!(summary.traces_deleted, 0);
        assert_eq!(summary.traces_pruned, 1);
        assert!(bead_dir.exists());

        // Verify data files removed, metadata remains.
        assert!(!bead_dir.join("stdout.txt").exists());
        assert!(!bead_dir.join("stderr.txt").exists());
        assert!(!bead_dir.join("trace.jsonl").exists());
        assert!(!bead_dir.join(TEST_OUTPUT_FILE).exists());
        assert!(bead_dir.join("metadata.json").exists());

        // Verify metadata marked as pruned.
        let content = std::fs::read_to_string(bead_dir.join("metadata.json")).unwrap();
        let parsed: TraceMetadata = serde_json::from_str(&content).unwrap();
        assert!(parsed.pruned);
    }

    #[test]
    fn trace_cleanup_recent_trace_unchanged() {
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        std::fs::create_dir_all(&traces_dir).unwrap();

        // Create a recent trace (less than 7 days ago).
        let bead_dir = traces_dir.join("needle-recent");
        std::fs::create_dir_all(&bead_dir).unwrap();

        std::fs::write(bead_dir.join("stdout.txt"), "stdout").unwrap();

        let recent_metadata = TraceMetadata {
            bead_id: BeadId::from("needle-recent"),
            agent: "test".to_string(),
            provider: None,
            model: None,
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code: 0,
            outcome: "success".to_string(),
            duration_ms: 100,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            captured_at: Utc::now() - chrono::Duration::days(1),
            trace_format: TraceFormat::RawText,
            pruned: false,
            template_version: None,
            timeout_reason: None,
            terminal_reason: None,
            api_error_status: None,
        };
        let metadata_path = bead_dir.join("metadata.json");
        std::fs::write(
            &metadata_path,
            serde_json::to_string(&recent_metadata).unwrap(),
        )
        .unwrap();

        // Run cleanup.
        let summary = cleanup_traces(&traces_dir, 30, 7).unwrap();

        assert_eq!(summary.traces_deleted, 0);
        assert_eq!(summary.traces_pruned, 0);
        assert!(bead_dir.join("stdout.txt").exists());
    }

    #[test]
    fn trace_cleanup_missing_traces_dir_ok() {
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("nonexistent_traces");

        // Should not error on missing directory.
        let summary = cleanup_traces(&traces_dir, 30, 7).unwrap();
        assert_eq!(summary.traces_deleted, 0);
        assert_eq!(summary.traces_pruned, 0);
    }

    #[test]
    fn trace_cleanup_already_pruned_trace_skipped() {
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        std::fs::create_dir_all(&traces_dir).unwrap();

        // Create an old success trace that's already marked as pruned.
        let bead_dir = traces_dir.join("needle-already-pruned");
        std::fs::create_dir_all(&bead_dir).unwrap();

        // Metadata shows pruned: true
        let pruned_metadata = TraceMetadata {
            bead_id: BeadId::from("needle-already-pruned"),
            agent: "test".to_string(),
            provider: None,
            model: None,
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code: 0,
            outcome: "success".to_string(),
            duration_ms: 100,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            captured_at: Utc::now() - chrono::Duration::days(8), // Old enough to prune
            trace_format: TraceFormat::RawText,
            pruned: true, // Already pruned
            template_version: None,
            timeout_reason: None,
            terminal_reason: None,
            api_error_status: None,
        };
        let metadata_path = bead_dir.join("metadata.json");
        std::fs::write(
            &metadata_path,
            serde_json::to_string(&pruned_metadata).unwrap(),
        )
        .unwrap();

        // First cleanup: should skip the already-pruned trace.
        let summary1 = cleanup_traces(&traces_dir, 30, 7).unwrap();
        assert_eq!(summary1.traces_deleted, 0);
        assert_eq!(
            summary1.traces_pruned, 0,
            "already-pruned trace should not be counted"
        );

        // Second cleanup: should still skip.
        let summary2 = cleanup_traces(&traces_dir, 30, 7).unwrap();
        assert_eq!(summary2.traces_deleted, 0);
        assert_eq!(
            summary2.traces_pruned, 0,
            "already-pruned trace should not be counted again"
        );
    }

    #[test]
    fn trace_cleanup_pruned_then_not_counted_again() {
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        std::fs::create_dir_all(&traces_dir).unwrap();

        // Create an old success trace that needs pruning.
        let bead_dir = traces_dir.join("needle-will-be-pruned");
        std::fs::create_dir_all(&bead_dir).unwrap();

        std::fs::write(bead_dir.join("stdout.txt"), "stdout").unwrap();
        std::fs::write(bead_dir.join("stderr.txt"), "stderr").unwrap();

        let old_metadata = TraceMetadata {
            bead_id: BeadId::from("needle-will-be-pruned"),
            agent: "test".to_string(),
            provider: None,
            model: None,
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code: 0,
            outcome: "success".to_string(),
            duration_ms: 100,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            captured_at: Utc::now() - chrono::Duration::days(8), // Old enough to prune
            trace_format: TraceFormat::RawText,
            pruned: false, // Not yet pruned
            template_version: None,
            timeout_reason: None,
            terminal_reason: None,
            api_error_status: None,
        };
        let metadata_path = bead_dir.join("metadata.json");
        std::fs::write(
            &metadata_path,
            serde_json::to_string(&old_metadata).unwrap(),
        )
        .unwrap();

        // First cleanup: should prune the trace.
        let summary1 = cleanup_traces(&traces_dir, 30, 7).unwrap();
        assert_eq!(summary1.traces_deleted, 0);
        assert_eq!(
            summary1.traces_pruned, 1,
            "trace should be pruned on first cleanup"
        );

        // Verify files were removed and metadata marked as pruned.
        assert!(!bead_dir.join("stdout.txt").exists());
        assert!(!bead_dir.join("stderr.txt").exists());
        let content = std::fs::read_to_string(&metadata_path).unwrap();
        let parsed: TraceMetadata = serde_json::from_str(&content).unwrap();
        assert!(parsed.pruned, "metadata should be marked as pruned");

        // Second cleanup: should NOT count the same trace again.
        let summary2 = cleanup_traces(&traces_dir, 30, 7).unwrap();
        assert_eq!(summary2.traces_deleted, 0);
        assert_eq!(
            summary2.traces_pruned, 0,
            "already-pruned trace should not be counted again"
        );
    }

    #[test]
    fn trace_cleanup_partially_pruned_trace_fixed_and_not_counted() {
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        std::fs::create_dir_all(&traces_dir).unwrap();

        // Create a trace in a partially-pruned state:
        // - Data files are gone (simulating interrupted prune after file removal)
        // - Metadata still shows pruned: false
        let bead_dir = traces_dir.join("needle-partial");
        std::fs::create_dir_all(&bead_dir).unwrap();

        // DO NOT create data files - simulate they were already removed
        // in a previous interrupted prune operation

        let old_metadata = TraceMetadata {
            bead_id: BeadId::from("needle-partial"),
            agent: "test".to_string(),
            provider: None,
            model: None,
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code: 0,
            outcome: "success".to_string(),
            duration_ms: 100,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            captured_at: Utc::now() - chrono::Duration::days(8), // Old enough to prune
            trace_format: TraceFormat::RawText,
            pruned: false, // NOT marked as pruned (partial state)
            template_version: None,
            timeout_reason: None,
            terminal_reason: None,
            api_error_status: None,
        };
        let metadata_path = bead_dir.join("metadata.json");
        std::fs::write(
            &metadata_path,
            serde_json::to_string(&old_metadata).unwrap(),
        )
        .unwrap();

        // First cleanup: should fix metadata and NOT count as pruned
        // (because no data files were actually removed)
        let summary1 = cleanup_traces(&traces_dir, 30, 7).unwrap();
        assert_eq!(summary1.traces_deleted, 0);
        assert_eq!(
            summary1.traces_pruned, 0,
            "partially-pruned trace should not be counted"
        );

        // Verify metadata was fixed
        let content = std::fs::read_to_string(&metadata_path).unwrap();
        let parsed: TraceMetadata = serde_json::from_str(&content).unwrap();
        assert!(
            parsed.pruned,
            "metadata should be marked as pruned after fix"
        );

        // Second cleanup: should still skip (now properly marked as pruned)
        let summary2 = cleanup_traces(&traces_dir, 30, 7).unwrap();
        assert_eq!(summary2.traces_deleted, 0);
        assert_eq!(
            summary2.traces_pruned, 0,
            "fixed trace should not be counted again"
        );
    }

    #[test]
    fn trace_cleanup_finishes_interrupted_metadata_first_prune() {
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        let bead_dir = traces_dir.join("needle-interrupted");
        std::fs::create_dir_all(&bead_dir).unwrap();

        std::fs::write(bead_dir.join("stdout.txt"), "stdout").unwrap();
        std::fs::write(bead_dir.join("stderr.txt"), "stderr").unwrap();
        std::fs::write(bead_dir.join("trace.jsonl"), "{\"event\":\"test\"}").unwrap();
        std::fs::write(bead_dir.join(TEST_OUTPUT_FILE), "test output").unwrap();

        let metadata = TraceMetadata {
            bead_id: BeadId::from("needle-interrupted"),
            agent: "test".to_string(),
            provider: None,
            model: None,
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code: 0,
            outcome: "success".to_string(),
            duration_ms: 100,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            captured_at: Utc::now() - chrono::Duration::days(8),
            trace_format: TraceFormat::RawText,
            pruned: true,
            template_version: None,
            timeout_reason: None,
            terminal_reason: None,
            api_error_status: None,
        };
        std::fs::write(
            bead_dir.join("metadata.json"),
            serde_json::to_string(&metadata).unwrap(),
        )
        .unwrap();

        let summary = cleanup_traces(&traces_dir, 30, 7).unwrap();

        assert_eq!(summary.traces_deleted, 0);
        assert_eq!(summary.traces_pruned, 0);
        assert!(!bead_dir.join("stdout.txt").exists());
        assert!(!bead_dir.join("stderr.txt").exists());
        assert!(!bead_dir.join("trace.jsonl").exists());
        assert!(!bead_dir.join(TEST_OUTPUT_FILE).exists());
        assert!(bead_dir.join("metadata.json").exists());
    }

    fn attempt_metadata() -> TraceMetadata {
        TraceMetadata {
            bead_id: test_bead_id(),
            agent: "probe".to_string(),
            provider: Some("anthropic".to_string()),
            model: Some("glm-4.7".to_string()),
            requested_model: Some("glm-4.7".to_string()),
            effective_model: Some("glm-5.3-flash".to_string()),
            model_resolution_source: Some("claude_message.model".to_string()),
            exit_code: 0,
            outcome: "success".to_string(),
            duration_ms: 10,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            captured_at: Utc::now(),
            trace_format: TraceFormat::RawText,
            pruned: false,
            template_version: None,
            timeout_reason: None,
            terminal_reason: None,
            api_error_status: None,
        }
    }

    fn read_raw_metadata(capture: &TraceCapture) -> serde_json::Value {
        let content = std::fs::read_to_string(capture.trace_dir().join("metadata.json")).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[test]
    fn trace_metadata_carries_the_bound_attempt_identity() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture =
            TraceCapture::new(&test_bead_id(), beads_root, Some("0192-attempt-identity")).unwrap();

        capture.write_metadata(&attempt_metadata()).unwrap();

        // The attempt id is injected outside `TraceMetadata`, so read the raw
        // document: parsing into the struct would silently drop the key.
        let raw = read_raw_metadata(&capture);
        assert_eq!(raw["attempt_id"], "0192-attempt-identity");
        assert_eq!(raw["model"], "glm-4.7");
        assert_eq!(raw["requested_model"], "glm-4.7");
        assert_eq!(raw["effective_model"], "glm-5.3-flash");
        assert_eq!(raw["model_resolution_source"], "claude_message.model");
    }

    #[test]
    fn trace_metadata_carries_only_redacted_claim_handle_facts() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let mut capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();
        capture.bind_claim_handle(ClaimHandleMetadata {
            bead_id: test_bead_id(),
            target_workspace_identity: "workspace-sha256".to_string(),
            assignee: "worker-a".to_string(),
            starting_revision: Some(7),
            current_revision: Some(8),
            claim_epoch: Some(3),
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(60)),
            capabilities: crate::claim::ClaimCapabilities {
                fenced_claim: true,
                renewable_lease: true,
                guarded_mutations: true,
                credential_stdin: true,
            },
            protected: true,
        });
        capture.write_metadata(&attempt_metadata()).unwrap();

        let raw = read_raw_metadata(&capture);
        assert_eq!(raw["claim_handle"]["bead_id"], test_bead_id().as_ref());
        assert_eq!(raw["claim_handle"]["current_revision"], 8);
        assert_eq!(raw["claim_handle"]["protected"], true);
        assert!(raw["claim_handle"].get("fencing_credential").is_none());
    }

    #[test]
    fn trace_metadata_has_no_attempt_id_until_one_is_bound() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root, None).unwrap();
        capture.write_metadata(&attempt_metadata()).unwrap();

        // An unbound capture must not invent an identity: a missing key is
        // the observable shape of missing propagation, never a placeholder.
        let raw = read_raw_metadata(&capture);
        assert!(raw.get("attempt_id").is_none());
    }

    #[test]
    fn trace_attempt_identity_survives_while_content_is_sanitized() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let sanitizer = Arc::new(
            Sanitizer::new(&[crate::sanitize::CustomPattern {
                id: "test-token".to_string(),
                pattern: "sk-test-[a-z0-9]+".to_string(),
                entropy: None,
            }])
            .unwrap(),
        );
        let capture = TraceCapture::new_with_sanitizer(
            &test_bead_id(),
            beads_root,
            Some("0192-attempt-identity"),
            Some(sanitizer),
        )
        .unwrap();

        capture
            .write_stdout("token sk-test-abc123 leaked into output")
            .unwrap();
        capture.write_metadata(&attempt_metadata()).unwrap();

        let stdout = std::fs::read_to_string(capture.trace_dir().join("stdout.txt")).unwrap();
        assert!(
            !stdout.contains("sk-test-abc123"),
            "sanitizer must still redact content: {stdout}"
        );
        assert!(stdout.contains("[REDACTED:test-token]"));

        // Identity propagation must not open a redaction bypass: the
        // metadata key is present even though every content file is
        // sanitized.
        let raw = read_raw_metadata(&capture);
        assert_eq!(raw["attempt_id"], "0192-attempt-identity");
    }

    // ── Attempt-scoped trace directories ──

    fn retention_metadata(bead_id: &str, exit_code: i32, age_days: i64) -> TraceMetadata {
        TraceMetadata {
            bead_id: BeadId::from(bead_id),
            agent: "test".to_string(),
            provider: None,
            model: None,
            requested_model: None,
            effective_model: None,
            model_resolution_source: None,
            exit_code,
            outcome: if exit_code == 0 {
                "success".to_string()
            } else {
                "failure".to_string()
            },
            duration_ms: 100,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            captured_at: Utc::now() - chrono::Duration::days(age_days),
            trace_format: TraceFormat::RawText,
            pruned: false,
            template_version: None,
            timeout_reason: None,
            terminal_reason: None,
            api_error_status: None,
        }
    }

    /// Stage one complete attempt capture for the retention fixtures.
    fn stage_attempt(
        bead_dir: &Path,
        attempt_id: &str,
        exit_code: i32,
        age_days: i64,
    ) -> std::path::PathBuf {
        let attempt_dir = bead_dir.join(attempt_id);
        std::fs::create_dir_all(&attempt_dir).unwrap();
        std::fs::write(attempt_dir.join(STDOUT_FILE), "stdout").unwrap();
        std::fs::write(attempt_dir.join(STDERR_FILE), "stderr").unwrap();
        std::fs::write(attempt_dir.join("trace.jsonl"), "{\"event\":\"test\"}").unwrap();
        std::fs::write(
            attempt_dir.join("metadata.json"),
            serde_json::to_string(&retention_metadata(
                bead_dir.file_name().unwrap().to_str().unwrap(),
                exit_code,
                age_days,
            ))
            .unwrap(),
        )
        .unwrap();
        attempt_dir
    }

    #[test]
    fn attempt_scoped_capture_writes_into_attempt_directory() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(
            &test_bead_id(),
            beads_root,
            Some("0192a6f0-0000-7000-8000-000000000001"),
        )
        .unwrap();

        assert!(capture
            .trace_dir()
            .ends_with("traces/needle-test/0192a6f0-0000-7000-8000-000000000001"));
        assert!(capture.trace_dir().is_dir());
        // The flat bead directory is a pure parent here, not the capture.
        assert!(!capture
            .trace_dir()
            .parent()
            .unwrap()
            .join(STDOUT_FILE)
            .exists());
    }

    #[test]
    fn unsafe_attempt_ids_fall_back_to_the_flat_layout() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        for unusable in ["", "  ", "..", "a/b", "a\\b"] {
            let capture = TraceCapture::new(&test_bead_id(), beads_root, Some(unusable)).unwrap();
            assert!(
                capture.trace_dir().ends_with("traces/needle-test"),
                "attempt id {unusable:?} must not become a path segment"
            );
        }
    }

    #[test]
    fn attempt_scoped_dir_appends_only_usable_components() {
        let bead_dir = Path::new("/workspace/.beads/traces/needle-test");
        assert_eq!(
            attempt_scoped_dir(bead_dir, Some("0192a6f0-0000-7000-8000-000000000001")),
            bead_dir.join("0192a6f0-0000-7000-8000-000000000001")
        );
        // No attempt identity resolves to the bead directory itself.
        assert_eq!(attempt_scoped_dir(bead_dir, None), bead_dir);
        for unusable in ["", "  ", ".", "..", "a/b", "a\\b"] {
            assert_eq!(
                attempt_scoped_dir(bead_dir, Some(unusable)),
                bead_dir,
                "attempt id {unusable:?} must not scope the directory"
            );
        }
    }

    #[test]
    fn two_dispatches_produce_two_complete_attempt_directories() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        // First dispatch of the bead.
        let first = TraceCapture::new(
            &test_bead_id(),
            beads_root,
            Some("0192a6f0-0000-7000-8000-000000000001"),
        )
        .unwrap();
        first.write_stdout("first stdout").unwrap();
        first.write_stderr("first stderr").unwrap();
        first
            .write_trace_jsonl(&[r#"{"event":"first"}"#.to_string()])
            .unwrap();

        // Retry: a new attempt id must land in a new directory, not overwrite.
        let second = TraceCapture::new(
            &test_bead_id(),
            beads_root,
            Some("0192a6f0-0000-7000-8000-000000000002"),
        )
        .unwrap();
        second.write_stdout("second stdout").unwrap();
        second.write_stderr("second stderr").unwrap();
        second
            .write_trace_jsonl(&[r#"{"event":"second"}"#.to_string()])
            .unwrap();

        let first_dir = beads_root
            .join(".beads")
            .join("traces")
            .join("needle-test")
            .join("0192a6f0-0000-7000-8000-000000000001");
        let second_dir = beads_root
            .join(".beads")
            .join("traces")
            .join("needle-test")
            .join("0192a6f0-0000-7000-8000-000000000002");

        assert!(first_dir.is_dir() && second_dir.is_dir());
        assert_eq!(
            std::fs::read_to_string(first_dir.join(STDOUT_FILE)).unwrap(),
            "first stdout"
        );
        assert_eq!(
            std::fs::read_to_string(first_dir.join(STDERR_FILE)).unwrap(),
            "first stderr"
        );
        assert_eq!(
            std::fs::read_to_string(first_dir.join("trace.jsonl")).unwrap(),
            r#"{"event":"first"}"#
        );
        assert_eq!(
            std::fs::read_to_string(second_dir.join(STDOUT_FILE)).unwrap(),
            "second stdout"
        );
        assert_eq!(
            std::fs::read_to_string(second_dir.join(STDERR_FILE)).unwrap(),
            "second stderr"
        );
        assert_eq!(
            std::fs::read_to_string(second_dir.join("trace.jsonl")).unwrap(),
            r#"{"event":"second"}"#
        );
    }

    #[test]
    fn retention_prunes_aged_attempt_and_spares_newer_sibling() {
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        let bead_dir = traces_dir.join("needle-retried");
        std::fs::create_dir_all(&bead_dir).unwrap();

        let old_attempt = stage_attempt(&bead_dir, "0192a6f0-0000-7000-8000-000000000001", 0, 8);
        let new_attempt = stage_attempt(&bead_dir, "0192a6f0-0000-7000-8000-000000000002", 0, 1);

        let summary = cleanup_traces(&traces_dir, 30, 7).unwrap();

        assert_eq!(summary.traces_deleted, 0);
        assert_eq!(summary.traces_pruned, 1, "only the aged attempt is pruned");

        // Aged attempt: metadata-only.
        assert!(!old_attempt.join(STDOUT_FILE).exists());
        assert!(!old_attempt.join(STDERR_FILE).exists());
        assert!(!old_attempt.join("trace.jsonl").exists());
        assert!(old_attempt.join("metadata.json").exists());
        let pruned: TraceMetadata = serde_json::from_str(
            &std::fs::read_to_string(old_attempt.join("metadata.json")).unwrap(),
        )
        .unwrap();
        assert!(pruned.pruned);

        // Newer attempt of the same bead: untouched.
        assert!(new_attempt.join(STDOUT_FILE).exists());
        assert!(new_attempt.join(STDERR_FILE).exists());
        assert!(new_attempt.join("trace.jsonl").exists());
        assert!(new_attempt.join("metadata.json").exists());
        let fresh: TraceMetadata = serde_json::from_str(
            &std::fs::read_to_string(new_attempt.join("metadata.json")).unwrap(),
        )
        .unwrap();
        assert!(!fresh.pruned);
    }

    #[test]
    fn retention_deletes_aged_failed_attempts_and_the_empty_bead_dir() {
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        let bead_dir = traces_dir.join("needle-flaky");
        std::fs::create_dir_all(&bead_dir).unwrap();

        let old_failure = stage_attempt(&bead_dir, "0192a6f0-0000-7000-8000-000000000001", 1, 31);
        // A recent failed retry stays: each attempt is retained on its own age.
        let recent_retry = stage_attempt(&bead_dir, "0192a6f0-0000-7000-8000-000000000002", 1, 1);

        let summary = cleanup_traces(&traces_dir, 30, 7).unwrap();

        assert_eq!(summary.traces_deleted, 1);
        assert!(!old_failure.exists());
        assert!(recent_retry.join("metadata.json").exists());
        assert!(bead_dir.is_dir(), "the bead dir survives its live attempts");

        // Once the retry also ages out, the bead directory goes with it.
        std::fs::write(
            recent_retry.join("metadata.json"),
            serde_json::to_string(&retention_metadata("needle-flaky", 1, 31)).unwrap(),
        )
        .unwrap();
        let summary = cleanup_traces(&traces_dir, 30, 7).unwrap();
        assert_eq!(summary.traces_deleted, 1);
        assert!(!bead_dir.exists(), "no empty bead-dir shell remains");
    }

    #[test]
    fn retention_prunes_legacy_flat_and_attempt_layouts_side_by_side() {
        // A bead directory can hold both a legacy flat capture (older binary)
        // and attempt directories (this change); each is retained on its own
        // metadata, and nothing migrates the flat files.
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        let bead_dir = traces_dir.join("needle-mixed");
        std::fs::create_dir_all(&bead_dir).unwrap();

        // Legacy flat capture, success, past retention.
        std::fs::write(bead_dir.join(STDOUT_FILE), "legacy stdout").unwrap();
        std::fs::write(bead_dir.join("trace.jsonl"), "{\"event\":\"legacy\"}").unwrap();
        std::fs::write(
            bead_dir.join("metadata.json"),
            serde_json::to_string(&retention_metadata("needle-mixed", 0, 8)).unwrap(),
        )
        .unwrap();

        // Attempt capture, success, past retention too.
        let attempt = stage_attempt(&bead_dir, "0192a6f0-0000-7000-8000-000000000001", 0, 8);

        let summary = cleanup_traces(&traces_dir, 30, 7).unwrap();

        assert_eq!(summary.traces_deleted, 0);
        assert_eq!(
            summary.traces_pruned, 2,
            "flat and attempt captures each prune"
        );

        assert!(!bead_dir.join(STDOUT_FILE).exists());
        assert!(!bead_dir.join("trace.jsonl").exists());
        assert!(bead_dir.join("metadata.json").exists());
        assert!(!attempt.join(STDOUT_FILE).exists());
        assert!(attempt.join("metadata.json").exists());
    }

    #[test]
    fn retention_failed_flat_capture_spares_attempt_dirs() {
        // A legacy failed capture aging out must not take newer attempt
        // captures with it: the flat capture's own files go, the bead
        // directory and the fresh attempt stay.
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        let bead_dir = traces_dir.join("needle-mixed-failed");
        std::fs::create_dir_all(&bead_dir).unwrap();

        // Legacy flat capture: failed, past the 30-day failure retention.
        std::fs::write(bead_dir.join(STDOUT_FILE), "legacy stdout").unwrap();
        std::fs::write(bead_dir.join("trace.jsonl"), "{\"event\":\"legacy\"}").unwrap();
        std::fs::write(
            bead_dir.join("metadata.json"),
            serde_json::to_string(&retention_metadata("needle-mixed-failed", 1, 31)).unwrap(),
        )
        .unwrap();

        // Fresh attempt of the same bead.
        let fresh = stage_attempt(&bead_dir, "0192a6f0-0000-7000-8000-000000000002", 0, 1);

        let summary = cleanup_traces(&traces_dir, 30, 7).unwrap();

        assert_eq!(summary.traces_deleted, 1);
        // The flat capture is deleted file by file...
        assert!(!bead_dir.join(STDOUT_FILE).exists());
        assert!(!bead_dir.join("trace.jsonl").exists());
        assert!(!bead_dir.join("metadata.json").exists());
        // ...while the fresh attempt and its bead directory survive.
        assert!(fresh.join(STDOUT_FILE).exists());
        assert!(fresh.join("metadata.json").exists());
        assert!(bead_dir.is_dir());
    }

    #[test]
    fn retention_leaves_bead_journal_files_alone() {
        // attempts.jsonl / lessons.jsonl live at the bead level and are not
        // trace data: cleanup must neither prune around them into deletion
        // nor empty the bead directory while they exist.
        let temp_dir = TempDir::new().unwrap();
        let traces_dir = temp_dir.path().join("traces");
        let bead_dir = traces_dir.join("needle-journaled");
        std::fs::create_dir_all(&bead_dir).unwrap();
        std::fs::write(bead_dir.join("attempts.jsonl"), "{}\n").unwrap();
        std::fs::write(bead_dir.join("lessons.jsonl"), "{}\n").unwrap();

        let old_attempt = stage_attempt(&bead_dir, "0192a6f0-0000-7000-8000-000000000001", 1, 31);

        let summary = cleanup_traces(&traces_dir, 30, 7).unwrap();

        assert_eq!(summary.traces_deleted, 1);
        assert!(!old_attempt.exists());
        assert!(bead_dir.join("attempts.jsonl").exists());
        assert!(bead_dir.join("lessons.jsonl").exists());
        assert!(bead_dir.is_dir());
    }
}
