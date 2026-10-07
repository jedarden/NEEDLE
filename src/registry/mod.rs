//! Worker state registry — shared JSON file tracking all active workers.
//!
//! The registry is informational: "who is running, what are they doing, how
//! many beads processed." It is NOT used for coordination — heartbeats handle
//! that.
//!
//! File: `~/.needle/state/workers.json`
//! Access: flock-protected read-modify-write (atomic updates).  The lock is a
//! stable sidecar (`workers.json.lock`) because replacing `workers.json` also
//! replaces the inode that an inode lock would protect.
//!
//! Depends on: `config`, `types`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// PID liveness checking (platform-specific)
// ──────────────────────────────────────────────────────────────────────────────

/// Check if a process with the given PID is currently running.
///
/// Returns `false` if the PID does not exist or if we lack permission to signal it.
/// This is best-effort: if we can't determine liveness, we assume the process is dead
/// to avoid counting stale entries toward concurrency limits.
pub fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Use libc::kill with signal 0 to check if process exists.
        // SAFETY: kill(pid, 0) only checks existence, doesn't send a signal.
        unsafe {
            let ret = libc::kill(pid as i32, 0);
            if ret == 0 {
                // kill succeeded: process exists and we have permission to signal it.
                // On Linux, kill(pid, 0) also succeeds for a zombie (state Z) — it
                // still holds a PID table entry, it has just already exited and is
                // awaiting reaping. Treat zombies as not-alive so callers (supervisor
                // capacity accounting, mend's liveness check) don't count a dead
                // worker as live. See ADR-010 / GH #12.
                if is_zombie_linux(pid) == Some(true) {
                    return false;
                }
                return true;
            }
            // kill failed: check errno to distinguish EPERM from ESRCH.
            // We must read errno immediately after kill, before any other syscalls.
            #[cfg(target_os = "linux")]
            let errno = *libc::__errno_location();
            #[cfg(target_os = "macos")]
            let errno = *libc::__error();
            match errno {
                libc::ESRCH => {
                    // No such process: PID is dead.
                    false
                }
                libc::EPERM => {
                    // Process exists but we don't have permission to signal it.
                    // We treat this as "alive" since the process actually exists.
                    true
                }
                _ => {
                    // Other errors (e.g., EINVAL for invalid signal).
                    // Conservatively treat as dead to avoid false positives.
                    false
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        // On non-Unix platforms, conservatively return false (assume dead).
        // This prevents false positives where dead PIDs are counted as live.
        // TODO: Implement Windows liveness check via OpenProcess if needed.
        false
    }
}

/// Check `/proc/<pid>/stat` for zombie state (`Z`) on Linux.
///
/// Returns `None` if the check can't be performed (non-Linux, unreadable
/// `/proc` entry — e.g. a race with the process exiting, or an unexpected
/// format) — callers must treat `None` as "undetermined", not "not a
/// zombie", and fall back to the `kill(pid, 0)` result.
///
/// `/proc/<pid>/stat` format is `pid (comm) state ...`; `comm` can itself
/// contain spaces or parentheses (it's the raw, possibly-truncated process
/// name), so the state field is located via the *last* `)` in the line
/// rather than by splitting on whitespace.
#[cfg(target_os = "linux")]
fn is_zombie_linux(pid: u32) -> Option<bool> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rfind(')')?;
    let state = stat.get(after_comm + 1..)?.trim_start().chars().next()?;
    Some(state == 'Z')
}

#[cfg(not(target_os = "linux"))]
fn is_zombie_linux(_pid: u32) -> Option<bool> {
    None
}

// ──────────────────────────────────────────────────────────────────────────────
// WorkerEntry
// ──────────────────────────────────────────────────────────────────────────────

/// A single worker entry in the registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerEntry {
    /// Fully-qualified worker identity (`{adapter}-{worker_id}`, e.g., `claude-foxtrot`).
    pub id: String,
    /// Process ID of the worker.
    pub pid: u32,
    /// Workspace the worker is processing beads from.
    pub workspace: PathBuf,
    /// Agent adapter name.
    pub agent: String,
    /// Worker adapter's requested model alias (if known). This is worker
    /// configuration identity, not provider-returned per-attempt evidence.
    pub model: Option<String>,
    /// Provider name (e.g., `anthropic`, `openai`).
    pub provider: Option<String>,
    /// When the worker started.
    pub started_at: DateTime<Utc>,
    /// Number of dispatch cycles so far — one per dispatch, regardless of
    /// outcome. This is the denominator for churn metrics, not a completion
    /// count; `beads_completed` counts cycles that actually shipped.
    pub beads_processed: u64,
    /// Number of dispatch cycles that ended with the bead actually closed.
    ///
    /// Always ≤ [`WorkerEntry::beads_processed`]: a cycle that releases,
    /// defers, quarantines, or errors does not complete anything. Optional
    /// on read so entries written before this field existed load as 0.
    #[serde(default)]
    pub beads_completed: u64,
    /// Number of times the configuration has been reloaded since boot.
    /// Used by `needle config --dump --live` to show the live config generation.
    #[serde(default)]
    pub config_reload_generation: u64,
    /// Last state the worker reported, when it reported one.
    ///
    /// `None` for entries written before this field existed, and for launchers
    /// that only spawn workers. Heartbeat files carry the live state; this
    /// registry copy preserves it for dashboard polling between heartbeats and
    /// for states reached before the health monitor exists (a worker holding
    /// in `AdmissionBlocked` during boot has no heartbeat file yet — see plan
    /// revision 24 §4.6 / N-T33).
    #[serde(default)]
    pub state: Option<crate::types::WorkerState>,
}

/// The user-facing configuration dump published by a running worker.
///
/// This intentionally stores the already-formatted dump rather than a full
/// [`Config`].  The latter would copy resolved secrets (for example OTLP
/// headers) into the shared state directory.  The CLI only needs the fields
/// that `config --dump` exposes, with and without source annotations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LiveConfigSnapshot {
    /// Values formatted without source annotations.
    pub values: Vec<String>,
    /// Values formatted for `needle config --dump --show-source`.
    pub values_with_sources: Vec<String>,
    /// Number of successful config reloads since the worker booted.
    pub reload_generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LiveConfigFile {
    snapshots: HashMap<String, LiveConfigSnapshot>,
    updated_at: DateTime<Utc>,
}

impl Default for LiveConfigFile {
    fn default() -> Self {
        Self {
            snapshots: HashMap::new(),
            updated_at: Utc::now(),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// RegistryFile
// ──────────────────────────────────────────────────────────────────────────────

/// The on-disk JSON structure for `workers.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryFile {
    pub workers: Vec<WorkerEntry>,
    pub updated_at: DateTime<Utc>,
}

impl Default for RegistryFile {
    fn default() -> Self {
        RegistryFile {
            workers: Vec::new(),
            updated_at: Utc::now(),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Registry
// ──────────────────────────────────────────────────────────────────────────────

/// Worker state registry with flock-protected read-modify-write access.
#[derive(Clone)]
pub struct Registry {
    path: PathBuf,
}

impl Registry {
    /// Create a registry instance pointing at the given state directory.
    ///
    /// The directory will be created on first write if it does not exist.
    pub fn new(state_dir: &Path) -> Self {
        Registry {
            path: state_dir.join("workers.json"),
        }
    }

    /// Create a registry using the default state directory (`~/.needle/state`).
    pub fn default_location(needle_home: &Path) -> Self {
        Self::new(&needle_home.join("state"))
    }

    /// Path to the workers.json file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Register a worker on startup.
    ///
    /// If a worker with the same ID already exists, it is replaced (handles
    /// stale entries from previous runs).
    pub fn register(&self, entry: WorkerEntry) -> Result<()> {
        self.modify(|reg| {
            // Remove any existing entry with the same ID (stale from previous run).
            reg.workers.retain(|w| w.id != entry.id);
            reg.workers.push(entry);
        })
    }

    /// Deregister a worker on shutdown.
    ///
    /// Best-effort: errors are logged but not propagated to avoid masking
    /// the real shutdown reason.
    pub fn deregister(&self, worker_id: &str) -> Result<()> {
        self.modify(|reg| {
            reg.workers.retain(|w| w.id != worker_id);
        })?;
        self.remove_live_config(worker_id)
    }

    /// Publish the configuration currently in use by a worker.
    pub fn update_live_config(&self, worker_id: &str, snapshot: LiveConfigSnapshot) -> Result<()> {
        self.modify_live_configs(|configs| {
            configs.snapshots.insert(worker_id.to_string(), snapshot);
        })
    }

    /// Read a worker's most recently published live configuration.
    pub fn live_config(&self, worker_id: &str) -> Result<Option<LiveConfigSnapshot>> {
        Ok(self.read_live_configs()?.snapshots.get(worker_id).cloned())
    }

    /// Update a worker's beads_processed count.
    pub fn update_beads_processed(&self, worker_id: &str, beads_processed: u64) -> Result<()> {
        self.modify(|reg| {
            if let Some(entry) = reg.workers.iter_mut().find(|w| w.id == worker_id) {
                entry.beads_processed = beads_processed;
            }
        })
    }

    /// Update a worker's dispatch-cycle and completion counts in one write.
    ///
    /// `beads_processed` counts every dispatch cycle; `beads_completed`
    /// counts only the cycles that ended with the bead closed. Writing them
    /// together keeps the pair coherent for readers polling the registry
    /// between cycles.
    pub fn update_bead_counts(
        &self,
        worker_id: &str,
        beads_processed: u64,
        beads_completed: u64,
    ) -> Result<()> {
        self.modify(|reg| {
            if let Some(entry) = reg.workers.iter_mut().find(|w| w.id == worker_id) {
                entry.beads_processed = beads_processed;
                entry.beads_completed = beads_completed;
            }
        })
    }

    /// Update a worker's current workspace.
    pub fn update_workspace(&self, worker_id: &str, workspace: &Path) -> Result<()> {
        self.modify(|reg| {
            if let Some(entry) = reg.workers.iter_mut().find(|w| w.id == worker_id) {
                entry.workspace = workspace.to_path_buf();
            }
        })
    }

    /// Update a worker's reported state.
    ///
    /// Writing `None` clears a previously recorded state. An unknown worker
    /// ID is a no-op, not an error — the entry may have deregistered between
    /// the caller's read and this write.
    ///
    /// Best-effort by callers: a registry write failure must never take a
    /// worker down, so errors are logged and swallowed here like
    /// [`Registry::deregister`].
    pub fn update_state(&self, worker_id: &str, state: Option<crate::types::WorkerState>) {
        let result = self.modify(|reg| {
            if let Some(entry) = reg.workers.iter_mut().find(|w| w.id == worker_id) {
                entry.state = state.clone();
            }
        });
        if let Err(e) = result {
            tracing::warn!(
                error = %e,
                worker_id,
                "failed to update worker state in registry; status will show the stale state"
            );
        }
    }

    /// Read a single worker's entry, if it is registered and alive.
    pub fn get(&self, worker_id: &str) -> Result<Option<WorkerEntry>> {
        Ok(self.list()?.into_iter().find(|w| w.id == worker_id))
    }

    /// Read every registry entry without applying PID liveness filtering.
    ///
    /// Fleet-facing process reconciliation needs the raw view so it can
    /// distinguish a registration whose PID is dead (or has been reused by a
    /// non-NEEDLE process) from a worker that is actually running. Callers
    /// that only need live entries should continue to use [`Registry::list`].
    pub fn list_all(&self) -> Result<Vec<WorkerEntry>> {
        Ok(self.read()?.workers)
    }

    /// Read all registered workers, filtering out entries for dead PIDs.
    ///
    /// This lazy cleanup ensures that workers killed via SIGKILL or crashes
    /// don't accumulate in the registry and falsely count toward concurrency
    /// limits.
    pub fn list(&self) -> Result<Vec<WorkerEntry>> {
        let reg = self.read()?;
        let total_count = reg.workers.len();
        let live_workers: Vec<WorkerEntry> = reg
            .workers
            .into_iter()
            .filter(|w| is_pid_alive(w.pid))
            .collect();

        // If we filtered out dead entries, persist the cleanup so we don't
        // need to re-check their PIDs on every read. This is best-effort:
        // if the write fails (disk full, race with another writer), we still
        // return the correctly filtered list — we'll just re-filter next time.
        let live_count = live_workers.len();
        if live_count != total_count {
            let dead_count = total_count - live_count;
            tracing::debug!(dead_count, "filtered dead worker entries from registry");

            if let Err(e) = self.write_cleaned() {
                tracing::warn!(
                    error = %e,
                    "failed to persist dead PID cleanup; will re-filter next read"
                );
            }
        }

        Ok(live_workers)
    }

    /// Write a cleaned registry file (used by list() to persist dead PID filtering).
    ///
    /// Persist dead-PID cleanup under the same read-modify-write lock as all
    /// other registry mutations.  Re-reading while holding the lock matters:
    /// a worker may register between `list()`'s snapshot and this cleanup.
    fn write_cleaned(&self) -> Result<()> {
        self.modify(|reg| {
            reg.workers.retain(|worker| is_pid_alive(worker.pid));
        })
    }

    fn live_config_path(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("live-config.json")
    }

    fn read_live_configs(&self) -> Result<LiveConfigFile> {
        let path = self.live_config_path();
        if !path.exists() {
            return Ok(LiveConfigFile::default());
        }

        let file = std::fs::File::open(&path)
            .with_context(|| format!("failed to open live config: {}", path.display()))?;
        FileExt::lock_shared(&file)
            .with_context(|| format!("failed to acquire shared lock: {}", path.display()))?;
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read live config: {}", path.display()))?;
        FileExt::unlock(&file)
            .with_context(|| format!("failed to release lock: {}", path.display()))?;

        if content.trim().is_empty() {
            return Ok(LiveConfigFile::default());
        }

        serde_json::from_str(&content)
            .with_context(|| format!("failed to parse live config JSON: {}", path.display()))
    }

    fn modify_live_configs<F>(&self, mutator: F) -> Result<()>
    where
        F: FnOnce(&mut LiveConfigFile),
    {
        let path = self.live_config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create live config directory: {}",
                    parent.display()
                )
            })?;
        }

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("failed to open live config: {}", path.display()))?;
        FileExt::lock_exclusive(&file)
            .with_context(|| format!("failed to acquire exclusive lock: {}", path.display()))?;

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let mut configs: LiveConfigFile = if content.trim().is_empty() {
            LiveConfigFile::default()
        } else {
            serde_json::from_str(&content).unwrap_or_default()
        };
        mutator(&mut configs);
        configs.updated_at = Utc::now();

        let tmp_path = path.with_extension("json.tmp");
        let json =
            serde_json::to_string_pretty(&configs).context("failed to serialize live config")?;
        std::fs::write(&tmp_path, &json).with_context(|| {
            format!(
                "failed to write temporary live config: {}",
                tmp_path.display()
            )
        })?;
        std::fs::rename(&tmp_path, &path)
            .with_context(|| format!("failed to replace live config: {}", path.display()))?;

        FileExt::unlock(&file)
            .with_context(|| format!("failed to release lock: {}", path.display()))?;
        Ok(())
    }

    fn remove_live_config(&self, worker_id: &str) -> Result<()> {
        let path = self.live_config_path();
        if !path.exists() {
            return Ok(());
        }

        self.modify_live_configs(|configs| {
            configs.snapshots.remove(worker_id);
        })
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Stable lock path for the registry.  Never lock `workers.json` itself:
    /// atomic replacement gives the next writer a different inode and makes
    /// an inode lock ineffective for serialization.
    fn lock_path(&self) -> PathBuf {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workers.json".to_string());
        self.path.with_file_name(format!("{name}.lock"))
    }

    fn open_lock(&self) -> Result<std::fs::File> {
        if let Some(parent) = self.lock_path().parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create registry directory: {}", parent.display())
            })?;
        }
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path())
            .with_context(|| {
                format!(
                    "failed to open registry lock: {}",
                    self.lock_path().display()
                )
            })
    }

    /// Read the registry while the caller holds the stable lock.
    fn read_locked(&self) -> Result<RegistryFile> {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RegistryFile::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read registry: {}", self.path.display()));
            }
        };

        if content.trim().is_empty() {
            bail!("registry file is empty: {}", self.path.display());
        }

        serde_json::from_str(&content)
            .with_context(|| format!("failed to parse registry JSON: {}", self.path.display()))
    }

    /// Replace the registry atomically using a unique temporary file in the
    /// same directory.  A shared fixed-name temporary file lets concurrent
    /// writers overwrite each other's staged contents before either rename.
    fn write_atomically(&self, reg: &RegistryFile) -> Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let mut temp = tempfile::Builder::new()
            .prefix(".workers.json.")
            .suffix(".tmp")
            .tempfile_in(parent)
            .with_context(|| {
                format!(
                    "failed to create temporary registry in: {}",
                    parent.display()
                )
            })?;
        serde_json::to_writer_pretty(temp.as_file_mut(), reg)
            .context("failed to serialize registry")?;
        temp.as_file_mut()
            .sync_all()
            .context("failed to flush temporary registry")?;
        temp.persist(&self.path).map_err(|error| {
            anyhow::anyhow!(
                "failed to rename temporary registry to {}: {}",
                self.path.display(),
                error
            )
        })?;
        Ok(())
    }

    /// Read the registry file, returning a default if it doesn't exist.
    fn read(&self) -> Result<RegistryFile> {
        let lock = self.open_lock()?;
        FileExt::lock_shared(&lock).with_context(|| {
            format!(
                "failed to acquire shared registry lock: {}",
                self.lock_path().display()
            )
        })?;
        self.read_locked()
    }

    /// Perform a flock-protected read-modify-write operation.
    fn modify<F>(&self, mutator: F) -> Result<()>
    where
        F: FnOnce(&mut RegistryFile),
    {
        // Ensure the parent directory exists.
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create registry directory: {}", parent.display())
            })?;
        }

        let lock = self.open_lock()?;
        FileExt::lock_exclusive(&lock).with_context(|| {
            format!(
                "failed to acquire exclusive registry lock: {}",
                self.lock_path().display()
            )
        })?;

        let mut reg = self.read_locked()?;

        // Apply the mutation.
        mutator(&mut reg);
        reg.updated_at = Utc::now();

        self.write_atomically(&reg)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::fixture_root;
    use crate::types::WorkerState;
    use std::time::{Duration, Instant};

    const CHILD_DIR_ENV: &str = "NEEDLE_REGISTRY_TEST_DIR";
    const CHILD_ID_ENV: &str = "NEEDLE_REGISTRY_TEST_ID";
    const CHILD_COUNT_ENV: &str = "NEEDLE_REGISTRY_TEST_COUNT";

    fn make_entry(id: &str) -> WorkerEntry {
        WorkerEntry {
            id: id.to_string(),
            pid: std::process::id(),
            workspace: fixture_root("test-workspace"),
            agent: "claude".to_string(),
            model: Some("sonnet".to_string()),
            provider: Some("anthropic".to_string()),
            started_at: Utc::now(),
            beads_processed: 0,
            beads_completed: 0,
            config_reload_generation: 0,
            state: None,
        }
    }

    #[test]
    fn update_state_round_trips_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        reg.register(make_entry("alpha")).unwrap();

        reg.update_state("alpha", Some(WorkerState::AdmissionBlocked));
        let entry = reg.get("alpha").unwrap().expect("entry present");
        assert_eq!(entry.state, Some(WorkerState::AdmissionBlocked));

        reg.update_state("alpha", Some(WorkerState::Selecting));
        let entry = reg.get("alpha").unwrap().expect("entry present");
        assert_eq!(entry.state, Some(WorkerState::Selecting));

        // Back to "no state reported" once normal heartbeat-driven states resume.
        reg.update_state("alpha", None);
        let entry = reg.get("alpha").unwrap().expect("entry present");
        assert_eq!(entry.state, None);

        // Unknown worker is a no-op, not an error.
        reg.update_state("ghost", Some(WorkerState::AdmissionBlocked));
    }

    #[test]
    fn entry_without_state_field_reads_as_none() {
        // A file written by a binary that predates the `state` field must
        // still parse: a hard failure here would make `list()` error, and the
        // write path's `unwrap_or_default()` would then wipe the registry.
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        let legacy = r#"{
  "workers": [
    {
      "id": "legacy-worker",
      "pid": __PID__,
      "workspace": "/tmp/ws",
      "agent": "claude",
      "model": "sonnet",
      "provider": "anthropic",
      "started_at": "2026-09-03T14:00:00Z",
      "beads_processed": 1,
      "config_reload_generation": 0
    }
  ],
  "updated_at": "2026-09-03T14:00:00Z"
}"#
        // list() drops entries whose pid is no longer alive, so the fixture
        // has to claim a live one. What is under test is the absent `state`
        // field, not liveness.
        .replace("__PID__", &std::process::id().to_string());
        std::fs::write(reg.path(), legacy).unwrap();

        let workers = reg.list().unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].state, None);
    }

    #[test]
    fn admission_blocked_state_survives_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        let mut entry = make_entry("blocked-worker");
        entry.pid = std::process::id(); // must be alive or list() filters it
        entry.state = Some(WorkerState::AdmissionBlocked);
        reg.register(entry).unwrap();

        // Re-read from disk through a fresh handle, the way a polling
        // dashboard's `needle status` invocation would.
        let reread = Registry::new(dir.path());
        let entry = reread
            .get("blocked-worker")
            .unwrap()
            .expect("entry present");
        assert_eq!(entry.state, Some(WorkerState::AdmissionBlocked));
    }

    /// A PID that is definitely not in use on any real system.
    ///
    /// This must be a value that:
    /// 1. Is positive when cast to i32 (avoid -1 special case)
    /// 2. Is unlikely to ever be assigned by the kernel
    ///
    /// We use 9999999 which is well beyond typical PID ranges.
    const DEAD_PID: u32 = 9999999;

    #[test]
    fn register_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());

        reg.register(make_entry("alpha")).unwrap();
        reg.register(make_entry("bravo")).unwrap();

        let all_workers = reg.list_all().unwrap();
        assert_eq!(all_workers.len(), 2);
        let workers = reg.list().unwrap();
        assert_eq!(workers.len(), 2);
        assert_eq!(workers[0].id, "alpha");
        assert_eq!(workers[1].id, "bravo");
    }

    #[test]
    fn deregister_removes_worker() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());

        reg.register(make_entry("alpha")).unwrap();
        reg.register(make_entry("bravo")).unwrap();
        reg.deregister("alpha").unwrap();

        let workers = reg.list().unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].id, "bravo");
    }

    #[test]
    fn deregister_nonexistent_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());

        reg.register(make_entry("alpha")).unwrap();
        reg.deregister("nonexistent").unwrap();

        let workers = reg.list().unwrap();
        assert_eq!(workers.len(), 1);
    }

    #[test]
    fn register_replaces_stale_entry() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());

        let mut entry1 = make_entry("alpha");
        entry1.beads_processed = 5;
        reg.register(entry1).unwrap();

        let entry2 = make_entry("alpha");
        reg.register(entry2).unwrap();

        let workers = reg.list().unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].beads_processed, 0);
    }

    #[test]
    fn update_bead_counts_writes_both_counters() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());

        reg.register(make_entry("alpha")).unwrap();
        reg.update_bead_counts("alpha", 9, 4).unwrap();

        let workers = reg.list().unwrap();
        assert_eq!(workers[0].beads_processed, 9);
        assert_eq!(workers[0].beads_completed, 4);
    }

    #[test]
    fn update_beads_processed() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());

        reg.register(make_entry("alpha")).unwrap();
        reg.update_beads_processed("alpha", 42).unwrap();

        let workers = reg.list().unwrap();
        assert_eq!(workers[0].beads_processed, 42);
    }

    #[test]
    fn update_nonexistent_worker_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());

        reg.register(make_entry("alpha")).unwrap();
        reg.update_beads_processed("ghost", 10).unwrap();

        let workers = reg.list().unwrap();
        assert_eq!(workers[0].beads_processed, 0);
    }

    #[test]
    fn empty_registry_returns_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());

        let workers = reg.list().unwrap();
        assert!(workers.is_empty());
    }

    #[test]
    fn live_config_snapshot_round_trips_and_is_removed_with_worker() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        reg.register(make_entry("alpha")).unwrap();

        let snapshot = LiveConfigSnapshot {
            values: vec!["worker.max_workers: 4".to_string()],
            values_with_sources: vec!["worker.max_workers: 4 (from: built-in default)".to_string()],
            reload_generation: 2,
        };
        reg.update_live_config("alpha", snapshot.clone()).unwrap();

        assert_eq!(reg.live_config("alpha").unwrap(), Some(snapshot));
        reg.deregister("alpha").unwrap();
        assert_eq!(reg.live_config("alpha").unwrap(), None);
    }

    #[test]
    fn registry_file_is_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());

        reg.register(make_entry("alpha")).unwrap();

        let content = std::fs::read_to_string(reg.path()).unwrap();
        let parsed: RegistryFile = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.workers.len(), 1);
    }

    #[test]
    fn default_location_uses_state_subdir() {
        let reg = Registry::default_location(Path::new("/home/test/.needle"));
        assert_eq!(
            reg.path(),
            Path::new("/home/test/.needle/state/workers.json")
        );
    }

    #[test]
    fn concurrent_registration_no_corruption() {
        let dir = tempfile::tempdir().unwrap();

        // Simulate sequential registrations (true concurrency tested in integration tests).
        let reg = Registry::new(dir.path());
        for i in 0..10 {
            reg.register(make_entry(&format!("worker-{i}"))).unwrap();
        }

        let workers = reg.list().unwrap();
        assert_eq!(workers.len(), 10);
    }

    /// Child-process body for `concurrent_process_registration_preserves_all`.
    ///
    /// The filesystem barrier makes every child reach the write together,
    /// rather than relying on a scheduler-dependent sleep to expose the race.
    #[test]
    fn concurrent_process_registration_worker() {
        let Some(dir) = std::env::var_os(CHILD_DIR_ENV) else {
            return;
        };
        let id = std::env::var(CHILD_ID_ENV).unwrap();
        let expected = std::env::var(CHILD_COUNT_ENV)
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let dir = PathBuf::from(dir);
        std::fs::write(dir.join(format!("ready-{id}")), b"ready").unwrap();

        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let ready = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("ready-"))
                .count();
            if ready == expected {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "concurrent registry test barrier timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        Registry::new(&dir)
            .register(make_entry(&id))
            .expect("child registration should succeed");
    }

    #[test]
    fn concurrent_process_registration_preserves_all() {
        let dir = tempfile::tempdir().unwrap();
        let count = 12usize;
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::with_capacity(count);

        for index in 0..count {
            children.push(
                std::process::Command::new(&executable)
                    .args([
                        "--exact",
                        "registry::tests::concurrent_process_registration_worker",
                        "--test-threads=1",
                    ])
                    .env(CHILD_DIR_ENV, dir.path())
                    .env(CHILD_ID_ENV, format!("process-worker-{index}"))
                    .env(CHILD_COUNT_ENV, count.to_string())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
        }

        for (index, mut child) in children.into_iter().enumerate() {
            let status = child.wait().unwrap();
            assert!(status.success(), "registry child {index} failed: {status}");
        }

        let workers = Registry::new(dir.path()).list_all().unwrap();
        let ids: std::collections::HashSet<_> = workers.iter().map(|worker| &worker.id).collect();
        assert_eq!(workers.len(), count, "all child registrations must survive");
        assert_eq!(ids.len(), count, "child registrations must have unique IDs");
    }

    #[test]
    fn malformed_registry_is_rejected_without_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        reg.register(make_entry("existing")).unwrap();

        let corrupt = b"{\"workers\":[";
        std::fs::write(reg.path(), corrupt).unwrap();
        let before = std::fs::read(reg.path()).unwrap();

        let error = reg.register(make_entry("new")).unwrap_err();
        assert!(error.to_string().contains("failed to parse registry JSON"));
        assert_eq!(std::fs::read(reg.path()).unwrap(), before);
        assert!(
            reg.list_all().is_err(),
            "corrupt input must remain unreadable"
        );
    }

    #[test]
    fn registry_json_roundtrip() {
        let entry = make_entry("alpha");
        let reg_file = RegistryFile {
            workers: vec![entry.clone()],
            updated_at: Utc::now(),
        };

        let json = serde_json::to_string_pretty(&reg_file).unwrap();
        let parsed: RegistryFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.workers.len(), 1);
        assert_eq!(parsed.workers[0].id, entry.id);
        assert_eq!(parsed.workers[0].pid, entry.pid);
        assert_eq!(parsed.workers[0].agent, entry.agent);
    }

    #[test]
    fn list_filters_out_dead_pids() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());

        // Register a live worker (current process).
        reg.register(make_entry("live-alpha")).unwrap();

        // Create an entry with a fake (dead) PID.
        let mut dead_entry = make_entry("dead-worker");
        dead_entry.pid = DEAD_PID;

        // Write the dead entry directly to the file.
        let file_content = serde_json::to_string_pretty(&RegistryFile {
            workers: vec![make_entry("live-alpha"), dead_entry],
            updated_at: Utc::now(),
        })
        .unwrap();
        std::fs::write(reg.path(), file_content).unwrap();

        // list() should filter out the dead PID.
        let workers = reg.list().unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].id, "live-alpha");
    }

    #[test]
    fn list_persists_cleanup_after_filtering_dead_pids() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());

        // Register a live worker.
        reg.register(make_entry("live-bravo")).unwrap();

        // Create an entry with a fake (dead) PID.
        let mut dead_entry = make_entry("dead-charlie");
        dead_entry.pid = DEAD_PID;

        // Write both entries to the file.
        let file_content = serde_json::to_string_pretty(&RegistryFile {
            workers: vec![make_entry("live-bravo"), dead_entry],
            updated_at: Utc::now(),
        })
        .unwrap();
        std::fs::write(reg.path(), file_content).unwrap();

        // list() should filter and persist the cleanup.
        let workers = reg.list().unwrap();
        assert_eq!(workers.len(), 1);

        // Read the file directly to verify cleanup was persisted.
        let content = std::fs::read_to_string(reg.path()).unwrap();
        let parsed: RegistryFile = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.workers.len(), 1);
        assert_eq!(parsed.workers[0].id, "live-bravo");
    }

    #[test]
    fn is_pid_alive_returns_true_for_current_process() {
        // The current process's PID should be alive.
        assert!(is_pid_alive(std::process::id()));
    }

    #[test]
    fn is_pid_alive_returns_false_for_nonexistent_pid() {
        // A PID that's unlikely to exist should be dead.
        // DEAD_PID is well beyond any valid PID on real systems.
        assert!(!is_pid_alive(DEAD_PID));
    }

    // ── zombie-state tests (ADR-010 / GitHub issue jedarden/NEEDLE#12) ──
    //
    // kill(pid, 0) succeeds for a zombie exactly as it does for a live
    // process — these tests confirm is_pid_alive additionally treats a
    // zombie (state Z) as not-alive on Linux, rather than inheriting that
    // false positive.

    #[test]
    fn is_zombie_linux_returns_none_for_nonexistent_pid() {
        // Can't read /proc/<pid>/stat for a PID that doesn't exist — must
        // return None (undetermined), never Some(false) which callers could
        // misread as a confirmed non-zombie.
        assert_eq!(is_zombie_linux(DEAD_PID), None);
    }
}
