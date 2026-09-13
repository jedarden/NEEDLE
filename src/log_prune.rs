//! Prune orphaned NEEDLE worker log files under `.needle/logs/`.
//!
//! # Why this exists
//!
//! [`crate::log_writer::SizeCappedWriter`] correctly bounds ONE identifier's
//! own log family to a fixed byte budget, but nothing ever deletes that
//! family once its owning process exits. `needle cleanup` only removes
//! orphaned tmux sessions — it has never touched `.needle/logs/`. A fleet
//! that mints a fresh identifier on every dispatch (rather than reusing a
//! long-lived one) accumulates one abandoned log family per dispatch,
//! forever. On codinghome this produced 100G / 38,983 files in five days,
//! 78G of it from one dispatch source. See bead needle-daab3ee2 for the full
//! incident writeup.
//!
//! # Liveness
//!
//! A log file's owning identifier is considered live if a currently running
//! `needle run` process's `--agent`/`--identifier` pair reconstructs the
//! same sanitized filename prefix (mirroring exactly how
//! `worker_log_writer` in `cli` names the file at write time). This is
//! deliberately NOT the tmux-session-based liveness check `needle cleanup`
//! uses: NEEDLE workers here are commonly launched directly (e.g. under
//! systemd), with no tmux session at all, so a check anchored to tmux would
//! silently fail to protect them.
//!
//! # Defense in depth
//!
//! Live-identifier resolution has a known gap: a worker launched without an
//! explicit `--identifier` (an internally-generated NATO name) cannot be
//! reconstructed from its command line alone, so it would not appear in the
//! live-prefix set. To keep that gap from ever causing a live file to be
//! deleted, a file is only a deletion candidate when BOTH conditions hold:
//! its prefix is not in the live set, AND it has not been modified within
//! [`DEFAULT_MIN_IDLE_SECS`]. A currently-active worker is, by construction,
//! writing to its own log continuously (if nothing else, the idle heartbeat
//! in `worker::do_select`'s exhausted-backoff loop), so the recency check
//! alone is sufficient to protect it even if the identifier match misses.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};

/// A file is never deleted if it was modified more recently than this, even
/// when its prefix does not appear in the live set. See the module-level
/// "Defense in depth" section for why this backstop exists.
pub const DEFAULT_MIN_IDLE_SECS: u64 = 300;

/// Outcome of one sweep of `.needle/logs/`.
#[derive(Debug, Default, Clone)]
pub struct PruneReport {
    pub deleted_files: usize,
    pub deleted_bytes: u64,
    pub kept_files: usize,
    pub kept_bytes: u64,
    /// Per-file errors (e.g. a permission failure on `remove_file`). A sweep
    /// with errors still reports every file it *did* successfully handle;
    /// errors never abort the rest of the sweep.
    pub errors: Vec<String>,
}

/// Recover a log file's owning prefix from its file name, or `None` if the
/// name does not match a NEEDLE worker log file this sweep understands.
///
/// All three forms `worker_log_writer` (in `cli`) can produce for one
/// identifier collapse to the same prefix:
/// - `needle-<prefix>.log`             (the live file)
/// - `needle-<prefix>.log.<N>`         (a rotated historical generation)
/// - `needle-<prefix>.stderr.log`      (raw stderr the tmux wrapper redirects)
fn owning_prefix(file_name: &str) -> Option<String> {
    if !file_name.starts_with("needle-") {
        return None;
    }
    if let Some(base) = file_name.strip_suffix(".stderr.log") {
        return Some(base.to_string());
    }
    if let Some(base) = file_name.strip_suffix(".log") {
        return Some(base.to_string());
    }
    if let Some(idx) = file_name.find(".log.") {
        let (base, rest) = file_name.split_at(idx);
        let suffix = &rest[".log.".len()..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return Some(base.to_string());
        }
    }
    None
}

/// Delete files under `log_dir` whose owning prefix is absent from
/// `live_prefixes` and whose mtime is older than `min_idle_secs`.
///
/// Never recurses into subdirectories (`archive/` under `.needle/logs/` is a
/// separate, actively used store and out of scope for this sweep — only
/// direct entries of `log_dir` are considered). A file this sweep does not
/// recognize as a NEEDLE worker log (see [`owning_prefix`]) is left
/// untouched and not counted in the report at all.
///
/// With `dry_run: true`, candidates are counted as `deleted_files`/
/// `deleted_bytes` without touching the filesystem, so callers can preview
/// the effect of a real run.
pub fn sweep_orphaned_logs(
    log_dir: &Path,
    live_prefixes: &HashSet<String>,
    min_idle_secs: u64,
    dry_run: bool,
) -> Result<PruneReport> {
    let mut report = PruneReport::default();

    let entries = match fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", log_dir.display())),
    };

    let now = SystemTime::now();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if !file_type.is_file() {
            continue;
        }

        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(prefix) = owning_prefix(&name) else {
            continue;
        };

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let bytes = metadata.len();

        let is_live = live_prefixes.contains(&prefix);
        // An mtime we failed to read is treated as "just modified" — protect
        // the file rather than risk deleting something active.
        let is_recent = metadata
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .map(|age| age.as_secs() < min_idle_secs)
            .unwrap_or(true);

        if is_live || is_recent {
            report.kept_files += 1;
            report.kept_bytes += bytes;
            continue;
        }

        if dry_run {
            report.deleted_files += 1;
            report.deleted_bytes += bytes;
            continue;
        }

        match fs::remove_file(entry.path()) {
            Ok(()) => {
                report.deleted_files += 1;
                report.deleted_bytes += bytes;
            }
            Err(e) => {
                report
                    .errors
                    .push(format!("{}: {e}", entry.path().display()));
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn touch(dir: &Path, name: &str) {
        let mut f = fs::File::create(dir.join(name)).unwrap();
        f.write_all(b"line one\n").unwrap();
    }

    #[test]
    fn owning_prefix_strips_all_three_suffix_forms() {
        assert_eq!(
            owning_prefix("needle-glm-armor.log").as_deref(),
            Some("needle-glm-armor")
        );
        assert_eq!(
            owning_prefix("needle-glm-armor.log.7").as_deref(),
            Some("needle-glm-armor")
        );
        assert_eq!(
            owning_prefix("needle-glm-armor.stderr.log").as_deref(),
            Some("needle-glm-armor")
        );
    }

    #[test]
    fn owning_prefix_rejects_unrelated_names() {
        assert_eq!(owning_prefix("readme.txt"), None);
        assert_eq!(owning_prefix("needle-glm-armor.jsonl"), None);
        assert_eq!(owning_prefix("needle-glm-armor.log.abc"), None);
    }

    #[test]
    fn orphan_with_no_live_match_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "needle-cgov-sonnet-dead.log");

        let live = HashSet::new();
        let report = sweep_orphaned_logs(dir.path(), &live, 0, false).unwrap();

        assert_eq!(report.deleted_files, 1);
        assert_eq!(report.kept_files, 0);
        assert!(!dir.path().join("needle-cgov-sonnet-dead.log").exists());
    }

    #[test]
    fn live_prefix_is_never_deleted_even_with_zero_idle_threshold() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "needle-glm-armor.log");

        let mut live = HashSet::new();
        live.insert("needle-glm-armor".to_string());
        let report = sweep_orphaned_logs(dir.path(), &live, 0, false).unwrap();

        assert_eq!(report.deleted_files, 0);
        assert_eq!(report.kept_files, 1);
        assert!(dir.path().join("needle-glm-armor.log").exists());
    }

    #[test]
    fn freshly_written_file_is_protected_by_recency_even_if_not_live() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "needle-glm-vista.log");

        let live = HashSet::new();
        // A large idle threshold means "just written" always counts as recent.
        let report = sweep_orphaned_logs(dir.path(), &live, 3600, false).unwrap();

        assert_eq!(report.deleted_files, 0);
        assert_eq!(report.kept_files, 1);
    }

    #[test]
    fn dry_run_reports_without_deleting() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "needle-cgov-sonnet-dead.log");

        let live = HashSet::new();
        let report = sweep_orphaned_logs(dir.path(), &live, 0, true).unwrap();

        assert_eq!(report.deleted_files, 1);
        assert!(dir.path().join("needle-cgov-sonnet-dead.log").exists());
    }

    #[test]
    fn rotated_and_stderr_siblings_of_one_orphan_are_all_deleted() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "needle-cgov-sonnet-dead.log");
        touch(dir.path(), "needle-cgov-sonnet-dead.log.1");
        touch(dir.path(), "needle-cgov-sonnet-dead.stderr.log");

        let live = HashSet::new();
        let report = sweep_orphaned_logs(dir.path(), &live, 0, false).unwrap();

        assert_eq!(report.deleted_files, 3);
    }

    #[test]
    fn subdirectories_are_never_descended_into() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        fs::create_dir(&archive).unwrap();
        touch(&archive, "needle-cgov-sonnet-dead.log");

        let live = HashSet::new();
        let report = sweep_orphaned_logs(dir.path(), &live, 0, false).unwrap();

        assert_eq!(report.deleted_files, 0);
        assert_eq!(report.kept_files, 0);
        assert!(archive.join("needle-cgov-sonnet-dead.log").exists());
    }

    #[test]
    fn unrecognized_file_names_are_left_alone_and_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "some-other-file.txt");

        let live = HashSet::new();
        let report = sweep_orphaned_logs(dir.path(), &live, 0, false).unwrap();

        assert_eq!(report.deleted_files, 0);
        assert_eq!(report.kept_files, 0);
        assert!(dir.path().join("some-other-file.txt").exists());
    }

    #[test]
    fn missing_log_dir_returns_empty_report() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");

        let live = HashSet::new();
        let report = sweep_orphaned_logs(&missing, &live, 0, false).unwrap();

        assert_eq!(report.deleted_files, 0);
        assert_eq!(report.kept_files, 0);
    }
}
