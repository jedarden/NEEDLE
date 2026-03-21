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
// WorkerEntry
// ──────────────────────────────────────────────────────────────────────────────

/// A single worker entry in the registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerEntry {
    /// Worker identifier (e.g., `alpha`, `bravo`).
    pub id: String,
    /// Process ID of the worker.
    pub pid: u32,
    /// Workspace the worker is processing beads from.
    pub workspace: PathBuf,
    /// Agent adapter name.
    pub agent: String,
    /// Model name (if known).
    pub model: Option<String>,
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

    /// Read all registered workers.
    pub fn list(&self) -> Result<Vec<WorkerEntry>> {
        let reg = self.read()?;
        Ok(reg.workers)
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
            started_at: Utc::now(),
            beads_processed: 0,
        }
    }

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
}
