//! Per-bead attempt history: what the next attempt is told about the last ones.
//!
//! Before this module a failed attempt taught its successor nothing. The gate
//! output was computed, fingerprinted and written to telemetry, gate-health
//! state and the trace directory — and the next agent received a
//! byte-identical prompt. Only labels (`failure-count:N`, `verification-failed`,
//! `quarantine-until:`) survived a failure, and none of them says *why*.
//!
//! This is plan revision 31 leaf R3 (`needle-60163eac`): the admitted recovery
//! context is injected into the next attempt. It is deliberately the smallest
//! shape that closes the loop — a bounded, append-only record per attempt and
//! a rendering of the newest few into the dispatch prompt — and it does not
//! wait for the checkpoint journal or the learning inbox (C1–C5, D1–D6).
//!
//! Two stores, one truth:
//!
//! - **Local journal** `<workspace>/.beads/traces/<bead-id>/attempts.jsonl`,
//!   next to the attempt's own trace. Mend prunes `trace.jsonl`/`stdout.txt`
//!   from that directory but never the directory itself, so the journal
//!   outlives the transcript. It is gitignored with the rest of `traces/`.
//! - **Cross-host mirror** in bead-rs structured data, namespace
//!   [`DATA_NAMESPACE`], so a worker on another host — or a fresh clone —
//!   sees the same history. It survives `bead update --notes`, which
//!   *replaces* notes and is what the prompt tells agents to run.
//!
//! The record carries what the ledger row carries plus a bounded, sanitized
//! failure summary. It never carries a transcript, model reasoning, or a
//! credential: the summary is gate output or stderr already passed through
//! the trace sanitizer's caps, cut to a fixed size.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::bead_store::BeadStore;
use crate::types::BeadId;

/// Schema version stamped on every record.
pub const SCHEMA_VERSION: u32 = 1;
/// bead-rs structured-data namespace holding the cross-host mirror.
pub const DATA_NAMESPACE: &str = "needle-attempts";
/// Immutable schema reference declared with the structured data.
pub const DATA_SCHEMA_REF: &str = "urn:needle:schema:attempt-history:v1";
/// Records kept in the local journal before the oldest are dropped.
const LOCAL_KEEP: usize = 50;
/// Records mirrored into bead-rs structured data.
const MIRROR_KEEP: usize = 5;
/// Bytes of failure summary kept per record in the mirror.
const MIRROR_SUMMARY_BYTES: usize = 600;
/// Bytes of failure summary kept per record in the local journal.
pub const LOCAL_SUMMARY_BYTES: usize = 1200;
/// Semantic outcome of an attempt whose evidence was accepted.
const VERIFIED_SUCCESS: &str = "verified_success";

/// One resolved attempt, as the next attempt should see it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttemptRecord {
    /// [`SCHEMA_VERSION`] at the time of writing.
    pub schema_version: u32,
    /// The `attempt.resolved` ledger row's attempt ID.
    pub attempt_id: String,
    /// RFC 3339 time the attempt resolved.
    pub recorded_at: String,
    /// Worker that ran the attempt.
    pub worker: String,
    /// Adapter that executed it (e.g. `claude-code-glm-5.3-flash`).
    pub adapter: String,
    /// Model identifier when the adapter declares one.
    #[serde(default)]
    pub model: Option<String>,
    /// Semantic outcome class (`verified_success`, `work_failure`,
    /// `infrastructure_failure`, `indeterminate`, `cancelled`).
    pub outcome: String,
    /// Machine-readable terminal reason (`gate:definition-of-done`,
    /// `exit_code:1`, `timeout`, `signal:9`).
    #[serde(default)]
    pub terminal_reason: Option<String>,
    /// Process exit code (observation only).
    pub exit_code: i32,
    /// Lifecycle action NEEDLE requested afterwards (`Released`, `Deferred`…).
    pub requested_action: String,
    /// Commits the attempt produced in the workspace.
    #[serde(default)]
    pub commits: Vec<String>,
    /// Wall-clock duration of the attempt.
    pub duration_ms: u64,
    /// Bounded, sanitized failure text — the part the next agent must read.
    #[serde(default)]
    pub failure_summary: Option<String>,
}

impl AttemptRecord {
    /// Whether this attempt's evidence was accepted.
    pub fn is_verified_success(&self) -> bool {
        self.outcome == VERIFIED_SUCCESS
    }
}

/// How much history the prompt renderer may spend.
#[derive(Debug, Clone, Copy)]
pub struct HistoryLimits {
    /// Newest attempts rendered.
    pub max_attempts: usize,
    /// Byte cap on the rendered section.
    pub max_bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        HistoryLimits {
            max_attempts: 3,
            max_bytes: 4000,
        }
    }
}

/// Path of the local journal for `bead_id` in `workspace`.
pub fn history_path(workspace: &Path, bead_id: &BeadId) -> PathBuf {
    workspace
        .join(".beads")
        .join("traces")
        .join(bead_id.as_ref())
        .join("attempts.jsonl")
}

/// Append `record` to the local journal, keeping the newest [`LOCAL_KEEP`].
pub fn append_local(workspace: &Path, bead_id: &BeadId, record: &AttemptRecord) -> Result<()> {
    let path = history_path(workspace, bead_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut records = load_local(workspace, bead_id).unwrap_or_default();
    records.push(record.clone());
    if records.len() > LOCAL_KEEP {
        let drop = records.len() - LOCAL_KEEP;
        records.drain(..drop);
    }
    let mut body = String::new();
    for r in &records {
        body.push_str(&serde_json::to_string(r)?);
        body.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

/// Load the local journal, oldest first. A missing file is an empty history;
/// a malformed line is skipped rather than failing the whole read.
pub fn load_local(workspace: &Path, bead_id: &BeadId) -> Result<Vec<AttemptRecord>> {
    let path = history_path(workspace, bead_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<AttemptRecord>(l).ok())
        .collect())
}

/// The bounded mirror written to bead-rs structured data: the newest
/// [`MIRROR_KEEP`] records with summaries cut to [`MIRROR_SUMMARY_BYTES`].
pub fn to_data_value(records: &[AttemptRecord]) -> serde_json::Value {
    let start = records.len().saturating_sub(MIRROR_KEEP);
    let attempts: Vec<AttemptRecord> = records[start..]
        .iter()
        .map(|r| AttemptRecord {
            failure_summary: r
                .failure_summary
                .as_deref()
                .map(|s| truncate_head_tail(s, MIRROR_SUMMARY_BYTES)),
            ..r.clone()
        })
        .collect();
    serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "total_attempts": records.len(),
        "attempts": attempts,
    })
}

/// Parse a mirror written by [`to_data_value`]. Unknown shapes yield nothing.
pub fn from_data_value(value: &serde_json::Value) -> Vec<AttemptRecord> {
    value
        .get("attempts")
        .and_then(|a| a.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| serde_json::from_value::<AttemptRecord>(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Load the history a prompt should show: the local journal first, the
/// cross-host mirror when the local journal is empty. Never fails — a store
/// error is logged and yields an empty history.
pub async fn load_for_prompt(
    workspace: &Path,
    bead_id: &BeadId,
    store: &dyn BeadStore,
) -> Vec<AttemptRecord> {
    match load_local(workspace, bead_id) {
        Ok(records) if !records.is_empty() => return records,
        Ok(_) => {}
        Err(e) => tracing::debug!(
            bead_id = %bead_id,
            error = %e,
            "attempt history: local journal unreadable, trying the bead-rs mirror"
        ),
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        store.get_data(bead_id, DATA_NAMESPACE),
    )
    .await
    {
        Ok(Ok(Some(value))) => from_data_value(&value),
        Ok(Ok(None)) => Vec::new(),
        Ok(Err(e)) => {
            tracing::debug!(bead_id = %bead_id, error = %e, "attempt history: mirror read failed");
            Vec::new()
        }
        Err(_) => {
            tracing::debug!(bead_id = %bead_id, "attempt history: mirror read timed out");
            Vec::new()
        }
    }
}

/// Persist one resolved attempt: append it locally and refresh the mirror.
///
/// Best-effort on both stores. The local append failing is logged at warn
/// (it is the primary source); the mirror failing is logged at debug (an
/// older backend has no `data` command, and that is not an error).
pub async fn record(
    workspace: &Path,
    bead_id: &BeadId,
    record: AttemptRecord,
    store: &dyn BeadStore,
    mirror: bool,
) {
    if let Err(e) = append_local(workspace, bead_id, &record) {
        tracing::warn!(
            bead_id = %bead_id,
            workspace = %workspace.display(),
            error = %e,
            "attempt history: failed to append the local journal"
        );
        return;
    }
    if !mirror {
        return;
    }
    let records = load_local(workspace, bead_id).unwrap_or_else(|_| vec![record]);
    let value = to_data_value(&records);
    match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        store.set_data(bead_id, DATA_NAMESPACE, DATA_SCHEMA_REF, &value),
    )
    .await
    {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => tracing::debug!(
            bead_id = %bead_id,
            "attempt history: backend has no structured data support; local journal only"
        ),
        Ok(Err(e)) => tracing::debug!(
            bead_id = %bead_id,
            error = %e,
            "attempt history: mirror write failed; local journal only"
        ),
        Err(_) => tracing::debug!(
            bead_id = %bead_id,
            "attempt history: mirror write timed out; local journal only"
        ),
    }
}

/// Render the newest attempts as the prompt section, or `""` when there is
/// nothing an agent needs to know (no prior attempts).
///
/// Newest first. The header states how many attempts failed so the agent
/// treats the bead as a retry, and the instruction is explicit: read the
/// failure, do not repeat it, make a failed gate pass first.
pub fn render(records: &[AttemptRecord], limits: HistoryLimits) -> String {
    if records.is_empty() || limits.max_attempts == 0 {
        return String::new();
    }
    let failed = records.iter().filter(|r| !r.is_verified_success()).count();
    let mut out = String::new();
    out.push_str("## Previous attempts on this bead (newest first)\n\n");
    out.push_str(&format!(
        "This bead has been attempted {} time{} before ({} without a verified success). \
         Read the failures below before you start. Do NOT repeat an approach that already \
         failed the same way. If a verification gate failed, make that gate pass first — \
         the gate is what decides whether your work is accepted.\n\n",
        records.len(),
        if records.len() == 1 { "" } else { "s" },
        failed
    ));

    let newest_first = records.iter().rev().take(limits.max_attempts);
    for (i, r) in newest_first.enumerate() {
        let n = records.len() - i;
        let mut block = String::new();
        block.push_str(&format!(
            "### Attempt {n} — {} — {} — outcome: {}{}\n",
            short_time(&r.recorded_at),
            r.adapter,
            r.outcome,
            r.terminal_reason
                .as_deref()
                .map(|t| format!(" ({t})"))
                .unwrap_or_default()
        ));
        if !r.commits.is_empty() {
            block.push_str(&format!(
                "Commits it left in the workspace: {}\n",
                r.commits
                    .iter()
                    .map(|c| c.chars().take(12).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(summary) = r
            .failure_summary
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            block.push_str("```\n");
            block.push_str(summary.trim_end());
            block.push_str("\n```\n");
        }
        block.push('\n');
        if out.len() + block.len() > limits.max_bytes {
            out.push_str("(older attempts omitted — history capped at ");
            out.push_str(&limits.max_bytes.to_string());
            out.push_str(" bytes)\n");
            break;
        }
        out.push_str(&block);
    }
    out.trim_end().to_string()
}

/// `2026-09-12T14:19:11.123Z` → `2026-09-12T14:19Z`; anything else unchanged.
fn short_time(ts: &str) -> String {
    if ts.len() >= 16 && ts.as_bytes()[10] == b'T' {
        format!("{}Z", &ts[..16])
    } else {
        ts.to_string()
    }
}

/// Keep the first two thirds and last third of `text` when it exceeds
/// `max_bytes`, on char boundaries, with an elision marker between.
pub fn truncate_head_tail(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let marker = "\n… [elided] …\n";
    if max_bytes <= marker.len() + 2 {
        return text.chars().take(max_bytes).collect();
    }
    let budget = max_bytes - marker.len();
    let head_len = budget * 2 / 3;
    let tail_len = budget - head_len;
    let mut head_end = head_len;
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len() - tail_len;
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!("{}{}{}", &text[..head_end], marker, &text[tail_start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(n: u32, outcome: &str, summary: Option<&str>) -> AttemptRecord {
        AttemptRecord {
            schema_version: SCHEMA_VERSION,
            attempt_id: format!("att-{n}"),
            recorded_at: format!("2026-09-12T14:{:02}:00.000Z", n),
            worker: "w".into(),
            adapter: "claude-code-glm-5.3-flash".into(),
            model: Some("glm-5.3-flash".into()),
            outcome: outcome.into(),
            terminal_reason: Some("gate:definition-of-done".into()),
            exit_code: 0,
            requested_action: "Released".into(),
            commits: vec!["abcdef1234567890".into()],
            duration_ms: 1000,
            failure_summary: summary.map(str::to_string),
        }
    }

    #[test]
    fn append_and_load_round_trip_keeps_order_and_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let id = BeadId::from("nd-1");
        for n in 0..(LOCAL_KEEP as u32 + 5) {
            append_local(dir.path(), &id, &rec(n, "work_failure", Some("boom"))).unwrap();
        }
        let loaded = load_local(dir.path(), &id).unwrap();
        assert_eq!(loaded.len(), LOCAL_KEEP);
        assert_eq!(loaded[0].attempt_id, "att-5");
        assert_eq!(
            loaded.last().unwrap().attempt_id,
            format!("att-{}", LOCAL_KEEP + 4)
        );
        assert!(history_path(dir.path(), &id).exists());
    }

    #[test]
    fn load_skips_malformed_lines_and_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let id = BeadId::from("nd-2");
        assert!(load_local(dir.path(), &id).unwrap().is_empty());
        let path = history_path(dir.path(), &id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "not json\n{}\n",
                serde_json::to_string(&rec(1, "work_failure", None)).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(load_local(dir.path(), &id).unwrap().len(), 1);
    }

    #[test]
    fn render_is_empty_without_history_and_newest_first_with() {
        assert_eq!(render(&[], HistoryLimits::default()), "");
        let records = vec![
            rec(1, "work_failure", Some("error[E0308]: mismatched types")),
            rec(2, "indeterminate", None),
        ];
        let text = render(&records, HistoryLimits::default());
        assert!(text.starts_with("## Previous attempts on this bead"));
        assert!(text.contains("attempted 2 times before (2 without a verified success)"));
        let a2 = text.find("### Attempt 2").unwrap();
        let a1 = text.find("### Attempt 1").unwrap();
        assert!(a2 < a1, "newest first");
        assert!(text.contains("error[E0308]"));
        assert!(text.contains("abcdef123456"));
        assert!(text.contains("Do NOT repeat"));
    }

    #[test]
    fn render_respects_attempt_and_byte_caps() {
        let records: Vec<_> = (0..6)
            .map(|n| rec(n, "work_failure", Some(&"x".repeat(500))))
            .collect();
        let capped = render(
            &records,
            HistoryLimits {
                max_attempts: 2,
                max_bytes: 100_000,
            },
        );
        assert_eq!(capped.matches("### Attempt").count(), 2);
        let bytes = render(
            &records,
            HistoryLimits {
                max_attempts: 6,
                max_bytes: 900,
            },
        );
        assert!(bytes.len() <= 1000, "{}", bytes.len());
        assert!(bytes.contains("older attempts omitted"));
    }

    #[test]
    fn mirror_value_round_trips_and_is_bounded() {
        let records: Vec<_> = (0..9)
            .map(|n| rec(n, "work_failure", Some(&"y".repeat(2000))))
            .collect();
        let value = to_data_value(&records);
        assert_eq!(value["total_attempts"], 9);
        let back = from_data_value(&value);
        assert_eq!(back.len(), MIRROR_KEEP);
        assert_eq!(back[0].attempt_id, "att-4");
        let summary = back[0].failure_summary.as_ref().unwrap();
        assert!(
            summary.len() <= MIRROR_SUMMARY_BYTES + 20,
            "{}",
            summary.len()
        );
        assert!(summary.contains("[elided]"));
        assert!(from_data_value(&serde_json::json!({"nope": 1})).is_empty());
    }

    #[test]
    fn truncate_head_tail_keeps_both_ends() {
        let text = format!("{}MIDDLE{}", "h".repeat(400), "t".repeat(400));
        let cut = truncate_head_tail(&text, 300);
        assert!(cut.starts_with("hhh"));
        assert!(cut.ends_with("ttt"));
        assert!(!cut.contains("MIDDLE"));
        assert!(cut.len() <= 300);
        assert_eq!(truncate_head_tail("short", 300), "short");
        let multi = "é".repeat(300);
        let cut = truncate_head_tail(&multi, 100);
        assert!(cut.len() <= 100);
    }
}
