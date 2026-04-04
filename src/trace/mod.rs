//! Trace capture: adapter-specific structured trace collection.
//!
//! This module captures full execution traces from agent runs including
//! tool calls, agent reasoning, and verifier output. Traces are stored
//! in `.beads/traces/<bead-id>/` with structured metadata.
//!
//! ## Trace Retention Policy
//!
//! - **Failed beads**: 30 days (full trace retained)
//! - **Successful beads**: metadata-only after 7 days (trace data pruned)
//!
//! ## Directory Structure
//!
//! ```text
//! .beads/traces/<bead-id>/
//! ├── trace.jsonl     # Structured trace events (one JSON object per line)
//! ├── stdout.txt      # Raw stdout from agent process
//! ├── stderr.txt      # Raw stderr from agent process
//! └── metadata.json   # Timing, tokens, cost, template version
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::sanitize::Sanitizer;
use crate::types::BeadId;

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
    pub fn write_stdout(&self, stdout: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let content = self.sanitize(stdout);
        let path = self.trace_dir.join("stdout.txt");
        std::fs::write(&path, content.as_bytes())
            .with_context(|| format!("failed to write stdout trace: {}", path.display()))
    }

    /// Write stderr to `stderr.txt`.
    ///
    /// Content is sanitized before writing if a sanitizer is configured.
    pub fn write_stderr(&self, stderr: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let content = self.sanitize(stderr);
        let path = self.trace_dir.join("stderr.txt");
        std::fs::write(&path, content.as_bytes())
            .with_context(|| format!("failed to write stderr trace: {}", path.display()))
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
    /// Deletes trace.jsonl, stdout.txt, and stderr.txt, keeping only metadata.json.
    /// Updates the `pruned` flag in metadata.
    pub fn prune_trace_data(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Delete trace data files.
        for file in ["trace.jsonl", "stdout.txt", "stderr.txt"] {
            let path = self.trace_dir.join(file);
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("failed to prune trace file: {}", path.display()))?;
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
/// - Failed beads (non-zero exit): delete after 30 days
/// - Successful beads (exit 0): prune data after 7 days, keep metadata only
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

        let should_delete = is_failed && age_days > retention_days_failed as u64;
        let should_prune = !is_failed && age_days > retention_days_success as u64;

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
            } else {
                summary.traces_pruned += 1;
            }
        }
    }

    Ok(summary)
}

/// Prune trace data files in a directory, keeping only metadata.json.
fn prune_trace_dir(trace_dir: &Path) -> Result<()> {
    for file in ["trace.jsonl", "stdout.txt", "stderr.txt"] {
        let path = trace_dir.join(file);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to prune trace file: {}", path.display()))?;
        }
    }

    // Update metadata to mark as pruned.
    let metadata_path = trace_dir.join("metadata.json");
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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
        assert!(capture.trace_dir().exists());
        assert!(capture.trace_dir().ends_with("traces/needle-test"));
    }

    #[test]
    fn trace_capture_writes_stdout() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();

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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
        capture.write_stderr("error output").unwrap();

        let stderr_path = capture.trace_dir().join("stderr.txt");
        assert!(stderr_path.exists());
        let content = std::fs::read_to_string(stderr_path).unwrap();
        assert_eq!(content, "error output");
    }

    #[test]
    fn trace_capture_writes_trace_jsonl() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();

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

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
        assert!(capture.trace_dir().exists());

        capture.delete().unwrap();
        assert!(!capture.trace_dir().exists());
    }

    #[test]
    fn trace_capture_prune_removes_data_keeps_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let beads_root = temp_dir.path();

        let capture = TraceCapture::new(&test_bead_id(), beads_root).unwrap();
        capture.write_stdout("stdout").unwrap();
        capture.write_stderr("stderr").unwrap();

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
        };
        capture.write_metadata(&metadata).unwrap();

        // Verify files exist.
        assert!(capture.trace_dir().join("stdout.txt").exists());
        assert!(capture.trace_dir().join("stderr.txt").exists());
        assert!(capture.trace_dir().join("metadata.json").exists());

        // Prune.
        capture.prune_trace_data().unwrap();

        // Verify data files removed, metadata remains.
        assert!(!capture.trace_dir().join("stdout.txt").exists());
        assert!(!capture.trace_dir().join("stderr.txt").exists());
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
}
