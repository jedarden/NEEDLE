//! Trace capture: adapter-specific structured trace collection.
//!
//! This module captures full execution traces from agent runs including
//! tool calls, agent reasoning, and verifier output. Traces are stored
//! in `.beads/traces/<bead-id>/` with structured metadata.
//!
//! ## Trace Retention Policy
//!
//! - **Failed beads**: 7 days (full trace retained)
//! - **Successful beads**: metadata-only after 1 day (trace data pruned)
//!
//! ## Directory Structure
//!
//! ```text
//! .beads/traces/<bead-id>/
//! ├── trace.jsonl     # Structured trace events (one JSON object per line)
//! ├── stdout.txt      # Raw stdout from agent process
//! ├── stderr.txt      # Raw stderr from agent process
//! ├── test-output.txt # Processed test output (for test runs)
//! └── metadata.json   # Timing, tokens, cost, template version
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::cargo_test::TestMetrics;
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
    /// Model identifier (e.g., "claude-sonnet-4-6").
    pub model: Option<String>,
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
}

/// Adapter-specific trace format identifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceFormat {
    /// Claude Code JSON output format.
    ClaudeJson,
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
    /// Trace directory for this bead (`.beads/traces/<bead-id>`).
    trace_dir: PathBuf,
    /// Whether trace capture is enabled.
    enabled: bool,
    /// Optional sanitizer applied to all content before writing to disk.
    sanitizer: Option<Arc<Sanitizer>>,
}

impl TraceCapture {
    /// Create a new trace capture for a bead without sanitization.
    ///
    /// `beads_root` is the workspace directory containing `.beads/`.
    /// Returns `None` if trace capture is disabled.
    pub fn new(bead_id: &BeadId, beads_root: &Path) -> Option<Self> {
        Self::new_with_sanitizer(bead_id, beads_root, None)
    }

    /// Create a new trace capture for a bead with an optional sanitizer.
    ///
    /// When `sanitizer` is `Some`, all trace content is sanitized synchronously
    /// before writing to disk (no unsanitized window on disk).
    pub fn new_with_sanitizer(
        bead_id: &BeadId,
        beads_root: &Path,
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
        let trace_dir = beads_root
            .join(".beads")
            .join("traces")
            .join(bead_id.as_ref());

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
        })
    }

    /// Get the trace directory path.
    pub fn trace_dir(&self) -> &Path {
        &self.trace_dir
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
        let json =
            serde_json::to_string_pretty(metadata).context("failed to serialize trace metadata")?;
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
        n if n.starts_with("claude-") => TraceFormat::ClaudeJson,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeResultEnvelope {
    /// Envelope `is_error` flag.
    pub is_error: bool,
    /// Envelope `subtype`. Kept for diagnostics only — it is NOT a usable
    /// failure signal (see the struct doc).
    pub subtype: Option<String>,
    /// Envelope `terminal_reason`, when the CLI reports why the session ended
    /// (e.g. `"api_error"`).
    pub terminal_reason: Option<String>,
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
            })
        })
}

/// Whether a stream's final result envelope reports a terminal failure.
///
/// `false` when the stream carries no result envelope — callers without one
/// fall back to the exit code.
pub fn stream_indicates_failure(stdout: &str) -> bool {
    parse_result_envelope(stdout)
        .map(|envelope| envelope.indicates_failure())
        .unwrap_or(false)
}

/// Classify an outcome from the result envelope when the trace format carries
/// one, falling back to the exit code for formats that do not.
///
/// The exit code alone misclassifies claude runs that ended on a terminal API
/// error — the CLI exits 0 — so the envelope wins whenever it exists.
pub fn classify_from_stream(exit_code: i32, stdout: &str, format: &TraceFormat) -> Outcome {
    if *format == TraceFormat::ClaudeJson && stream_indicates_failure(stdout) {
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
            if let Err(e) = fix_pruned_metadata(&path) {
                tracing::debug!(
                    path = %path.display(),
                    error = %e,
                    "failed to fix pruned metadata for partially-pruned trace"
                );
            }
        }

        if should_delete {
            // Delete entire trace directory.
            if let Err(e) = std::fs::remove_dir_all(&path) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to delete old trace directory"
                );
            } else {
                summary.traces_deleted += 1;
            }
        } else if should_prune {
            // Prune trace data, keep metadata.
            if let Err(e) = prune_trace_dir(&path) {
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

    Ok(summary)
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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
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
        let capture = TraceCapture::new(&bead_id, beads_root);
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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();

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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
        let metadata = TraceMetadata {
            bead_id: test_bead_id(),
            agent: "claude-sonnet".to_string(),
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
        assert!(capture.trace_dir().exists());

        capture.delete().unwrap();
        assert!(!capture.trace_dir().exists());
    }

    #[test]
    fn trace_capture_write_stdout_handles_errors_gracefully() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();
        std::fs::create_dir_all(beads_root.join(".beads")).unwrap();

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();

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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
        capture.write_stdout("stdout").unwrap();
        capture.write_stderr("stderr").unwrap();
        capture.write_test_output("test output").unwrap();

        let metadata = TraceMetadata {
            bead_id: test_bead_id(),
            agent: "test".to_string(),
            provider: None,
            model: None,
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
    fn parse_result_envelope_finds_the_final_result_line() {
        let stdout = result_stream(API_ERROR_RESULT_LINE);
        let envelope = parse_result_envelope(&stdout).expect("envelope should parse");

        assert!(envelope.is_error);
        assert_eq!(envelope.subtype.as_deref(), Some("success"));
        assert_eq!(envelope.terminal_reason.as_deref(), Some("api_error"));
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
        };
        assert!(envelope.indicates_failure());
    }

    #[test]
    fn envelope_success_is_not_a_failure() {
        let envelope = ClaudeResultEnvelope {
            is_error: false,
            subtype: Some("success".to_string()),
            terminal_reason: None,
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
        };
        assert!(!envelope.indicates_failure());
    }

    #[test]
    fn envelope_empty_terminal_reason_is_not_a_failure() {
        let envelope = ClaudeResultEnvelope {
            is_error: false,
            subtype: Some("success".to_string()),
            terminal_reason: Some(String::new()),
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
        };

        let json = serde_json::to_string(&metadata).unwrap();
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
}
