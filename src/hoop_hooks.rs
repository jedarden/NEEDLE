//! Integration hooks consumed by HOOP (a separate, additive observability and
//! control-plane tool — `~/HOOP` on this host). NEEDLE runs standalone
//! without any of these; every function here is best-effort and must never
//! fail a worker's boot or interrupt its bead-processing cycle. See HOOP's
//! `docs/needle-hooks.md` for the wire format each hook produces.
//!
//! Hooks implemented here:
//! - Hook 2 (event tap): [`emit_needle_event`] appends to `.beads/events.jsonl`.
//! - Hook 3 (heartbeat): [`emit_needle_heartbeat`] appends to `.beads/heartbeats.jsonl`.
//! - Hook 5 (spawn ack): [`write_spawn_ack`] writes `~/.hoop/workers/<worker>.ack`.
//!
//! Hook 1 (dispatch tag) and Hook 4 (stitch label inheritance) live at their
//! natural call sites (`worker::do_build_prompt` and `mitosis::create_children`
//! respectively) rather than here.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;

/// HOOP Hook 5 — spawn ack.
///
/// Writes `~/.hoop/workers/<worker_name>.ack` at the very start of a worker's
/// boot sequence, before the heartbeat loop begins. `tmux send-keys` can
/// silently truncate a spawn command longer than ~255 bytes — the worker then
/// never starts, but emits no heartbeats either, so HOOP cannot distinguish
/// "not yet started" from "spawn failed" without a positive ack. Uses a
/// tmp-file + rename so HOOP never observes a partially-written file.
pub fn write_spawn_ack(worker_name: &str) -> Result<()> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let dir = PathBuf::from(home).join(".hoop").join("workers");
    write_spawn_ack_in(&dir, worker_name)
}

fn write_spawn_ack_in(dir: &Path, worker_name: &str) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let body = format!(
        "{}\n",
        serde_json::json!({
            "worker": worker_name,
            "ts": Utc::now().to_rfc3339(),
            "pid": std::process::id(),
        })
    );
    let tmp_path = dir.join(format!("{worker_name}.ack.tmp"));
    let final_path = dir.join(format!("{worker_name}.ack"));
    std::fs::write(&tmp_path, body)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("failed to rename into {}", final_path.display()))?;
    Ok(())
}

/// A workspace is a directory that already has a `.beads/` store. The default
/// event/heartbeat paths live inside it; writing them anywhere else would
/// create a half-workspace (`.beads/` without `config.json`) that bead-rs's
/// discovery later refuses to init past — on 2026-08-30 test fixtures using
/// `/tmp` as a workspace left `/tmp/.beads` behind and broke every temp-dir
/// `bead init` on the CI pod. Explicit `$NEEDLE_EVENTS` / `$NEEDLE_HEARTBEATS`
/// overrides are honoured regardless.
fn default_target_allowed(workspace: &Path, env_override: &str) -> bool {
    std::env::var_os(env_override).is_some() || workspace.join(".beads").is_dir()
}

/// Resolve the HOOP event-tap target: `$NEEDLE_EVENTS` if set, else
/// `<workspace>/.beads/events.jsonl`.
pub fn events_path(workspace: &Path) -> PathBuf {
    std::env::var("NEEDLE_EVENTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace.join(".beads").join("events.jsonl"))
}

/// HOOP Hook 2 — event tap.
///
/// Appends one JSONL line per bead-state transition. Best-effort: on any
/// failure this logs a warning and returns — a stalled or unwritable
/// `events.jsonl` must never block bead processing. `extra` fields (e.g.
/// `adapter`, `model`, `outcome`, `duration_ms`, `exit_code`, `error`,
/// `reason`) are merged into the line's top-level object.
pub fn emit_needle_event(
    workspace: &Path,
    worker: &str,
    bead: Option<&str>,
    strand: Option<&str>,
    event: &str,
    extra: Value,
) {
    if !default_target_allowed(workspace, "NEEDLE_EVENTS") {
        tracing::debug!(
            workspace = %workspace.display(),
            event,
            "hoop_hooks: not a bead workspace (no .beads/) — event not recorded"
        );
        return;
    }
    let path = events_path(workspace);
    if let Err(e) = append_jsonl_line(&path, |obj| {
        if let Some(b) = bead {
            obj.insert("bead".to_string(), Value::String(b.to_string()));
        }
        if let Some(s) = strand {
            obj.insert("strand".to_string(), Value::String(s.to_string()));
        }
        obj.insert("worker".to_string(), Value::String(worker.to_string()));
        obj.insert("event".to_string(), Value::String(event.to_string()));
        merge_extra(obj, extra);
    }) {
        tracing::warn!(
            error = %e,
            path = %path.display(),
            event,
            "hoop_hooks: failed to append events.jsonl line (non-fatal)"
        );
    }
}

/// Resolve the HOOP heartbeat target: `$NEEDLE_HEARTBEATS` if set, else
/// `<workspace>/.beads/heartbeats.jsonl`.
pub fn heartbeats_path(workspace: &Path) -> PathBuf {
    std::env::var("NEEDLE_HEARTBEATS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace.join(".beads").join("heartbeats.jsonl"))
}

/// Clean up a heartbeat file.
///
/// Removes the heartbeat file at the specified path. This is used when a worker
/// shuts down cleanly to remove its heartbeat tracking file. Best-effort:
/// failures are ignored to avoid disrupting shutdown.
///
/// # Arguments
///
/// * `path` - Path to the heartbeat file to remove
///
/// # Returns
///
/// * `Ok(())` - If the file was removed successfully or doesn't exist
/// * `Err(e)` - If removal fails for reasons other than NotFound
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use needle::hoop_hooks::cleanup_heartbeat_file;
///
/// let path = Path::new("/tmp/heartbeat.json");
/// cleanup_heartbeat_file(path)?;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn cleanup_heartbeat_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(_) => {
            tracing::debug!(
                path = %path.display(),
                "heartbeat file removed successfully"
            );
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // File doesn't exist - this is success (idempotent cleanup)
            tracing::debug!(
                path = %path.display(),
                "heartbeat file does not exist, skipping cleanup"
            );
            Ok(())
        }
        Err(e) => {
            // Log the error but still return to avoid disrupting shutdown
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "failed to remove heartbeat file during cleanup (non-fatal)"
            );
            // Best-effort: don't fail shutdown for cleanup errors
            Ok(())
        }
    }
}

/// HOOP Hook 3 — heartbeat.
///
/// Appends one JSONL line per heartbeat tick in HOOP's three-state format
/// (`executing` / `idle` / `knot`). Best-effort, same failure contract as
/// [`emit_needle_event`].
pub fn emit_needle_heartbeat(workspace: &Path, worker: &str, state: &str, extra: Value) {
    if !default_target_allowed(workspace, "NEEDLE_HEARTBEATS") {
        tracing::debug!(
            workspace = %workspace.display(),
            state,
            "hoop_hooks: not a bead workspace (no .beads/) — heartbeat not recorded"
        );
        return;
    }
    let path = heartbeats_path(workspace);
    if let Err(e) = append_jsonl_line(&path, |obj| {
        obj.insert("worker".to_string(), Value::String(worker.to_string()));
        obj.insert("state".to_string(), Value::String(state.to_string()));
        merge_extra(obj, extra);
    }) {
        tracing::warn!(
            error = %e,
            path = %path.display(),
            state,
            "hoop_hooks: failed to append heartbeats.jsonl line (non-fatal)"
        );
    }
}

fn merge_extra(obj: &mut serde_json::Map<String, Value>, extra: Value) {
    if let Value::Object(extra_map) = extra {
        for (k, v) in extra_map {
            obj.insert(k, v);
        }
    }
}

/// Append one JSONL line built by `populate`, creating the parent directory
/// and the file if needed. Every line always carries `ts` (RFC 3339 UTC).
fn append_jsonl_line(
    path: &Path,
    populate: impl FnOnce(&mut serde_json::Map<String, Value>),
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut obj = serde_json::Map::new();
    obj.insert("ts".to_string(), Value::String(Utc::now().to_rfc3339()));
    populate(&mut obj);
    let line = serde_json::to_string(&Value::Object(obj))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    writeln!(writer, "{line}")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn default_targets_never_create_a_beads_dir_outside_a_workspace() {
        let dir = tempfile::tempdir().unwrap();
        // No .beads/ here and no env override: nothing may be written.
        std::env::remove_var("NEEDLE_EVENTS");
        std::env::remove_var("NEEDLE_HEARTBEATS");
        emit_needle_event(dir.path(), "w", None, None, "test", serde_json::json!({}));
        emit_needle_heartbeat(dir.path(), "w", "idle", serde_json::json!({}));
        assert!(
            !dir.path().join(".beads").exists(),
            ".beads/ must not be created"
        );
        // With a store present, the default paths are used.
        std::fs::create_dir_all(dir.path().join(".beads")).unwrap();
        emit_needle_event(dir.path(), "w", None, None, "test", serde_json::json!({}));
        assert!(dir.path().join(".beads").join("events.jsonl").exists());
    }
    use super::*;
    use tempfile::TempDir;

    fn read_lines(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn spawn_ack_writes_expected_fields_and_no_tmp_leftover() {
        let dir = TempDir::new().unwrap();
        write_spawn_ack_in(dir.path(), "alpha").unwrap();

        let final_path = dir.path().join("alpha.ack");
        let tmp_path = dir.path().join("alpha.ack.tmp");
        assert!(final_path.exists());
        assert!(!tmp_path.exists());

        let body: Value =
            serde_json::from_str(std::fs::read_to_string(&final_path).unwrap().trim()).unwrap();
        assert_eq!(body["worker"], "alpha");
        assert_eq!(body["pid"], std::process::id());
        assert!(body["ts"].is_string());
    }

    #[test]
    fn spawn_ack_creates_missing_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("nested").join("workers");
        write_spawn_ack_in(&nested, "bravo").unwrap();
        assert!(nested.join("bravo.ack").exists());
    }

    #[test]
    fn events_path_defaults_to_workspace_beads_dir() {
        // Env mutation races the whole process, not just this module — hold
        // the crate-wide env lock (util::test_env).
        let _env_guard = crate::util::test_env::isolate_env();
        unsafe {
            std::env::remove_var("NEEDLE_EVENTS");
        }
        let ws = Path::new("/tmp/some-workspace");
        assert_eq!(events_path(ws), ws.join(".beads").join("events.jsonl"));
    }

    #[test]
    fn emit_needle_event_appends_expected_line() {
        let _env_guard = crate::util::test_env::isolate_env();
        let dir = TempDir::new().unwrap();
        unsafe {
            std::env::set_var(
                "NEEDLE_EVENTS",
                dir.path().join("events.jsonl").to_str().unwrap(),
            );
        }
        emit_needle_event(
            dir.path(),
            "alpha",
            Some("bd-abc123"),
            Some("pluck"),
            "claim",
            serde_json::json!({}),
        );
        emit_needle_event(
            dir.path(),
            "alpha",
            Some("bd-abc123"),
            None,
            "dispatch",
            serde_json::json!({"adapter": "claude", "model": "opus"}),
        );
        unsafe {
            std::env::remove_var("NEEDLE_EVENTS");
        }

        let lines = read_lines(&dir.path().join("events.jsonl"));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["worker"], "alpha");
        assert_eq!(lines[0]["bead"], "bd-abc123");
        assert_eq!(lines[0]["strand"], "pluck");
        assert_eq!(lines[0]["event"], "claim");
        assert!(lines[0]["ts"].is_string());

        assert_eq!(lines[1]["event"], "dispatch");
        assert_eq!(lines[1]["adapter"], "claude");
        assert_eq!(lines[1]["model"], "opus");
        assert!(lines[1].get("strand").is_none());
    }

    #[test]
    fn emit_needle_event_is_best_effort_on_unwritable_path() {
        let _env_guard = crate::util::test_env::isolate_env();
        // Parent is a file, not a directory — create_dir_all/open must fail,
        // and the function must not panic.
        let dir = TempDir::new().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let bogus_workspace = blocker.join("workspace");
        unsafe {
            std::env::remove_var("NEEDLE_EVENTS");
        }
        emit_needle_event(
            &bogus_workspace,
            "alpha",
            None,
            None,
            "claim",
            serde_json::json!({}),
        );
        // No panic = pass.
    }

    #[test]
    fn emit_needle_heartbeat_appends_three_states() {
        let _env_guard = crate::util::test_env::isolate_env();
        let dir = TempDir::new().unwrap();
        unsafe {
            std::env::set_var(
                "NEEDLE_HEARTBEATS",
                dir.path().join("heartbeats.jsonl").to_str().unwrap(),
            );
        }
        emit_needle_heartbeat(
            dir.path(),
            "alpha",
            "executing",
            serde_json::json!({"bead": "bd-abc123", "pid": 12345, "adapter": "claude"}),
        );
        emit_needle_heartbeat(
            dir.path(),
            "alpha",
            "idle",
            serde_json::json!({"last_strand": "pluck"}),
        );
        emit_needle_heartbeat(
            dir.path(),
            "alpha",
            "knot",
            serde_json::json!({"reason": "strands exhausted"}),
        );
        unsafe {
            std::env::remove_var("NEEDLE_HEARTBEATS");
        }

        let lines = read_lines(&dir.path().join("heartbeats.jsonl"));
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["state"], "executing");
        assert_eq!(lines[0]["pid"], 12345);
        assert_eq!(lines[1]["state"], "idle");
        assert_eq!(lines[1]["last_strand"], "pluck");
        assert_eq!(lines[2]["state"], "knot");
        assert_eq!(lines[2]["reason"], "strands exhausted");
    }
}
