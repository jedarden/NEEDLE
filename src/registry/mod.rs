//! Worker state registry — shared JSON file tracking all active workers.
//!
//! The registry is informational: "who is running, what are they doing, how
//! many beads processed." It is NOT used for coordination — heartbeats handle
//! that.
//!
//! File: `~/.needle/state/workers.json`
//! Access: flock-protected read-modify-write (atomic updates).
//!
//! Depends on: `config`, `types`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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
                return true;
            }
            // kill failed: check errno to distinguish EPERM from ESRCH.
            // We must read errno immediately after kill, before any other syscalls.
            let errno = *libc::__errno_location();
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
    /// Model name (if known).
    pub model: Option<String>,
    /// Provider name (e.g., `anthropic`, `openai`).
    pub provider: Option<String>,
    /// When the worker started.
    pub started_at: DateTime<Utc>,
    /// Number of beads processed so far.
    pub beads_processed: u64,
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
        })
    }

    /// Update a worker's beads_processed count.
    pub fn update_beads_processed(&self, worker_id: &str, beads_processed: u64) -> Result<()> {
        self.modify(|reg| {
            if let Some(entry) = reg.workers.iter_mut().find(|w| w.id == worker_id) {
                entry.beads_processed = beads_processed;
            }
        })
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

            let cleaned_reg = RegistryFile {
                workers: live_workers.clone(),
                updated_at: Utc::now(),
            };
            if let Err(e) = self.write_cleaned(cleaned_reg) {
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
    /// No file locking: read() already released its shared lock before returning.
    /// The atomic rename ensures concurrent writers don't corrupt the file —
    /// one write wins cleanly.
    fn write_cleaned(&self, reg: RegistryFile) -> Result<()> {
        let tmp_path = self.path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(&reg).context("failed to serialize registry")?;
        std::fs::write(&tmp_path, &json)
            .with_context(|| format!("failed to write temp registry: {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &self.path).with_context(|| {
            format!("failed to rename temp registry to: {}", self.path.display())
        })?;
        Ok(())
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Read the registry file, returning a default if it doesn't exist.
    fn read(&self) -> Result<RegistryFile> {
        if !self.path.exists() {
            return Ok(RegistryFile::default());
        }

        let file = std::fs::File::open(&self.path)
            .with_context(|| format!("failed to open registry: {}", self.path.display()))?;

        // Shared lock for reading (use fs2 trait method explicitly for MSRV compat).
        FileExt::lock_shared(&file)
            .with_context(|| format!("failed to acquire shared lock: {}", self.path.display()))?;

        let content = std::fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read registry: {}", self.path.display()))?;

        FileExt::unlock(&file)
            .with_context(|| format!("failed to release lock: {}", self.path.display()))?;

        if content.trim().is_empty() {
            return Ok(RegistryFile::default());
        }

        serde_json::from_str(&content)
            .with_context(|| format!("failed to parse registry JSON: {}", self.path.display()))
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

        // Open or create the file.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)
            .with_context(|| format!("failed to open registry: {}", self.path.display()))?;

        // Exclusive lock for writing (use fs2 trait method explicitly for MSRV compat).
        FileExt::lock_exclusive(&file).with_context(|| {
            format!("failed to acquire exclusive lock: {}", self.path.display())
        })?;

        // Read current contents.
        let content = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut reg: RegistryFile = if content.trim().is_empty() {
            RegistryFile::default()
        } else {
            serde_json::from_str(&content).unwrap_or_default()
        };

        // Apply the mutation.
        mutator(&mut reg);
        reg.updated_at = Utc::now();

        // Write back atomically: write to temp file, then rename.
        let tmp_path = self.path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(&reg).context("failed to serialize registry")?;
        std::fs::write(&tmp_path, &json)
            .with_context(|| format!("failed to write temp registry: {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &self.path).with_context(|| {
            format!("failed to rename temp registry to: {}", self.path.display())
        })?;

        // Release lock (use fs2 trait method explicitly for MSRV compat).
        FileExt::unlock(&file)
            .with_context(|| format!("failed to release lock: {}", self.path.display()))?;

        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(id: &str) -> WorkerEntry {
        WorkerEntry {
            id: id.to_string(),
            pid: std::process::id(),
            workspace: PathBuf::from("/tmp/test-workspace"),
            agent: "claude".to_string(),
            model: Some("sonnet".to_string()),
            provider: Some("anthropic".to_string()),
            started_at: Utc::now(),
            beads_processed: 0,
        }
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
}
