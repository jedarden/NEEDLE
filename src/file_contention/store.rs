//! Checkout-local file-contention marker store (plan Phase 20, needle-ae8ff2a3).
//!
//! A marker advertises that a participating agent intends to mutate one
//! repository file. It lives beside the checkout, mirroring the source path:
//!
//! ```text
//! <repo>/src/worker/mod.rs
//! <repo>/.needle/locks/src/worker/mod.rs.lock
//! ```
//!
//! Markers are advisory runtime metadata for agents sharing ONE checkout.
//! They are never bead or attempt authority, never synchronized to another
//! checkout or host, and never repository content (see `git_safety`).
//! Creation and takeover happen inside a short `.registry.lock` critical
//! section, and multi-path requests are all-or-none.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Version of the on-disk marker record.
pub const SCHEMA_VERSION: u32 = 1;
/// Marker tree, relative to the repository root.
pub const LOCKS_DIR: &str = ".needle/locks";
/// Registry mutex file inside the marker tree.
pub const REGISTRY_LOCK: &str = ".registry.lock";
/// Suffix appended to the mirrored source path.
pub const MARKER_SUFFIX: &str = ".lock";

/// What the participant intends to do to the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteIntent {
    Create,
    Modify,
    Delete,
    /// Source of a rename; the destination is recorded separately.
    RenameFrom,
    /// Destination of a rename.
    RenameTo,
}

/// Who holds (or wants) a marker.
///
/// `bead_id` is absent for an interactive session. Either `attempt_id` or
/// `session_id` is required: together with `worker_id` and `host` it is the
/// ownership key that decides whether a marker is "ours".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub bead_id: Option<String>,
    pub attempt_id: Option<String>,
    pub session_id: Option<String>,
    pub worker_id: String,
    pub host: String,
    pub pid: u32,
}

impl Participant {
    /// Validate the identity a marker must carry.
    pub fn validate(&self) -> Result<()> {
        if self.worker_id.trim().is_empty() {
            bail!("file-contention participant requires a worker identity");
        }
        if self.host.trim().is_empty() {
            bail!("file-contention participant requires a host");
        }
        let has_attempt = self
            .attempt_id
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty());
        let has_session = self
            .session_id
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty());
        if !has_attempt && !has_session {
            bail!("file-contention participant requires an attempt_id or a session_id");
        }
        Ok(())
    }

    /// The attempt (worker dispatch) or session (interactive agent) key.
    pub fn owner_key(&self) -> Option<&str> {
        self.attempt_id
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| self.session_id.as_deref().filter(|v| !v.trim().is_empty()))
    }

    /// True when `record` was written by this same attempt/session.
    pub fn owns(&self, record: &MarkerRecord) -> bool {
        self.host == record.host
            && self.worker_id == record.worker_id
            && self.owner_key().is_some()
            && self.owner_key() == record.owner_key()
    }
}

/// The file as it was when the marker was first claimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub existed: bool,
    /// Modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: Option<i128>,
    pub size: Option<u64>,
    /// SHA-256 of the file content: the content identity. A timestamp-only
    /// change keeps this value; a content change does not.
    pub content_sha256: Option<String>,
}

/// One on-disk marker. Identifiers and file metadata only: no prompts,
/// credentials, bead bodies, or source contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkerRecord {
    pub schema_version: u32,
    pub path: String,
    pub bead_id: Option<String>,
    pub attempt_id: Option<String>,
    pub session_id: Option<String>,
    pub worker_id: String,
    pub host: String,
    pub pid: u32,
    pub intent: WriteIntent,
    /// Immutable: renewal never changes it.
    pub claimed_at: DateTime<Utc>,
    pub last_renewed_at: DateTime<Utc>,
    pub lease_expires_at: DateTime<Utc>,
    pub baseline: Baseline,
}

impl MarkerRecord {
    pub fn owner_key(&self) -> Option<&str> {
        self.attempt_id
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| self.session_id.as_deref().filter(|v| !v.trim().is_empty()))
    }

    pub fn lease_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.lease_expires_at
    }

    /// Structural self-consistency. A record failing this is ambiguous, never
    /// silently treated as safe.
    pub fn consistency_error(&self, expected_path: &str) -> Option<String> {
        if self.schema_version != SCHEMA_VERSION {
            return Some(format!(
                "unsupported schema_version {}",
                self.schema_version
            ));
        }
        if self.path != expected_path {
            return Some(format!(
                "record path {:?} does not match marker location {:?}",
                self.path, expected_path
            ));
        }
        if self.worker_id.trim().is_empty() || self.host.trim().is_empty() {
            return Some("record lacks worker or host identity".to_string());
        }
        if self.owner_key().is_none() {
            return Some("record lacks attempt_id and session_id".to_string());
        }
        if self.last_renewed_at < self.claimed_at {
            return Some("last_renewed_at precedes claimed_at".to_string());
        }
        if self.lease_expires_at < self.last_renewed_at {
            return Some("lease_expires_at precedes last_renewed_at".to_string());
        }
        None
    }
}

/// A marker as read from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerRead {
    Valid(Box<MarkerRecord>),
    /// Missing fields, bad JSON, wrong schema, or contradictory metadata.
    Corrupt {
        reason: String,
    },
}

/// One path in an acquisition request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathIntent {
    pub path: PathBuf,
    pub intent: WriteIntent,
}

/// Why a requested path could not be acquired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub path: String,
    pub existing: MarkerRead,
}

/// Result of an all-or-none acquisition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    /// Every path is now held (new, renewed, or taken over).
    Acquired(Vec<MarkerRecord>),
    /// At least one path is held by someone else; NOTHING was written.
    Conflicts(Vec<Conflict>),
}

/// Checkout-local marker store rooted at one repository.
#[derive(Debug, Clone)]
pub struct MarkerStore {
    repo_root: PathBuf,
    locks_root: PathBuf,
}

impl MarkerStore {
    /// Open the store for the repository at `repo_root` (canonicalized).
    pub fn open(repo_root: &Path) -> Result<Self> {
        let repo_root = repo_root
            .canonicalize()
            .with_context(|| format!("canonicalize repository root {}", repo_root.display()))?;
        if !repo_root.is_dir() {
            bail!("repository root {} is not a directory", repo_root.display());
        }
        let locks_root = repo_root.join(LOCKS_DIR);
        Ok(Self {
            repo_root,
            locks_root,
        })
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn locks_root(&self) -> &Path {
        &self.locks_root
    }

    /// Normalize a mutation target to a canonical repository-relative path.
    ///
    /// Rejects absolute paths outside the repository, `..`, symlink escapes,
    /// and anything under `.git/` or `.needle/`.
    pub fn normalize(&self, path: &Path) -> Result<String> {
        let relative: PathBuf = if path.is_absolute() {
            // Compare against the canonical root; the path itself may not
            // exist yet (create intent), so canonicalize its deepest
            // existing ancestor.
            let resolved = canonicalize_existing_prefix(path)?;
            match resolved.strip_prefix(&self.repo_root) {
                Ok(rel) => rel.to_path_buf(),
                Err(_) => bail!(
                    "path {} is outside the repository {}",
                    path.display(),
                    self.repo_root.display()
                ),
            }
        } else {
            path.to_path_buf()
        };

        let mut parts: Vec<String> = Vec::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => {
                    let part = part
                        .to_str()
                        .context("file-contention paths must be valid UTF-8")?;
                    parts.push(part.to_string());
                }
                Component::CurDir => {}
                Component::ParentDir => bail!("path {} contains '..'", path.display()),
                Component::RootDir | Component::Prefix(_) => {
                    bail!("path {} is not repository-relative", path.display())
                }
            }
        }
        if parts.is_empty() {
            bail!(
                "path {} names the repository root, not a file",
                path.display()
            );
        }
        if parts[0] == ".git" || parts[0] == ".needle" {
            bail!(
                "path {} is inside {}/, which is not a mutation target",
                path.display(),
                parts[0]
            );
        }

        // Symlink escape: whatever exists of the target must resolve inside
        // the repository.
        let joined = self.repo_root.join(parts.join("/"));
        let resolved = canonicalize_existing_prefix(&joined)?;
        if !resolved.starts_with(&self.repo_root) {
            bail!(
                "path {} resolves outside the repository via a symlink",
                path.display()
            );
        }
        Ok(parts.join("/"))
    }

    /// Marker file location for a normalized repository-relative path.
    pub fn marker_path(&self, rel: &str) -> PathBuf {
        self.locks_root.join(format!("{rel}{MARKER_SUFFIX}"))
    }

    /// Capture the current state of a repository file.
    pub fn baseline(&self, rel: &str) -> Result<Baseline> {
        let full = self.repo_root.join(rel);
        match fs::metadata(&full) {
            Ok(meta) if meta.is_file() => {
                let mtime_ns =
                    i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
                Ok(Baseline {
                    existed: true,
                    mtime_ns: Some(mtime_ns),
                    size: Some(meta.len()),
                    content_sha256: Some(sha256_file(&full)?),
                })
            }
            Ok(_) => bail!("{rel} exists but is not a regular file"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Baseline {
                existed: false,
                mtime_ns: None,
                size: None,
                content_sha256: None,
            }),
            Err(err) => Err(err).with_context(|| format!("stat {rel}")),
        }
    }

    /// Read one marker. `Ok(None)` when no marker exists.
    pub fn read(&self, rel: &str) -> Result<Option<MarkerRead>> {
        let marker = self.marker_path(rel);
        let bytes = match fs::read(&marker) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("read {}", marker.display())),
        };
        Ok(Some(parse_marker(&bytes, rel)))
    }

    /// Acquire every path or none of them.
    ///
    /// Paths are normalized and processed in lexicographic order. A path
    /// already held by the same attempt/session is renewed (its `claimed_at`
    /// and baseline are kept). A path held by anyone else, or carrying a
    /// corrupt marker, is a conflict unless `takeover` approves replacing it.
    /// The decision about staleness belongs to the caller (see `assessment`);
    /// the store only guarantees atomicity.
    pub fn acquire(
        &self,
        participant: &Participant,
        paths: &[PathIntent],
        lease: Duration,
        now: DateTime<Utc>,
        takeover: &dyn Fn(&str, &MarkerRead) -> bool,
    ) -> Result<AcquireOutcome> {
        participant.validate()?;
        if paths.is_empty() {
            bail!("file-contention acquisition requires at least one path");
        }
        let mut wanted: Vec<(String, WriteIntent)> = paths
            .iter()
            .map(|p| Ok((self.normalize(&p.path)?, p.intent)))
            .collect::<Result<_>>()?;
        wanted.sort_by(|a, b| a.0.cmp(&b.0));
        wanted.dedup_by(|a, b| a.0 == b.0);
        let lease = chrono::Duration::from_std(lease).context("lease duration out of range")?;

        self.with_registry(|| {
            let mut planned: Vec<MarkerRecord> = Vec::with_capacity(wanted.len());
            let mut conflicts: Vec<Conflict> = Vec::new();
            for (rel, intent) in &wanted {
                match self.read(rel)? {
                    Some(MarkerRead::Valid(existing)) if participant.owns(&existing) => {
                        let mut renewed = (*existing).clone();
                        renewed.last_renewed_at = now;
                        renewed.lease_expires_at = now + lease;
                        renewed.pid = participant.pid;
                        planned.push(renewed);
                    }
                    Some(existing) => {
                        if takeover(rel, &existing) {
                            planned.push(self.new_record(participant, rel, *intent, now, lease)?);
                        } else {
                            conflicts.push(Conflict {
                                path: rel.clone(),
                                existing,
                            });
                        }
                    }
                    None => planned.push(self.new_record(participant, rel, *intent, now, lease)?),
                }
            }
            if !conflicts.is_empty() {
                return Ok(AcquireOutcome::Conflicts(conflicts));
            }
            for record in &planned {
                self.write_record(record)?;
            }
            Ok(AcquireOutcome::Acquired(planned))
        })
    }

    /// Renew every marker this participant owns (or only `paths`).
    pub fn renew(
        &self,
        participant: &Participant,
        paths: Option<&[PathBuf]>,
        lease: Duration,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>> {
        participant.validate()?;
        let lease = chrono::Duration::from_std(lease).context("lease duration out of range")?;
        let only = self.normalize_filter(paths)?;
        self.with_registry(|| {
            let mut renewed = Vec::new();
            for (rel, read) in self.list()? {
                if only.as_ref().is_some_and(|set| !set.contains(&rel)) {
                    continue;
                }
                if let MarkerRead::Valid(mut record) = read {
                    if participant.owns(&record) {
                        record.last_renewed_at = now;
                        record.lease_expires_at = now + lease;
                        record.pid = participant.pid;
                        self.write_record(&record)?;
                        renewed.push(rel);
                    }
                }
            }
            Ok(renewed)
        })
    }

    /// Remove markers owned by this participant (or only `paths`). Markers
    /// held by anyone else are never touched.
    pub fn release(
        &self,
        participant: &Participant,
        paths: Option<&[PathBuf]>,
    ) -> Result<Vec<String>> {
        participant.validate()?;
        let only = self.normalize_filter(paths)?;
        self.with_registry(|| {
            let mut released = Vec::new();
            for (rel, read) in self.list()? {
                if only.as_ref().is_some_and(|set| !set.contains(&rel)) {
                    continue;
                }
                if let MarkerRead::Valid(record) = read {
                    if participant.owns(&record) {
                        self.remove_marker(&rel)?;
                        released.push(rel);
                    }
                }
            }
            Ok(released)
        })
    }

    /// Remove one marker regardless of owner. Callers must have decided,
    /// under their own policy, that the marker is reclaimable.
    pub fn remove_unconditionally(&self, rel: &str) -> Result<bool> {
        self.with_registry(|| self.remove_marker(rel))
    }

    /// Every marker in the tree, sorted by path. Corrupt markers are listed
    /// as such rather than skipped.
    pub fn list(&self) -> Result<Vec<(String, MarkerRead)>> {
        let mut out = Vec::new();
        if !self.locks_root.exists() {
            return Ok(out);
        }
        let mut stack = vec![self.locks_root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
                let entry = entry?;
                let path = entry.path();
                let file_type = entry.file_type()?;
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                let Ok(rel_marker) = path.strip_prefix(&self.locks_root) else {
                    continue;
                };
                let Some(rel_marker) = rel_marker.to_str() else {
                    continue;
                };
                if rel_marker == REGISTRY_LOCK || !rel_marker.ends_with(MARKER_SUFFIX) {
                    continue;
                }
                let rel = rel_marker.trim_end_matches(MARKER_SUFFIX).to_string();
                let read = match fs::read(&path) {
                    Ok(bytes) => parse_marker(&bytes, &rel),
                    Err(err) => MarkerRead::Corrupt {
                        reason: format!("unreadable marker: {err}"),
                    },
                };
                out.push((rel, read));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    fn new_record(
        &self,
        participant: &Participant,
        rel: &str,
        intent: WriteIntent,
        now: DateTime<Utc>,
        lease: chrono::Duration,
    ) -> Result<MarkerRecord> {
        Ok(MarkerRecord {
            schema_version: SCHEMA_VERSION,
            path: rel.to_string(),
            bead_id: participant.bead_id.clone(),
            attempt_id: participant.attempt_id.clone(),
            session_id: participant.session_id.clone(),
            worker_id: participant.worker_id.clone(),
            host: participant.host.clone(),
            pid: participant.pid,
            intent,
            claimed_at: now,
            last_renewed_at: now,
            lease_expires_at: now + lease,
            baseline: self.baseline(rel)?,
        })
    }

    fn normalize_filter(
        &self,
        paths: Option<&[PathBuf]>,
    ) -> Result<Option<std::collections::BTreeSet<String>>> {
        paths
            .map(|paths| {
                paths
                    .iter()
                    .map(|p| self.normalize(p))
                    .collect::<Result<_>>()
            })
            .transpose()
    }

    fn write_record(&self, record: &MarkerRecord) -> Result<()> {
        let marker = self.marker_path(&record.path);
        let parent = marker.parent().context("marker has no parent directory")?;
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        let tmp = parent.join(format!(
            ".{}.tmp-{}-{}",
            marker
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("marker"),
            std::process::id(),
            unique_suffix()
        ));
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .with_context(|| format!("create {}", tmp.display()))?;
            file.write_all(&serde_json::to_vec_pretty(record)?)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &marker).with_context(|| format!("publish {}", marker.display()))?;
        Ok(())
    }

    fn remove_marker(&self, rel: &str) -> Result<bool> {
        let marker = self.marker_path(rel);
        match fs::remove_file(&marker) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err).with_context(|| format!("remove {}", marker.display())),
        }
        // Prune now-empty mirrored directories, never the locks root itself.
        let mut dir = marker.parent().map(Path::to_path_buf);
        while let Some(current) = dir {
            if current == self.locks_root || !current.starts_with(&self.locks_root) {
                break;
            }
            if fs::remove_dir(&current).is_err() {
                break;
            }
            dir = current.parent().map(Path::to_path_buf);
        }
        Ok(true)
    }

    fn with_registry<T>(&self, body: impl FnOnce() -> Result<T>) -> Result<T> {
        fs::create_dir_all(&self.locks_root)
            .with_context(|| format!("create {}", self.locks_root.display()))?;
        let registry: File = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.locks_root.join(REGISTRY_LOCK))
            .context("open file-contention registry lock")?;
        registry
            .lock_exclusive()
            .context("lock file-contention registry")?;
        let result = body();
        let _ = FileExt::unlock(&registry);
        result
    }
}

fn parse_marker(bytes: &[u8], rel: &str) -> MarkerRead {
    match serde_json::from_slice::<MarkerRecord>(bytes) {
        Ok(record) => match record.consistency_error(rel) {
            None => MarkerRead::Valid(Box::new(record)),
            Some(reason) => MarkerRead::Corrupt { reason },
        },
        Err(err) => MarkerRead::Corrupt {
            reason: format!("unparseable marker: {err}"),
        },
    }
}

/// Canonicalize the deepest existing ancestor of `path`, then re-append the
/// non-existent tail. Lets a create-intent path be checked for escapes.
fn canonicalize_existing_prefix(path: &Path) -> Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(resolved) => {
                let mut out = resolved;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                return Ok(out);
            }
            Err(_) => {
                let Some(name) = existing.file_name().map(|n| n.to_os_string()) else {
                    bail!("cannot resolve any ancestor of {}", path.display());
                };
                tail.push(name);
                if !existing.pop() {
                    bail!("cannot resolve any ancestor of {}", path.display());
                }
            }
        }
    }
}

/// SHA-256 of a file's content, hex-encoded.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn unique_suffix() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    nanos ^ u128::from(COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    const LEASE: Duration = Duration::from_secs(120);

    fn repo() -> (tempfile::TempDir, MarkerStore) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/worker")).unwrap();
        fs::write(dir.path().join("src/worker/mod.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("README.md"), "# fixture\n").unwrap();
        let store = MarkerStore::open(dir.path()).unwrap();
        (dir, store)
    }

    fn worker(attempt: &str) -> Participant {
        Participant {
            bead_id: Some("needle-123".into()),
            attempt_id: Some(attempt.into()),
            session_id: None,
            worker_id: format!("worker-{attempt}"),
            host: "codinghome".into(),
            pid: 4242,
        }
    }

    fn session(id: &str) -> Participant {
        Participant {
            bead_id: None,
            attempt_id: None,
            session_id: Some(id.into()),
            worker_id: "interactive".into(),
            host: "codinghome".into(),
            pid: 77,
        }
    }

    fn modify(path: &str) -> PathIntent {
        PathIntent {
            path: PathBuf::from(path),
            intent: WriteIntent::Modify,
        }
    }

    fn never(_: &str, _: &MarkerRead) -> bool {
        false
    }

    fn now() -> DateTime<Utc> {
        "2026-10-07T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn marker_mirrors_the_source_path_under_needle_locks() {
        let (dir, store) = repo();
        let outcome = store
            .acquire(
                &worker("a1"),
                &[modify("src/worker/mod.rs")],
                LEASE,
                now(),
                &never,
            )
            .unwrap();
        assert!(matches!(outcome, AcquireOutcome::Acquired(ref r) if r.len() == 1));
        assert!(dir
            .path()
            .join(".needle/locks/src/worker/mod.rs.lock")
            .is_file());
        assert_eq!(
            store.marker_path("src/worker/mod.rs"),
            store
                .repo_root()
                .join(".needle/locks/src/worker/mod.rs.lock")
        );
    }

    #[test]
    fn record_is_versioned_and_carries_identity_and_baseline_only() {
        let (dir, store) = repo();
        store
            .acquire(
                &worker("a1"),
                &[modify("src/worker/mod.rs")],
                LEASE,
                now(),
                &never,
            )
            .unwrap();
        let raw =
            fs::read_to_string(dir.path().join(".needle/locks/src/worker/mod.rs.lock")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["path"], "src/worker/mod.rs");
        assert_eq!(value["bead_id"], "needle-123");
        assert_eq!(value["attempt_id"], "a1");
        assert_eq!(value["worker_id"], "worker-a1");
        assert_eq!(value["host"], "codinghome");
        assert_eq!(value["intent"], "modify");
        assert_eq!(value["baseline"]["existed"], true);
        assert_eq!(value["baseline"]["size"], 13);
        assert_eq!(
            value["baseline"]["content_sha256"].as_str().unwrap(),
            sha256_file(&dir.path().join("src/worker/mod.rs")).unwrap()
        );
        assert!(
            !raw.contains("fn main"),
            "marker must not contain source contents"
        );
    }

    #[test]
    fn create_intent_records_a_missing_file_baseline() {
        let (_dir, store) = repo();
        let outcome = store
            .acquire(
                &worker("a1"),
                &[PathIntent {
                    path: PathBuf::from("src/new_module.rs"),
                    intent: WriteIntent::Create,
                }],
                LEASE,
                now(),
                &never,
            )
            .unwrap();
        let AcquireOutcome::Acquired(records) = outcome else {
            panic!("expected acquisition");
        };
        assert!(!records[0].baseline.existed);
        assert_eq!(records[0].baseline.content_sha256, None);
    }

    #[test]
    fn normalization_rejects_escapes_and_reserved_trees() {
        let (dir, store) = repo();
        for bad in [
            "../outside.rs",
            "src/../../outside.rs",
            ".git/config",
            ".needle/locks/x.lock",
            ".needle/config.yaml",
        ] {
            assert!(
                store.normalize(Path::new(bad)).is_err(),
                "{bad} must be rejected"
            );
        }
        assert!(store.normalize(Path::new("/etc/passwd")).is_err());
        assert!(store.normalize(Path::new("")).is_err());
        assert_eq!(
            store.normalize(Path::new("./src/worker/mod.rs")).unwrap(),
            "src/worker/mod.rs"
        );
        let absolute = dir.path().join("src/worker/mod.rs");
        assert_eq!(store.normalize(&absolute).unwrap(), "src/worker/mod.rs");

        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        assert!(
            store.normalize(Path::new("escape/file.rs")).is_err(),
            "symlink escape must be rejected"
        );
    }

    #[test]
    fn identity_requires_attempt_or_session_and_worker() {
        let (_dir, store) = repo();
        let mut anonymous = worker("a1");
        anonymous.attempt_id = None;
        assert!(store
            .acquire(&anonymous, &[modify("README.md")], LEASE, now(), &never)
            .is_err());
        let mut nameless = worker("a1");
        nameless.worker_id = " ".into();
        assert!(store
            .acquire(&nameless, &[modify("README.md")], LEASE, now(), &never)
            .is_err());
        // An interactive session without a bead is a valid participant.
        let outcome = store
            .acquire(
                &session("s-1"),
                &[modify("README.md")],
                LEASE,
                now(),
                &never,
            )
            .unwrap();
        let AcquireOutcome::Acquired(records) = outcome else {
            panic!("expected acquisition");
        };
        assert_eq!(records[0].bead_id, None);
        assert_eq!(records[0].session_id.as_deref(), Some("s-1"));
    }

    #[test]
    fn same_attempt_renews_keeping_claimed_at_and_baseline() {
        let (dir, store) = repo();
        let first = now();
        store
            .acquire(&worker("a1"), &[modify("README.md")], LEASE, first, &never)
            .unwrap();
        fs::write(dir.path().join("README.md"), "# edited by the holder\n").unwrap();
        let later = first + chrono::Duration::seconds(60);
        let AcquireOutcome::Acquired(records) = store
            .acquire(&worker("a1"), &[modify("README.md")], LEASE, later, &never)
            .unwrap()
        else {
            panic!("same attempt must renew");
        };
        assert_eq!(records[0].claimed_at, first, "claimed_at is immutable");
        assert_eq!(records[0].last_renewed_at, later);
        assert_eq!(
            records[0].lease_expires_at,
            later + chrono::Duration::seconds(120)
        );
        assert_eq!(
            records[0].baseline.size,
            Some(10),
            "baseline is the first claim's state"
        );
    }

    #[test]
    fn other_holder_is_a_conflict_and_multi_path_is_all_or_none() {
        let (dir, store) = repo();
        store
            .acquire(
                &worker("a1"),
                &[modify("src/worker/mod.rs")],
                LEASE,
                now(),
                &never,
            )
            .unwrap();
        let outcome = store
            .acquire(
                &worker("b2"),
                &[modify("README.md"), modify("src/worker/mod.rs")],
                LEASE,
                now(),
                &never,
            )
            .unwrap();
        let AcquireOutcome::Conflicts(conflicts) = outcome else {
            panic!("expected a conflict");
        };
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "src/worker/mod.rs");
        assert!(
            matches!(&conflicts[0].existing, MarkerRead::Valid(r) if r.attempt_id.as_deref() == Some("a1"))
        );
        assert!(
            !dir.path().join(".needle/locks/README.md.lock").exists(),
            "a conflicting request must leave no partial set"
        );
    }

    #[test]
    fn takeover_replaces_only_when_the_caller_approves() {
        let (_dir, store) = repo();
        store
            .acquire(&worker("a1"), &[modify("README.md")], LEASE, now(), &never)
            .unwrap();
        let approve = |_: &str, _: &MarkerRead| true;
        let AcquireOutcome::Acquired(records) = store
            .acquire(
                &worker("b2"),
                &[modify("README.md")],
                LEASE,
                now(),
                &approve,
            )
            .unwrap()
        else {
            panic!("approved takeover must acquire");
        };
        assert_eq!(records[0].attempt_id.as_deref(), Some("b2"));
    }

    #[test]
    fn release_removes_only_owned_markers_and_prunes_empty_dirs() {
        let (dir, store) = repo();
        store
            .acquire(
                &worker("a1"),
                &[modify("src/worker/mod.rs")],
                LEASE,
                now(),
                &never,
            )
            .unwrap();
        store
            .acquire(&worker("b2"), &[modify("README.md")], LEASE, now(), &never)
            .unwrap();
        let released = store.release(&worker("a1"), None).unwrap();
        assert_eq!(released, vec!["src/worker/mod.rs".to_string()]);
        assert!(
            !dir.path().join(".needle/locks/src").exists(),
            "empty mirrored dirs are pruned"
        );
        assert!(
            dir.path().join(".needle/locks/README.md.lock").exists(),
            "another holder's marker stays"
        );
        assert!(store
            .release(&worker("a1"), Some(&[PathBuf::from("README.md")]))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn renew_updates_only_owned_markers() {
        let (_dir, store) = repo();
        store
            .acquire(&worker("a1"), &[modify("README.md")], LEASE, now(), &never)
            .unwrap();
        store
            .acquire(
                &worker("b2"),
                &[modify("src/worker/mod.rs")],
                LEASE,
                now(),
                &never,
            )
            .unwrap();
        let later = now() + chrono::Duration::seconds(30);
        assert_eq!(
            store.renew(&worker("a1"), None, LEASE, later).unwrap(),
            vec!["README.md"]
        );
        let Some(MarkerRead::Valid(other)) = store.read("src/worker/mod.rs").unwrap() else {
            panic!("other marker must remain valid");
        };
        assert_eq!(other.last_renewed_at, now());
    }

    #[test]
    fn list_reports_corrupt_and_contradictory_markers() {
        let (dir, store) = repo();
        store
            .acquire(&worker("a1"), &[modify("README.md")], LEASE, now(), &never)
            .unwrap();
        fs::create_dir_all(dir.path().join(".needle/locks/src")).unwrap();
        fs::write(
            dir.path().join(".needle/locks/src/broken.rs.lock"),
            b"{not json",
        )
        .unwrap();
        let mut moved = match store.read("README.md").unwrap() {
            Some(MarkerRead::Valid(r)) => r,
            other => panic!("unexpected {other:?}"),
        };
        moved.path = "somewhere/else.rs".into();
        fs::write(
            dir.path().join(".needle/locks/src/moved.rs.lock"),
            serde_json::to_vec(&moved).unwrap(),
        )
        .unwrap();
        let listed = store.list().unwrap();
        let paths: Vec<_> = listed.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["README.md", "src/broken.rs", "src/moved.rs"]);
        assert!(matches!(listed[0].1, MarkerRead::Valid(_)));
        assert!(matches!(listed[1].1, MarkerRead::Corrupt { .. }));
        assert!(
            matches!(&listed[2].1, MarkerRead::Corrupt { reason } if reason.contains("does not match"))
        );
        // A corrupt marker is a conflict, never silently free.
        let outcome = store
            .acquire(
                &worker("b2"),
                &[modify("src/broken.rs")],
                LEASE,
                now(),
                &never,
            )
            .unwrap();
        assert!(matches!(outcome, AcquireOutcome::Conflicts(_)));
    }

    #[test]
    fn registry_lets_exactly_one_concurrent_claimant_win() {
        let (dir, _store) = repo();
        let root = dir.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let store = MarkerStore::open(&root).unwrap();
                    barrier.wait();
                    store
                        .acquire(
                            &worker(&format!("t{i}")),
                            &[modify("README.md")],
                            LEASE,
                            now(),
                            &never,
                        )
                        .unwrap()
                })
            })
            .collect();
        let winners = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|o| matches!(o, AcquireOutcome::Acquired(_)))
            .count();
        assert_eq!(winners, 1);
    }

    #[test]
    fn separate_checkouts_do_not_see_each_others_markers() {
        let (_a_dir, a) = repo();
        let (_b_dir, b) = repo();
        a.acquire(&worker("a1"), &[modify("README.md")], LEASE, now(), &never)
            .unwrap();
        assert!(b.list().unwrap().is_empty());
        assert!(matches!(
            b.acquire(&worker("b2"), &[modify("README.md")], LEASE, now(), &never)
                .unwrap(),
            AcquireOutcome::Acquired(_)
        ));
    }
}
