//! Scheduled retention sweeps for workspaces without a live NEEDLE worker.
//!
//! The normal Mend strand cleans traces while a workspace is active.  This
//! module supplies the complementary operator-facing sweep: discover real
//! `.beads` workspaces in a deterministic order, skip any workspace with a
//! live worker or unfinished capture, and delegate eligible mutations to the
//! canonical trace cleanup implementation.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::Serialize;

use crate::config::{CliOverrides, Config, ConfigLoader};
use crate::registry::{is_pid_alive, Registry};
use crate::state_dir;
use crate::trace::{cleanup_traces_with_options, TraceCleanupOptions, TraceMetadata};

const CAPTURE_FILES: [&str; 5] = [
    "trace.jsonl",
    "stdout.txt",
    "stderr.txt",
    "test-output.txt",
    "prompt.md",
];
const DELETE_FILES: [&str; 8] = [
    "trace.jsonl",
    "stdout.txt",
    "stderr.txt",
    "test-output.txt",
    "prompt.md",
    "test_metrics.json",
    "compilation_errors.json",
    "metadata.json",
];

/// Machine-readable result printed by the scheduled command.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SweepSummary {
    pub dry_run: bool,
    pub workspaces_seen: u64,
    pub workspaces_swept: u64,
    pub workspaces_skipped_active: u64,
    pub workspaces_skipped_live_capture: u64,
    pub traces_pruned: u64,
    pub traces_deleted: u64,
    pub bytes_reclaimed: u64,
    pub errors: u64,
}

#[derive(Debug, Default)]
struct WorkspacePlan {
    active_capture: bool,
    traces_pruned: u64,
    traces_deleted: u64,
    bytes_reclaimable: u64,
}

/// Run one deterministic sweep.
pub fn sweep(root: &Path, explicit_workspaces: &[PathBuf], dry_run: bool) -> Result<SweepSummary> {
    let (root_config, _) = ConfigLoader::load_resolved(root, CliOverrides::default())?;
    state_dir::set_configured(root_config.paths.state_dir.clone());
    let live_workspaces = live_worker_workspaces(&root_config)?;
    let workspaces = if explicit_workspaces.is_empty() {
        discover_workspaces(root)?
    } else {
        let mut paths = explicit_workspaces.to_vec();
        paths.sort();
        paths.dedup();
        paths
    };

    let mut summary = SweepSummary {
        dry_run,
        workspaces_seen: workspaces.len() as u64,
        ..SweepSummary::default()
    };

    for workspace in workspaces {
        let Some(workspace_identity) = real_path(&workspace) else {
            summary.errors += 1;
            continue;
        };
        if live_workspaces.contains(&workspace_identity) {
            summary.workspaces_skipped_active += 1;
            continue;
        }

        let result = sweep_workspace(&workspace_identity, dry_run);
        match result {
            Ok(plan) if plan.active_capture => {
                summary.workspaces_skipped_live_capture += 1;
            }
            Ok(plan) => {
                summary.workspaces_swept += 1;
                summary.traces_pruned += plan.traces_pruned;
                summary.traces_deleted += plan.traces_deleted;
                summary.bytes_reclaimed = summary
                    .bytes_reclaimed
                    .saturating_add(plan.bytes_reclaimable);
            }
            Err(error) => {
                // Do not expose trace contents.  The structured count is for
                // monitoring; the operator can use journal context to locate
                // the affected workspace if needed.
                tracing::warn!(workspace = %workspace_identity.display(), error = %error, "trace retention sweep skipped workspace");
                summary.errors += 1;
            }
        }
    }

    Ok(summary)
}

/// Discover workspaces without following symlinks.  Sorting both directory
/// entries and the final result keeps repeated timer runs reproducible.
pub fn discover_workspaces(root: &Path) -> Result<Vec<PathBuf>> {
    if !is_real_dir(root) {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    discover_from(root, &mut found)?;
    found.sort();
    found.dedup();
    Ok(found)
}

fn discover_from(path: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    let beads_dir = path.join(".beads");
    if is_real_dir(&beads_dir) && is_real_dir(&beads_dir.join("traces")) {
        found.push(path.to_path_buf());
        return Ok(());
    }

    let mut entries = sorted_entries(path)?;
    for entry in entries.drain(..) {
        let name = entry.file_name();
        if matches!(
            name.and_then(|name| name.to_str()),
            Some(".git" | ".beads" | "target" | "node_modules")
        ) {
            continue;
        }
        if is_real_dir(&entry) {
            discover_from(&entry, found)?;
        }
    }
    Ok(())
}

fn sorted_entries(path: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("failed to read workspace root {}", path.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|entry| !matches!(entry.file_name(), Some(name) if name == ".needle"))
        .collect::<Vec<_>>();
    entries.sort();
    Ok(entries)
}

fn live_worker_workspaces(config: &Config) -> Result<HashSet<PathBuf>> {
    let registry = Registry::new(&state_dir::root_for(&config.workspace.home).join("state"));
    let mut live = HashSet::new();
    for worker in registry.list_all()? {
        if is_pid_alive(worker.pid) {
            if let Some(workspace) = real_path(&worker.workspace) {
                live.insert(workspace);
            }
        }
    }
    Ok(live)
}

fn sweep_workspace(workspace: &Path, dry_run: bool) -> Result<WorkspacePlan> {
    let (config, _) = ConfigLoader::load_resolved(workspace, CliOverrides::default())?;
    let traces_dir = workspace.join(".beads").join("traces");
    if !is_real_dir(&traces_dir) {
        return Ok(WorkspacePlan::default());
    }

    let lock = retention_lock(&traces_dir)?;
    let plan = plan_workspace(
        &traces_dir,
        config.strands.learning.trace_retention_failed_days,
        config.strands.learning.trace_retention_success_days,
        config.attempt_archive.enabled,
        config.attempt_archive.prune_local_after_spool,
    )?;
    if plan.active_capture || dry_run {
        drop(lock);
        return Ok(plan);
    }

    let before = directory_bytes(&traces_dir)?;
    let cleanup = cleanup_traces_with_options(
        &traces_dir,
        config.strands.learning.trace_retention_failed_days,
        config.strands.learning.trace_retention_success_days,
        TraceCleanupOptions {
            archive_enabled: config.attempt_archive.enabled,
            prune_local_after_spool: config.attempt_archive.prune_local_after_spool,
        },
    )?;
    let after = directory_bytes(&traces_dir)?;
    drop(lock);

    Ok(WorkspacePlan {
        active_capture: false,
        traces_pruned: u64::from(cleanup.traces_pruned),
        traces_deleted: u64::from(cleanup.traces_deleted),
        bytes_reclaimable: before.saturating_sub(after),
    })
}

fn retention_lock(traces_dir: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(traces_dir.join(".retention.lock"))
        .context("failed to open trace retention lock")?;
    file.lock_exclusive()
        .context("failed to acquire trace retention lock")?;
    Ok(file)
}

fn plan_workspace(
    traces_dir: &Path,
    retention_days_failed: u32,
    retention_days_success: u32,
    archive_enabled: bool,
    prune_local_after_spool: bool,
) -> Result<WorkspacePlan> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let archive_gate_required = archive_enabled && prune_local_after_spool;
    let mut plan = WorkspacePlan::default();

    for bead_dir in sorted_entries(traces_dir)? {
        if !is_real_dir(&bead_dir) {
            if is_symlink(&bead_dir) {
                anyhow::bail!("trace tree contains a symlink");
            }
            continue;
        }

        inspect_capture(
            &bead_dir,
            true,
            now,
            retention_days_failed,
            retention_days_success,
            archive_gate_required,
            &mut plan,
        )?;

        for attempt_dir in sorted_entries(&bead_dir)? {
            if is_symlink(&attempt_dir) {
                anyhow::bail!("trace tree contains a symlink");
            }
            if is_real_dir(&attempt_dir) {
                inspect_capture(
                    &attempt_dir,
                    false,
                    now,
                    retention_days_failed,
                    retention_days_success,
                    archive_gate_required,
                    &mut plan,
                )?;
            }
        }
    }
    Ok(plan)
}

fn inspect_capture(
    path: &Path,
    has_attempt_children: bool,
    now: u64,
    retention_days_failed: u32,
    retention_days_success: u32,
    archive_gate_required: bool,
    plan: &mut WorkspacePlan,
) -> Result<()> {
    let metadata_path = path.join("metadata.json");
    if !is_real_file(&metadata_path) {
        if capture_has_data(path)? {
            plan.active_capture = true;
        }
        return Ok(());
    }
    let metadata: TraceMetadata = serde_json::from_slice(
        &fs::read(&metadata_path).with_context(|| "failed to read trace metadata")?,
    )
    .context("failed to parse trace metadata")?;

    // Dispatch writes metadata before outcome handling adds finished_at.  A
    // scheduled sweep must never touch that live capture, even if its worker
    // registration disappeared between scans.
    if metadata.started_at.is_some() && metadata.finished_at.is_none() {
        plan.active_capture = true;
        return Ok(());
    }

    let age_days = now.saturating_sub(metadata.captured_at.timestamp().max(0) as u64) / 86_400;
    let archive_gate_open = !archive_gate_required || is_real_file(&path.join("spooled.json"));
    let failed = metadata.exit_code != 0;
    let has_data = capture_has_data(path)?;
    if !archive_gate_open {
        return Ok(());
    }
    if failed && age_days > u64::from(retention_days_failed) {
        plan.traces_deleted += 1;
        plan.bytes_reclaimable = plan
            .bytes_reclaimable
            .saturating_add(if has_attempt_children {
                sum_named_files(path, &DELETE_FILES)?
            } else {
                directory_bytes(path)?
            });
    } else if !failed && has_data && age_days > u64::from(retention_days_success) {
        plan.traces_pruned += 1;
        plan.bytes_reclaimable = plan
            .bytes_reclaimable
            .saturating_add(sum_named_files(path, &CAPTURE_FILES)?);
    }
    Ok(())
}

fn capture_has_data(path: &Path) -> Result<bool> {
    for name in CAPTURE_FILES {
        let child = path.join(name);
        if is_symlink(&child) {
            anyhow::bail!("trace tree contains a symlink");
        }
        if is_real_file(&child) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sum_named_files(path: &Path, names: &[&str]) -> Result<u64> {
    let mut total: u64 = 0;
    for name in names {
        let child = path.join(name);
        if is_symlink(&child) {
            anyhow::bail!("trace tree contains a symlink");
        }
        if is_real_file(&child) {
            total = total.saturating_add(fs::metadata(child)?.len());
        }
    }
    Ok(total)
}

fn directory_bytes(path: &Path) -> Result<u64> {
    if !is_real_dir(path) {
        return Ok(0);
    }
    let mut total: u64 = 0;
    for entry in sorted_entries(path)? {
        if is_symlink(&entry) {
            anyhow::bail!("trace tree contains a symlink");
        }
        if is_real_file(&entry) {
            total = total.saturating_add(fs::metadata(&entry)?.len());
        } else if is_real_dir(&entry) {
            total = total.saturating_add(directory_bytes(&entry)?);
        }
    }
    Ok(total)
}

fn real_path(path: &Path) -> Option<PathBuf> {
    is_real_dir(path)
        .then(|| fs::canonicalize(path).ok())
        .flatten()
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir())
        .unwrap_or(false)
}

fn is_real_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tempfile::TempDir;

    fn metadata(exit_code: i32, days_old: i64) -> TraceMetadata {
        TraceMetadata {
            exit_code,
            captured_at: Utc::now() - chrono::Duration::days(days_old),
            started_at: None,
            finished_at: Some(Utc::now()),
            ..TraceMetadata::default()
        }
    }

    fn write_capture(path: &Path, exit_code: i32, days_old: i64) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("stdout.txt"), "capture").unwrap();
        fs::write(
            path.join("metadata.json"),
            serde_json::to_vec(&metadata(exit_code, days_old)).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn plans_flat_and_attempt_layouts_without_trace_content() {
        let temp = TempDir::new().unwrap();
        let traces = temp.path().join("traces");
        let flat = traces.join("flat");
        write_capture(&flat, 0, 8);
        write_capture(&traces.join("attempt").join("a1"), 1, 31);

        let plan = plan_workspace(&traces, 30, 7, false, false).unwrap();
        assert_eq!(plan.traces_pruned, 1);
        assert_eq!(plan.traces_deleted, 1);
        assert!(plan.bytes_reclaimable > 0);
    }

    #[test]
    fn spool_gate_and_live_capture_are_fail_closed() {
        let temp = TempDir::new().unwrap();
        let traces = temp.path().join("traces");
        let gated = traces.join("gated");
        write_capture(&gated, 0, 8);
        let live = traces.join("live");
        fs::create_dir_all(&live).unwrap();
        fs::write(live.join("stdout.txt"), "live").unwrap();

        let plan = plan_workspace(&traces, 30, 7, true, true).unwrap();
        assert!(plan.active_capture);
        assert_eq!(plan.traces_pruned, 0);
        assert!(gated.join("stdout.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_capture_is_rejected_without_following_it() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new().unwrap();
        let traces = temp.path().join("traces");
        fs::create_dir_all(&traces).unwrap();
        symlink(temp.path(), traces.join("link")).unwrap();
        assert!(plan_workspace(&traces, 30, 7, false, false).is_err());
    }
}
