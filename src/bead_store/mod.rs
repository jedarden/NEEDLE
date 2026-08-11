//! Abstract bead store interface and `br` CLI implementation.
//!
//! NEEDLE interacts with beads exclusively through the `BeadStore` trait. The
//! default implementation shells out to `br --json`. JSON parsing failures are
//! explicit errors — never silently treated as empty results (v1 bug).
//!
//! The trait is `Send + Sync` because it is called from async worker tasks.
//!
//! Depends on: `types`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;

use crate::types::{Bead, BeadId, ClaimResult};

// ─── Corruption detection ────────────────────────────────────────────────────

/// Known error strings that indicate SQLite database corruption.
const CORRUPTION_MARKERS: &[&str] = &[
    "database disk image is malformed",
    "database or disk is full",
    "attempt to write a readonly database",
    "file is not a database",
];

// ─── Version handshake ───────────────────────────────────────────────────────

/// Known-bad bead-forge versions and their issues.
///
/// Each entry maps a version prefix to a description of known bugs.
const KNOWN_BAD_VERSIONS: &[(&str, &str)] = &[
    (
        "0.2.0",
        "--limit 0 returns empty set (should return all beads)",
    ),
    (
        "0.1.",
        "pre-0.2.0 versions have truncation bugs with default limits",
    ),
];

// ─── ETXTBSY retry helper ─────────────────────────────────────────────────────

/// Retry wrapper for subprocess spawns that handle ETXTBSY (errno 26).
///
/// ETXTBSY ("Text file busy") occurs when the kernel blocks execution of a binary
/// that has a write-mode file descriptor still open. This is a genuine race condition:
///
/// 1. A process writes a binary to disk and closes the file descriptor.
/// 2. The kernel's page cache and filesystem synchronization haven't fully completed.
/// 3. Another process immediately attempts to `exec()` the same binary.
/// 4. The kernel returns ETXTBSY (errno 26) because the file is still marked for write.
///
/// This is most common with:
/// - Freshly-extracted upgrade binaries executed immediately
/// - Test fixtures written and chmod'd immediately before use
/// - Any binary that's written to disk and executed in quick succession
///
/// The race is narrow (typically <100ms) but real. Retrying with a short backoff
/// is the correct fix — the file descriptor is already closed, we're just waiting
/// for the kernel to finish its internal bookkeeping.
///
/// # Parameters
///
/// * `spawn_fn` - An async function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 5)
/// * `backoff_ms` - Backoff delay between retries in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(T)` - The subprocess output on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
///
/// # When to use this helper
///
/// Use this wrapper whenever spawning a subprocess that may have been written to
/// disk immediately before execution:
///
/// - Freshly-installed or updated binaries
/// - Test fixtures created during test setup
/// - Any executable extracted from an archive and immediately run
///
/// Do NOT use this for long-running processes or stable system binaries — the
/// retry overhead is unnecessary and ETXTBSY is unlikely in those cases.
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use needle::bead_store::spawn_with_etxtbsy_retry;
///
/// # async fn example() -> Result<(), std::io::Error> {
/// let binary_path = Path::new("/path/to/binary");
/// let output = spawn_with_etxtbsy_retry(
///     || async {
///         tokio::process::Command::new(binary_path)
///             .arg("--version")
///             .output()
///             .await
///     },
///     5,
///     20,
/// ).await?;
/// # Ok(())
/// # }
/// ```
pub async fn spawn_with_etxtbsy_retry<F, Fut, T>(
    spawn_fn: F,
    max_attempts: u32,
    backoff_ms: u64,
) -> std::io::Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    let mut last_err = None;
    for attempt in 0..max_attempts {
        match spawn_fn().await {
            Ok(output) => return Ok(output),
            Err(e) if e.raw_os_error() == Some(26) && attempt + 1 < max_attempts => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.expect("loop always sets last_err before exhausting MAX_ATTEMPTS"))
}

/// Retry wrapper for `Command::spawn()` calls that handle ETXTBSY (errno 26).
///
/// Specialized version of `spawn_with_etxtbsy_retry` for subprocess spawns that
/// return a `Child` process. Use this when you need to interact with the spawned
/// process (e.g., for timeout handling with `kill_on_drop`).
///
/// # Parameters
///
/// * `spawn_fn` - An async function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 5)
/// * `backoff_ms` - Backoff delay between retries in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(Child)` - The spawned child process on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
pub async fn spawn_with_etxtbsy_retry_child<F, Fut>(
    spawn_fn: F,
    max_attempts: u32,
    backoff_ms: u64,
) -> std::io::Result<tokio::process::Child>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<tokio::process::Child>>,
{
    spawn_with_etxtbsy_retry(spawn_fn, max_attempts, backoff_ms).await
}

/// Retry wrapper for subprocess spawns with exponential backoff for ETXTBSY (errno 26).
///
/// This variant uses **exponential backoff** with jitter, making it more suitable for
/// high-concurrency scenarios where multiple processes might race for the same binary.
/// Exponential backoff prevents thundering herd problems that linear backoff can cause.
///
/// The backoff formula is: `base_ms * 2^attempt + random_jitter`, where:
/// - `base_ms` is the initial delay (default: 20ms)
/// - `attempt` is the current retry number (0-indexed)
/// - `random_jitter` is ±25% of the calculated delay to prevent synchronization
///
/// Example backoff sequence with base_ms=20:
/// - Attempt 1: ~20ms (15-25ms with jitter)
/// - Attempt 2: ~40ms (30-50ms with jitter)
/// - Attempt 3: ~80ms (60-100ms with jitter)
/// - Attempt 4: ~160ms (120-200ms with jitter)
/// - Attempt 5: ~320ms (240-400ms with jitter)
///
/// # Parameters
///
/// * `spawn_fn` - An async function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 10)
/// * `base_ms` - Base backoff delay in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(T)` - The subprocess output on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
///
/// # When to use exponential vs linear backoff
///
/// Use exponential backoff when:
/// - Spawning multiple processes concurrently that might race for the same binary
/// - Running in high-concurrency environments (CI, parallel tests)
/// - Dealing with slow filesystems where the race window might be longer
///
/// Use linear backoff (`spawn_with_etxtbsy_retry`) when:
/// - Spawning a single process in isolation
/// - The race window is known to be very short (<50ms)
/// - Minimal latency is more important than thundering herd prevention
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use needle::bead_store::spawn_with_etxtbsy_retry_exponential;
///
/// # async fn example() -> Result<(), std::io::Error> {
/// let binary_path = Path::new("/path/to/freshly-written-binary");
/// let output = spawn_with_etxtbsy_retry_exponential(
///     || async {
///         tokio::process::Command::new(binary_path)
///             .arg("--version")
///             .output()
///             .await
///     },
///     10,
///     20,
/// ).await?;
/// # Ok(())
/// # }
/// ```
pub async fn spawn_with_etxtbsy_retry_exponential<F, Fut, T>(
    spawn_fn: F,
    max_attempts: u32,
    base_ms: u64,
) -> std::io::Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    use rand::Rng;

    const ETXTBSY_ERRNO: i32 = 26;
    const JITTER_PERCENT: f64 = 0.25; // ±25% jitter

    let mut last_err = None;
    let mut rng = rand::thread_rng();

    for attempt in 0..max_attempts {
        match spawn_fn().await {
            Ok(output) => return Ok(output),
            Err(e) if e.raw_os_error() == Some(ETXTBSY_ERRNO) && attempt + 1 < max_attempts => {
                last_err = Some(e);

                // Calculate exponential backoff: base_ms * 2^attempt
                let exponential_delay = base_ms * (1 << attempt);

                // Add jitter to prevent synchronization
                let jitter_range = (exponential_delay as f64 * JITTER_PERCENT) as u64;
                let jitter = rng.gen_range(0..=jitter_range * 2);
                let delay = exponential_delay
                    .saturating_add(jitter)
                    .saturating_sub(jitter_range);

                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err.expect("loop always sets last_err before exhausting max_attempts"))
}

/// Retry wrapper for `Command::spawn()` with exponential backoff for ETXTBSY (errno 26).
///
/// Specialized version of `spawn_with_etxtbsy_retry_exponential` for subprocess spawns that
/// return a `Child` process. Use this when you need to interact with the spawned process
/// (e.g., for timeout handling with `kill_on_drop`).
///
/// # Parameters
///
/// * `spawn_fn` - An async function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 10)
/// * `base_ms` - Base backoff delay in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(Child)` - The spawned child process on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
pub async fn spawn_with_etxtbsy_retry_exponential_child<F, Fut>(
    spawn_fn: F,
    max_attempts: u32,
    base_ms: u64,
) -> std::io::Result<tokio::process::Child>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<tokio::process::Child>>,
{
    spawn_with_etxtbsy_retry_exponential(spawn_fn, max_attempts, base_ms).await
}

/// Result of a version check.
#[derive(Debug)]
pub enum VersionCheck {
    /// Version is OK or unknown (no known issues).
    Ok,
    /// Known-bad version detected with specific issues.
    KnownBad {
        version: String,
        issues: Vec<String>,
    },
    /// Version check failed (bf not found, parse error, etc.).
    Failed { reason: String },
}

/// Spawn `<br_path> --version>`, retrying briefly on ETXTBSY.
///
/// Wraps `spawn_with_etxtbsy_retry` to handle the specific case of checking
/// bead-forge version. The retry logic handles the race condition where a
/// binary written to disk moments earlier (e.g., during upgrade or test setup)
/// transiently reports "Text file busy" (errno 26).
async fn spawn_version_check(br_path: &Path) -> std::io::Result<std::process::Output> {
    spawn_with_etxtbsy_retry(
        || async {
            tokio::process::Command::new(br_path)
                .arg("--version")
                .output()
                .await
        },
        5,
        20,
    )
    .await
}

/// Check the bead-forge version and detect known-bad versions.
///
/// This function runs `bf --version` and parses the output to detect
/// versions with known bugs. The primary use case is detecting bead-forge
/// 0.2.0, which has a bug where `--limit 0` returns an empty set instead
/// of all beads.
///
/// # Returns
///
/// - `VersionCheck::Ok` if the version is not known to be bad
/// - `VersionCheck::KnownBad` if a known-bad version is detected
/// - `VersionCheck::Failed` if the version check failed
pub async fn check_bead_forge_version(br_path: &Path) -> VersionCheck {
    let timeout = std::time::Duration::from_secs(5);

    let output = match tokio::time::timeout(timeout, spawn_version_check(br_path)).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return VersionCheck::Failed {
                reason: format!("failed to spawn bf --version: {e}"),
            };
        }
        Err(_) => {
            return VersionCheck::Failed {
                reason: "bf --version timed out after 5s".to_string(),
            };
        }
    };

    if !output.status.success() {
        return VersionCheck::Failed {
            reason: format!(
                "bf --version exited with code {}",
                output.status.code().unwrap_or(-1)
            ),
        };
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout.trim().to_string();

    if version.is_empty() {
        return VersionCheck::Failed {
            reason: "bf --version produced no output".to_string(),
        };
    }

    // Extract version number by taking the second word if present
    // This handles formats like "bf 0.2.0" or "br 0.2.0-github"
    let version_number = version.split_whitespace().nth(1).unwrap_or(&version);

    // Check against known-bad version prefixes
    let mut issues = Vec::new();
    for &(bad_prefix, issue) in KNOWN_BAD_VERSIONS {
        if version_number.starts_with(bad_prefix) {
            issues.push(issue.to_string());
        }
    }

    if !issues.is_empty() {
        VersionCheck::KnownBad { version, issues }
    } else {
        VersionCheck::Ok
    }
}

/// Run version handshake and emit telemetry for known-bad versions.
///
/// This is called during worker boot to detect and warn about known-bad
/// bead-forge versions. It emits a WARN-level telemetry event if issues
/// are found.
pub async fn run_version_handshake(br_path: &Path) {
    match check_bead_forge_version(br_path).await {
        VersionCheck::Ok => {
            tracing::debug!("bead-forge version check passed");
        }
        VersionCheck::KnownBad { version, issues } => {
            for issue in &issues {
                tracing::warn!(
                    version = %version,
                    issue = %issue,
                    "bead-forge version has known bugs — explicit limits will be used to work around"
                );
            }
        }
        VersionCheck::Failed { reason } => {
            tracing::warn!(
                reason = %reason,
                "failed to check bead-forge version — cannot detect known-bad versions"
            );
        }
    }
}

// ─── Corruption detection ────────────────────────────────────────────────────

/// Known error strings that indicate SQLite database is locked (transient).
const LOCK_MARKERS: &[&str] = &[
    "database is locked",
    "sqlite error: 5", // SQLITE_BUSY = database is locked
    "sqlite error: 6", // SQLITE_LOCKED = table is locked
];

/// Known error strings that indicate br sync conflicts.
const SYNC_CONFLICT_MARKERS: &[&str] = &["SYNC_CONFLICT", "JSONL is newer", "sync conflict"];

/// Check if an error message indicates SQLite database corruption.
///
/// Returns `true` if the message contains any known corruption marker.
pub fn is_corruption_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    CORRUPTION_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Check if an error message indicates a br sync conflict.
///
/// Returns `true` if the message contains any known sync conflict marker.
pub fn is_sync_conflict(msg: &str) -> bool {
    SYNC_CONFLICT_MARKERS
        .iter()
        .any(|marker| msg.contains(marker))
}

/// Check if an error message indicates SQLite database is locked (transient condition).
///
/// Returns `true` if the message contains any known lock marker.
pub fn is_lock_error(msg: &str) -> bool {
    LOCK_MARKERS.iter().any(|marker| msg.contains(marker))
}

/// Check if a workspace has a valid bead store (i.e., has a `.beads/` directory).
///
/// Returns `true` if the workspace contains a `.beads/` directory, `false` otherwise.
/// This is used by strands to distinguish between "no home store configured" (expected,
/// benign for roam-only workers) and "home store is broken" (unexpected, problem).
///
/// # Arguments
///
/// * `workspace` - Path to the workspace directory to check
///
/// # Examples
///
/// ```no_run
/// use needle::bead_store::has_valid_bead_store;
/// use std::path::PathBuf;
///
/// let workspace = PathBuf::from("/home/coding/myproject");
/// if has_valid_bead_store(&workspace) {
///     println!("Workspace has a valid bead store");
/// } else {
///     println!("Workspace has no bead store - strand should return Skipped");
/// }
/// ```
pub fn has_valid_bead_store(workspace: &Path) -> bool {
    workspace.join(".beads").is_dir()
}

/// Outcome of a database recovery attempt.
#[derive(Debug)]
pub enum RecoveryOutcome {
    /// `br doctor --repair` fixed the issue.
    Repaired(RepairReport),
    /// Full rebuild (rm db + br sync --import) succeeded.
    Rebuilt,
    /// Recovery failed — JSONL itself may be corrupt or missing.
    Failed(anyhow::Error),
}

/// Error returned when SYNC_CONFLICT recovery fails after retry.
///
/// This is a distinct error type so callers can detect when br sync
/// recovery was attempted but the retry still failed. The caller may
/// choose to emit a failure event and continue rather than blocking.
#[derive(Debug, thiserror::Error)]
#[error("SYNC_CONFLICT recovery failed: {reason}")]
pub struct SyncRecoveryError {
    pub reason: String,
}

// ─── Filters ─────────────────────────────────────────────────────────────────

/// Filters applied when listing ready beads.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    /// Only return beads assigned to this actor. `None` = no filter.
    pub assignee: Option<String>,
    /// Exclude beads that have any of these labels.
    pub exclude_labels: Vec<String>,
    /// Exclude beads with these IDs.
    pub exclude_ids: HashSet<BeadId>,
}

// ─── RepairReport ─────────────────────────────────────────────────────────────

/// Summary from `br doctor --repair`.
#[derive(Debug, Default)]
pub struct RepairReport {
    pub warnings: Vec<String>,
    pub fixed: Vec<String>,
}

// ─── NewChild ─────────────────────────────────────────────────────────────────

/// A child bead to create during an atomic split (see [`BeadStore::split_bead`]).
///
/// Borrowed views only — the caller owns the backing strings/labels.
#[derive(Debug, Clone, Copy)]
pub struct NewChild<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub labels: &'a [&'a str],
}

// ─── BeadStore trait ─────────────────────────────────────────────────────────

/// Abstract interface to the bead backend.
#[async_trait]
pub trait BeadStore: Send + Sync {
    /// List all beads with no incomplete blockers (ready to work on).
    ///
    /// Returns an empty `Vec` when the queue is empty — that is not an error.
    /// Returns `Err` if JSON parsing or br invocation fails.
    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>>;

    /// List ALL beads in the workspace (no readiness/filter checks).
    ///
    /// Used by Knot strand for three-state verification — a DIFFERENT code
    /// path from `ready()` to avoid v1's false-positive bug.
    async fn list_all(&self) -> Result<Vec<Bead>>;

    /// Fetch a single bead by ID.
    async fn show(&self, id: &BeadId) -> Result<Bead>;

    /// Attempt to atomically claim a bead (set status=in_progress, assignee=actor).
    ///
    /// Returns a `ClaimResult` describing the outcome:
    /// - `Claimed(bead)` — success, returns the full bead.
    /// - `RaceLost { claimed_by }` — another worker got there first.
    /// - `NotClaimable { reason }` — bead not in a claimable state.
    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult>;

    /// Atomically find and claim the next available bead (server-selected).
    ///
    /// This is the preferred method for multi-worker scenarios as it eliminates
    /// race conditions: the server selects the bead and assigns it in a single
    /// BEGIN IMMEDIATE transaction. Two workers calling this simultaneously will
    /// always receive distinct beads.
    ///
    /// Returns a `ClaimResult` describing the outcome:
    /// - `Claimed(bead)` — success, returns the full bead.
    /// - `NotClaimable { reason }` — no beads available to claim.
    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult>;

    /// Release a claimed bead back to open (e.g., after agent failure).
    async fn release(&self, id: &BeadId) -> Result<()>;

    /// Quarantine a bead by setting status=blocked (e.g., after it exceeds the
    /// consecutive-failure threshold in `OutcomeConfig::quarantine_after_failures`).
    ///
    /// Unlike `release`, this deliberately does NOT clear the assignee or return
    /// the bead to a claimable state — the whole point is to stop Pluck from
    /// re-selecting it until a human (or a future auto-split) intervenes.
    async fn block(&self, id: &BeadId) -> Result<()>;

    /// Clear the assignee on a bead without changing its status.
    ///
    /// Used by mend to heal open beads with stale assignees (e.g., after reopen).
    async fn clear_assignee(&self, id: &BeadId) -> Result<()>;

    /// Flush local bead changes to JSONL before release.
    ///
    /// Runs `br sync --flush-only` to ensure any local writes are persisted
    /// to JSONL before attempting to release a bead. This prevents SYNC_CONFLICT
    /// errors when the JSONL has newer remote changes.
    async fn flush(&self) -> Result<()>;

    /// Reopen a closed (Done) bead back to open status.
    ///
    /// Used by validation gates when verification fails after an agent has
    /// already closed the bead.
    async fn reopen(&self, id: &BeadId) -> Result<()>;

    /// List all labels on a bead.
    async fn labels(&self, id: &BeadId) -> Result<Vec<String>>;

    /// Add a label to a bead.
    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()>;

    /// Remove a label from a bead.
    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()>;

    /// Create a new bead and return its ID.
    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId>;

    /// Add a dependency link: `blocker_id` blocks `blocked_id`.
    ///
    /// Uses `br dep add <blocker_id> --blocks <blocked_id>`.
    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()>;

    /// Atomically create `children` and link each as a blocker of `parent_id`.
    ///
    /// This is the "mitosis" / bead-splitting primitive. N child creations plus
    /// N dependency links must commit together: otherwise a crash between the
    /// `create` and the `dep add` (SIGKILL, OOM, pod eviction) leaves an orphaned
    /// child with no dependency link, and the parent never unblocks — plan.md
    /// Phase 5.3, Race 3.
    ///
    /// The parent is deliberately **not** closed: NEEDLE keeps a split parent
    /// open/blocked while its children are worked.
    ///
    /// The default implementation performs the historical non-atomic sequence
    /// (`create_bead` + `add_dependency`, one child at a time). Mock stores rely
    /// on it, and bf-backed stores fall back to it when the atomic path is
    /// unavailable. Backends that can run a transactional batch (bf) override
    /// this to make the whole split crash-safe.
    async fn split_bead(
        &self,
        parent_id: &BeadId,
        children: &[NewChild<'_>],
    ) -> Result<Vec<BeadId>> {
        let mut created = Vec::with_capacity(children.len());
        for child in children {
            let child_id = self
                .create_bead(child.title, child.body, child.labels)
                .await
                .with_context(|| format!("failed to create child bead: {}", child.title))?;
            self.add_dependency(&child_id, parent_id)
                .await
                .with_context(|| {
                    format!("failed to add dependency: {child_id} blocks {parent_id}")
                })?;
            created.push(child_id);
        }
        Ok(created)
    }

    /// Remove a dependency link: `blocker_id` blocks `blocked_id`.
    ///
    /// Uses `br dep remove <blocked_id> <blocker_id>`.
    async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()>;

    /// Run `br doctor --repair` and return the report.
    async fn doctor_repair(&self) -> Result<RepairReport>;

    /// Run `br doctor` (without `--repair`) to check database health.
    ///
    /// Returns warnings if any issues are detected, without attempting to fix them.
    async fn doctor_check(&self) -> Result<RepairReport>;

    /// Full database rebuild: remove SQLite DB and reimport from JSONL.
    ///
    /// 1. rm .beads/beads.db
    /// 2. br sync --import
    /// 3. Verify: br doctor
    ///
    /// Returns `Err` if rebuild or verification fails (JSONL itself may be corrupt).
    async fn full_rebuild(&self) -> Result<()>;

    /// Check if this store has a valid bead store (i.e., has a `.beads/` directory).
    ///
    /// Returns `true` if the workspace contains a `.beads/` directory, `false` otherwise.
    /// This is used by strands to distinguish between "no home store configured" (expected,
    /// benign for roam-only workers) and "home store is broken" (unexpected, problem).
    fn has_valid_store(&self) -> bool;
}

// ─── BrCliBeadStore ──────────────────────────────────────────────────────────

/// `br` CLI-backed bead store implementation.
///
/// All operations shell out to `br` with `--json` output and parse the result.
/// The workspace directory is set via `BEADS_PATH` / cwd when invoking br.
pub struct BrCliBeadStore {
    /// Path to the `br` binary.
    pub br_path: PathBuf,
    /// Workspace root (directory containing `.beads/`).
    pub workspace: PathBuf,
    /// Model name for velocity-aware claim scoring (e.g., "claude-sonnet-4-6").
    ///
    /// Passed to `bf claim --model` so bead-forge can route beads to the
    /// model/harness combo that closes each issue_type fastest (plan §4B.6).
    /// `None` falls back to the population-wide average.
    pub model: Option<String>,
    /// Harness name for velocity-aware claim scoring (e.g., "needle").
    pub harness: Option<String>,
    /// Harness version for velocity-aware claim scoring.
    pub harness_version: Option<String>,
}

impl BrCliBeadStore {
    /// Construct a new store, validating that the `br` binary exists.
    pub fn new(
        br_path: PathBuf,
        workspace: PathBuf,
        model: Option<String>,
        harness: Option<String>,
        harness_version: Option<String>,
    ) -> Result<Self> {
        if !br_path.exists() {
            bail!("br binary not found at {}", br_path.display());
        }
        Ok(BrCliBeadStore {
            br_path,
            workspace,
            model,
            harness,
            harness_version,
        })
    }

    /// Try to find the bead CLI on PATH or the default install location.
    ///
    /// Resolves `bf` (bead-forge, canonical) first and only falls back to the
    /// deprecated `br` alias for hosts that still carry the shim. Preferring
    /// `br` here is what kept NEEDLE its last consumer, and on a host with no
    /// shim at all it failed outright rather than using the CLI that was
    /// actually installed.
    ///
    /// `model`/`harness`/`harness_version` are threaded into `bf claim` for
    /// velocity-aware scoring (plan §4B.6). Any may be `None` — `bf claim`
    /// treats missing metadata as a documented fallback to the
    /// population-wide average, so partial metadata is safe.
    pub fn discover(
        workspace: PathBuf,
        model: Option<String>,
        harness: Option<String>,
        harness_version: Option<String>,
    ) -> Result<Self> {
        let home = std::env::var("HOME").unwrap_or_default();
        let br_path = which::which("bf")
            .or_else(|_| {
                let candidate = PathBuf::from(format!("{home}/.local/bin/bf"));
                if candidate.exists() {
                    Ok(candidate)
                } else {
                    Err(anyhow!("bf not found on PATH or at ~/.local/bin/bf"))
                }
            })
            .or_else(|_| which::which("br"))
            .or_else(|_| {
                let candidate = PathBuf::from(format!("{home}/.local/bin/br"));
                if candidate.exists() {
                    Ok(candidate)
                } else {
                    Err(anyhow!("bf not found on PATH or at ~/.local/bin/bf"))
                }
            })
            .context("bead CLI not found; install bead-forge (bf)")?;
        Ok(BrCliBeadStore {
            br_path,
            workspace,
            model,
            harness,
            harness_version,
        })
    }

    /// Default timeout for br subprocess calls (30 seconds).
    const DEFAULT_BR_TIMEOUT_SECS: u64 = 30;

    /// Run a `br` subcommand in the workspace directory and return stdout.
    ///
    /// Returns `Err` if the process fails to spawn, exits non-zero (unless
    /// the caller handles specific codes), or stdout is not valid UTF-8.
    async fn run_br(&self, args: &[&str]) -> Result<String> {
        self.run_br_in(&self.workspace, args, Self::DEFAULT_BR_TIMEOUT_SECS)
            .await
    }

    /// Run a `br` subcommand with a custom timeout.
    ///
    /// Use this for calls that may take longer (e.g., sync operations).
    #[allow(dead_code)]
    async fn run_br_with_timeout(&self, args: &[&str], timeout_secs: u64) -> Result<String> {
        self.run_br_in(&self.workspace, args, timeout_secs).await
    }

    async fn run_br_in(&self, dir: &Path, args: &[&str], timeout_secs: u64) -> Result<String> {
        let timeout_duration = std::time::Duration::from_secs(timeout_secs);

        // kill_on_drop ensures the process is killed if the wait_with_output
        // future is dropped (e.g., on timeout), preventing orphaned br processes.
        let br_path = self.br_path.clone();
        let dir_buf = dir.to_path_buf();
        let args_vec = args.to_vec();
        let child = spawn_with_etxtbsy_retry_child(
            || async {
                let mut cmd = tokio::process::Command::new(&br_path);
                cmd.args(&args_vec)
                    .current_dir(&dir_buf)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);
                cmd.spawn()
            },
            5,
            20,
        )
        .await
        .with_context(|| format!("failed to spawn br subprocess: {args:?}"))?;

        // Wait for output with timeout. On timeout, kill_on_drop fires automatically.
        let output = match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(e).context(format!("br subprocess failed: {args:?}"));
            }
            Err(_) => {
                tracing::error!(
                    args = ?args,
                    timeout_secs,
                    "br subprocess timed out, killing process"
                );
                bail!("br subprocess timed out after {timeout_secs}s: {args:?}");
            }
        };

        let stdout = String::from_utf8(output.stdout).context("br stdout was not valid UTF-8")?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);

            // FrankenSQLite crash recovery: if br was killed by a signal
            // (code() returns None) but stdout shows the operation completed
            // and stderr is empty, treat as success. This commonly happens
            // when br's SQLite layer crashes during post-commit cleanup while
            // the mutation was already persisted to the append-only JSONL file.
            if output.status.code().is_none() && stderr.is_empty() && !stdout.is_empty() {
                tracing::warn!(
                    args = ?args,
                    stdout = %stdout.trim(),
                    "br was killed by signal but stdout indicates success — \
                     treating as successful (FrankenSQLite crash recovery)"
                );
                return Ok(stdout);
            }

            // Auto-recover from SYNC_CONFLICT: run `br sync` then retry once.
            if is_sync_conflict(&stderr) {
                tracing::warn!(
                    args = ?args,
                    "br hit SYNC_CONFLICT, running br sync and retrying"
                );

                let sync_timeout = std::time::Duration::from_secs(60);
                let br_path = self.br_path.clone();
                let dir_buf_clone = dir_buf.to_path_buf();
                let sync_child = spawn_with_etxtbsy_retry_child(
                    || async {
                        let mut sync_cmd = tokio::process::Command::new(&br_path);
                        sync_cmd
                            .args(["sync"])
                            .current_dir(&dir_buf_clone)
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .kill_on_drop(true);
                        sync_cmd.spawn()
                    },
                    5,
                    20,
                )
                .await
                .context("failed to spawn br sync during SYNC_CONFLICT recovery")?;

                let sync_output = match tokio::time::timeout(
                    sync_timeout,
                    sync_child.wait_with_output(),
                )
                .await
                {
                    Ok(Ok(output)) => output,
                    Ok(Err(e)) => {
                        return Err(e).context("br sync failed during SYNC_CONFLICT recovery");
                    }
                    Err(_) => {
                        tracing::error!("br sync timed out after 60s during SYNC_CONFLICT recovery, killing process");
                        bail!("br sync timed out after 60s during SYNC_CONFLICT recovery");
                    }
                };

                if !sync_output.status.success() {
                    let sync_stderr = String::from_utf8_lossy(&sync_output.stderr);
                    tracing::warn!(stderr = %sync_stderr, "br sync failed, retrying original command anyway");
                }

                // Retry the original command once with timeout.
                let br_path = self.br_path.clone();
                let dir_buf_clone = dir_buf.to_path_buf();
                let args_vec_clone = args_vec.clone();
                let retry_child = spawn_with_etxtbsy_retry_child(
                    || async {
                        let mut retry_cmd = tokio::process::Command::new(&br_path);
                        retry_cmd
                            .args(&args_vec_clone)
                            .current_dir(&dir_buf_clone)
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .kill_on_drop(true);
                        retry_cmd.spawn()
                    },
                    5,
                    20,
                )
                .await
                .with_context(|| format!("failed to spawn br retry with args: {args:?}"))?;

                let retry =
                    match tokio::time::timeout(timeout_duration, retry_child.wait_with_output())
                        .await
                    {
                        Ok(Ok(output)) => output,
                        Ok(Err(e)) => {
                            return Err(e).context(format!("br retry failed: {args:?}"));
                        }
                        Err(_) => {
                            tracing::error!(
                                args = ?args,
                                timeout_secs,
                                "br retry timed out, killing process"
                            );
                            bail!("br retry subprocess timed out after {timeout_secs}s: {args:?}");
                        }
                    };

                let retry_stdout = String::from_utf8(retry.stdout)
                    .context("br retry stdout was not valid UTF-8")?;
                let retry_stderr = String::from_utf8_lossy(&retry.stderr).into_owned();

                if !retry.status.success() {
                    let retry_code = retry.status.code().unwrap_or(-1);
                    return Err(anyhow::Error::new(SyncRecoveryError {
                        reason: format!(
                            "exit code {retry_code} after br sync retry\n\
                             stderr: {retry_stderr}\nstdout: {retry_stdout}"
                        ),
                    }));
                }

                return Ok(retry_stdout);
            }

            bail!("br {args:?} exited with code {code}\nstderr: {stderr}\nstdout: {stdout}");
        }

        Ok(stdout)
    }

    /// Run br and return both exit code and stdout (for claim race detection).
    ///
    /// Auto-recovers from SYNC_CONFLICT (exit code 6): runs `br sync` then
    /// retries the original command once.
    async fn run_br_with_status(&self, args: &[&str]) -> Result<(i32, String)> {
        let timeout_duration = std::time::Duration::from_secs(Self::DEFAULT_BR_TIMEOUT_SECS);

        // kill_on_drop ensures the process is killed if the wait_with_output
        // future is dropped (e.g., on timeout), preventing orphaned br processes.
        let br_path = self.br_path.clone();
        let workspace = self.workspace.clone();
        let args_vec = args.to_vec();
        let child = spawn_with_etxtbsy_retry_child(
            || async {
                let mut cmd = tokio::process::Command::new(&br_path);
                cmd.args(&args_vec)
                    .current_dir(&workspace)
                    .kill_on_drop(true);
                cmd.spawn()
            },
            5,
            20,
        )
        .await
        .with_context(|| format!("failed to spawn br subprocess: {args:?}"))?;

        let output = match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(e).context(format!("br subprocess failed: {args:?}"));
            }
            Err(_) => {
                tracing::error!(
                    args = ?args,
                    timeout_secs = Self::DEFAULT_BR_TIMEOUT_SECS,
                    "br subprocess timed out, killing process"
                );
                bail!(
                    "br subprocess timed out after {timeout_secs}s: {args:?}",
                    timeout_secs = Self::DEFAULT_BR_TIMEOUT_SECS
                );
            }
        };

        let code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8(output.stdout).context("br stdout was not valid UTF-8")?;
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Auto-recover from SYNC_CONFLICT: run `br sync` then retry once.
        if code != 0 && is_sync_conflict(&stderr) {
            tracing::warn!(
                args = ?args,
                "br hit SYNC_CONFLICT (run_br_with_status), running br sync and retrying"
            );
            let sync_timeout = std::time::Duration::from_secs(60);
            let br_path = self.br_path.clone();
            let workspace = self.workspace.clone();
            let _ = tokio::time::timeout(
                sync_timeout,
                spawn_with_etxtbsy_retry(
                    || async {
                        tokio::process::Command::new(&br_path)
                            .args(["sync"])
                            .current_dir(&workspace)
                            .output()
                            .await
                    },
                    5,
                    20,
                ),
            )
            .await;

            let br_path = self.br_path.clone();
            let workspace = self.workspace.clone();
            let args_vec = args.to_vec();
            let retry = tokio::time::timeout(
                timeout_duration,
                spawn_with_etxtbsy_retry(
                    || async {
                        tokio::process::Command::new(&br_path)
                            .args(&args_vec)
                            .current_dir(&workspace)
                            .output()
                            .await
                    },
                    5,
                    20,
                ),
            )
            .await
            .with_context(|| {
                format!(
                    "br retry subprocess timed out after {timeout_secs}s: {args:?}",
                    timeout_secs = Self::DEFAULT_BR_TIMEOUT_SECS
                )
            })?
            .with_context(|| format!("failed to spawn br retry with args: {args:?}"))?;

            let retry_code = retry.status.code().unwrap_or(-1);
            let retry_stdout =
                String::from_utf8(retry.stdout).context("br retry stdout was not valid UTF-8")?;
            return Ok((retry_code, retry_stdout));
        }

        Ok((code, stdout))
    }

    /// Parse a JSON array or JSONL stream of beads from br output.
    fn parse_beads(json: &str, context: &str) -> Result<Vec<Bead>> {
        if json.trim().is_empty() {
            return Ok(vec![]);
        }
        // Try JSON array first, then fall back to JSONL (one object per line)
        if let Ok(beads) = serde_json::from_str::<Vec<Bead>>(json) {
            return Ok(beads);
        }
        json.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                serde_json::from_str::<Bead>(line)
                    .with_context(|| format!("JSON parse error from {context}:\n{line}"))
            })
            .collect()
    }

    /// Parse a single bead from a JSON array (first element).
    fn parse_single_bead(json: &str, context: &str) -> Result<Bead> {
        let beads = Self::parse_beads(json, context)?;
        beads
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("{context} returned empty array"))
    }

    /// Run `bf claim` for atomic bead selection and claiming.
    ///
    /// This uses bead-forge's atomic claim which performs scoring
    /// (downstream_impact + critical_float + priority + created_at) and
    /// the UPDATE in a single BEGIN IMMEDIATE transaction.
    ///
    /// The worker's `--model`/`--harness`/`--harness-version` are folded into
    /// the claim so bead-forge can record a `worker_sessions`/`velocity_stats`
    /// row and compute a velocity_adjusted_score (plan §4B.6) — routing beads
    /// to the model/harness combo that closes each issue_type fastest. The
    /// flags are emitted before `--assignee`/`--json`; any that are `None` are
    /// omitted, and `bf claim` falls back to the population-wide average.
    /// Locate the `bf` binary on PATH, falling back to the default install
    /// location (`~/.local/bin/bf`).
    fn resolve_bf(&self) -> Result<PathBuf> {
        which::which("bf").or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            let candidate = PathBuf::from(format!("{home}/.local/bin/bf"));
            if candidate.exists() {
                Ok(candidate)
            } else {
                Err(anyhow!("bf not found on PATH or at ~/.local/bin/bf"))
            }
        })
    }

    /// Run `bf batch --json <ops>` and return stdout.
    ///
    /// The entire op array executes inside a single SQLite `BEGIN IMMEDIATE`
    /// transaction (bf `execute_batch`), so a crash or a failing op rolls the
    /// whole batch back. Used by [`BeadStore::split_bead`] for crash-safe mitosis.
    async fn run_bf_batch(&self, ops_json: &str) -> Result<String> {
        let timeout_duration = std::time::Duration::from_secs(30);
        let bf_path = self
            .resolve_bf()
            .map_err(|e| e.context("bf CLI not found; cannot run atomic batch"))?;

        let args = ["batch", "--json", ops_json];
        let bf_path_clone = bf_path.clone();
        let workspace = self.workspace.clone();
        let args = args.to_vec();
        let child = spawn_with_etxtbsy_retry_child(
            || async {
                let mut cmd = tokio::process::Command::new(&bf_path_clone);
                cmd.args(&args)
                    .current_dir(&workspace)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);
                cmd.spawn()
            },
            5,
            20,
        )
        .await
        .context("failed to spawn bf batch subprocess")?;

        let output = match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(e).context("bf batch subprocess failed"),
            Err(_) => {
                tracing::error!("bf batch subprocess timed out, killing process");
                bail!("bf batch subprocess timed out after 30s");
            }
        };

        let stdout =
            String::from_utf8(output.stdout).context("bf batch stdout was not valid UTF-8")?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("bf batch exited with code {code}\nstderr: {stderr}");
        }

        Ok(stdout)
    }

    async fn run_bf_claim(&self, actor: &str) -> Result<String> {
        let timeout_duration = std::time::Duration::from_secs(30);

        // Try to find bf on PATH or at the default install location
        let bf_path = match self.resolve_bf() {
            Ok(p) => p,
            Err(e) => {
                return Err(e.context("bf CLI not found; falling back to br-style claim"));
            }
        };

        // Build the claim args. Velocity-aware scoring metadata is passed
        // BEFORE --assignee/--json; missing values are simply omitted.
        let mut args: Vec<&str> = Vec::with_capacity(10);
        args.push("claim");
        if let Some(model) = &self.model {
            args.push("--model");
            args.push(model.as_str());
        }
        if let Some(harness) = &self.harness {
            args.push("--harness");
            args.push(harness.as_str());
        }
        if let Some(harness_version) = &self.harness_version {
            args.push("--harness-version");
            args.push(harness_version.as_str());
        }
        args.push("--assignee");
        args.push(actor);
        args.push("--json");

        let bf_path_clone = bf_path.clone();
        let workspace = self.workspace.clone();
        let args_clone = args.clone();
        let child = spawn_with_etxtbsy_retry_child(
            || async {
                let mut cmd = tokio::process::Command::new(&bf_path_clone);
                cmd.args(&args_clone)
                    .current_dir(&workspace)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);
                cmd.spawn()
            },
            5,
            20,
        )
        .await
        .with_context(|| format!("failed to spawn bf subprocess: {:?}", args))?;

        let output = match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(e).context(format!("bf subprocess failed: {:?}", args));
            }
            Err(_) => {
                tracing::error!(
                    args = ?args,
                    "bf subprocess timed out, killing process"
                );
                bail!("bf subprocess timed out after 30s: {:?}", args);
            }
        };

        let stdout = String::from_utf8(output.stdout).context("bf stdout was not valid UTF-8")?;
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            bail!(
                "bf {:?} exited with code {}\nstderr: {}",
                args,
                code,
                stderr
            );
        }

        Ok(stdout)
    }

    /// Build the `bf claim` subprocess arguments for testing.
    ///
    /// This is a test helper that returns the exact arguments that would be passed
    /// to the `bf claim` subprocess, including metadata flags when available.
    /// Used by tests to verify that --model/--harness/--harness-version flags are
    /// properly included when metadata is set.
    #[cfg(test)]
    pub fn build_claim_args(&self, actor: &str) -> Vec<String> {
        let mut args: Vec<String> = Vec::with_capacity(10);
        args.push("claim".to_string());
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(harness) = &self.harness {
            args.push("--harness".to_string());
            args.push(harness.clone());
        }
        if let Some(harness_version) = &self.harness_version {
            args.push("--harness-version".to_string());
            args.push(harness_version.clone());
        }
        args.push("--assignee".to_string());
        args.push(actor.to_string());
        args.push("--json".to_string());
        args
    }
}

/// Build the `bf batch` op array for an atomic split: create every child, then
/// link each freshly-created child as a blocker of `parent_id`.
///
/// Creates come first so the `dep_add_blocker` ops can reference the new
/// children by positional placeholder (`@0`, `@1`, …), which bf resolves to the
/// created IDs in creation order. `dep_add_blocker.id` is the *blocked* bead
/// (the parent) and `.blocker` is the child — matching NEEDLE's
/// `add_dependency(child, parent)` semantics (child blocks parent). No `close`
/// op is emitted: a split parent stays open/blocked.
fn build_split_batch_ops(parent_id: &BeadId, children: &[NewChild<'_>]) -> Vec<serde_json::Value> {
    let mut ops = Vec::with_capacity(children.len() * 2);
    for child in children {
        ops.push(serde_json::json!({
            "op": "create",
            "title": child.title,
            "description": child.body,
            "labels": child.labels,
        }));
    }
    let parent = parent_id.as_ref();
    for idx in 0..children.len() {
        ops.push(serde_json::json!({
            "op": "dep_add_blocker",
            "id": parent,
            "blocker": format!("@{idx}"),
        }));
    }
    ops
}

/// Parse the child IDs created by `bf batch` from its stdout.
///
/// `bf batch` prints one line per op: `"[op N] ok: <id>"` for `create` ops and
/// `"[op N] ok"` (no id) for `dep_add_blocker`/`close`. Only creates carry an
/// id, so the ids returned here — in op order — are exactly the new children.
fn parse_batch_created_ids(stdout: &str) -> Vec<BeadId> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("[op ")?;
            let (_n, tail) = rest.split_once(']')?;
            let id = tail.trim().strip_prefix("ok:")?.trim();
            if id.is_empty() {
                None
            } else {
                Some(BeadId::from(id))
            }
        })
        .collect()
}

#[async_trait]
impl BeadStore for BrCliBeadStore {
    async fn list_all(&self) -> Result<Vec<Bead>> {
        // Use a large explicit limit instead of --limit 0, which returns
        // an empty set on bead-forge 0.2.0 (bug). 999999 effectively means "no limit".
        let stdout = self
            .run_br(&["list", "--json", "--limit", "999999"])
            .await?;
        Self::parse_beads(&stdout, "br list --json")
    }

    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
        // Always pass an explicit large limit to avoid default truncation that
        // hides low-priority beads in busy stores, and to avoid the --limit 0
        // bug in bead-forge 0.2.0 (which returns an empty set).
        let mut args = vec!["ready", "--json", "--limit", "10000"];

        // Build filter args — stored so they live long enough for the slice.
        let assignee_arg;
        if let Some(ref assignee) = filters.assignee {
            args.push("--assignee");
            assignee_arg = assignee.clone();
            args.push(&assignee_arg);
        }

        let stdout = self.run_br(&args).await?;
        let mut beads = Self::parse_beads(&stdout, "br ready --json")?;

        // Apply label exclusion filter (br CLI doesn't support this natively).
        if !filters.exclude_labels.is_empty() {
            beads.retain(|b| !b.labels.iter().any(|l| filters.exclude_labels.contains(l)));
        }

        // Apply ID exclusion filter (in-memory filter).
        if !filters.exclude_ids.is_empty() {
            beads.retain(|b| !filters.exclude_ids.contains(&b.id));
        }

        Ok(beads)
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        let id_str = id.as_ref();
        let stdout = self
            .run_br(&["show", id_str, "--json"])
            .await
            .with_context(|| format!("br show {id_str} failed"))?;
        Self::parse_single_bead(&stdout, &format!("br show {id_str} --json"))
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        let id_str = id.as_ref();

        // CRITICAL: Verify the bead is actually claimable BEFORE attempting to claim.
        // This prevents duplicate dispatches where two workers race to claim the same
        // bead. Without this check, the second worker can overwrite the first's claim.
        // See bead bf-1ne6u for details.
        let bead_before = self.show(id).await?;
        if bead_before.status != crate::types::BeadStatus::Open {
            // Bead is already in progress - another worker won this race
            let claimed_by = bead_before
                .assignee
                .clone()
                .unwrap_or_else(|| "(unknown)".to_string());
            return Ok(ClaimResult::RaceLost { claimed_by });
        }
        if let Some(claimed_by) = bead_before.assignee {
            // Bead has a stale assignee - not claimable
            return Ok(ClaimResult::RaceLost { claimed_by });
        }

        // Attempt claim by setting status=in_progress and assignee.
        //
        // Routed through `bf batch` (op=update) rather than `bf update ...
        // --assignee`: bf 0.4.1 dropped --assignee from the `update`
        // subcommand entirely (bf-1hmey), but `batch`'s update op still
        // accepts id/status/assignee together.
        let batch_json = serde_json::to_string(&serde_json::json!([{
            "op": "update",
            "id": id_str,
            "status": "in_progress",
            "assignee": actor,
        }]))
        .context("failed to serialize claim batch payload")?;
        let (code, _stdout) = self
            .run_br_with_status(&["batch", "--json", &batch_json])
            .await
            .with_context(|| format!("br batch update {id_str} (claim) failed to spawn"))?;

        match code {
            0 => {
                // Verify we actually won by reading back the bead.
                let bead = self.show(id).await?;
                // Verify BOTH status and assignee to catch races
                if bead.status == crate::types::BeadStatus::InProgress
                    && bead.assignee.as_deref() == Some(actor)
                {
                    Ok(ClaimResult::Claimed(bead))
                } else if bead.assignee.as_deref() == Some(actor) {
                    // Assignee matches but status is wrong - still treat as claimed
                    // (this handles edge cases where status didn't update but assignee did)
                    Ok(ClaimResult::Claimed(bead))
                } else {
                    let claimed_by = bead
                        .assignee
                        .clone()
                        .unwrap_or_else(|| "(unknown)".to_string());
                    Ok(ClaimResult::RaceLost { claimed_by })
                }
            }
            4 => {
                // br exit code 4 signals a conflict / optimistic lock failure.
                let bead = self.show(id).await.ok();
                let claimed_by = bead
                    .and_then(|b| b.assignee)
                    .unwrap_or_else(|| "(unknown)".to_string());
                Ok(ClaimResult::RaceLost { claimed_by })
            }
            _ => Ok(ClaimResult::ClaimError {
                reason: format!("br batch update exited with code {code}"),
            }),
        }
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        // See claim() above: --assignee no longer exists on `bf update` (bf-1hmey).
        let batch_json = serde_json::to_string(&serde_json::json!([{
            "op": "update",
            "id": id_str,
            "status": "open",
            "assignee": "",
        }]))
        .context("failed to serialize release batch payload")?;
        self.run_br(&["batch", "--json", &batch_json])
            .await
            .with_context(|| format!("br batch release {id_str} failed"))?;
        Ok(())
    }

    async fn block(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_br(&["update", id_str, "--status", "blocked"])
            .await
            .with_context(|| format!("br block {id_str} failed"))?;
        Ok(())
    }

    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        // See claim() above: --assignee no longer exists on `bf update` (bf-1hmey).
        let batch_json = serde_json::to_string(&serde_json::json!([{
            "op": "update",
            "id": id_str,
            "assignee": "",
        }]))
        .context("failed to serialize clear_assignee batch payload")?;
        self.run_br(&["batch", "--json", &batch_json])
            .await
            .with_context(|| format!("br batch clear_assignee {id_str} failed"))?;
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        self.run_br(&["sync", "--flush-only"]).await?;
        Ok(())
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_br(&["reopen", id_str])
            .await
            .with_context(|| format!("br reopen {id_str} failed"))?;
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        // Read labels from br show --json since br doesn't have a label list subcommand.
        // Note: v1 omitted labels here; this bead requires explicit label fetching.
        let bead = self.show(id).await?;
        Ok(bead.labels)
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_br(&["label", "add", id_str, "--label", label])
            .await
            .with_context(|| format!("br label add {id_str} {label} failed"))?;
        Ok(())
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_br(&["label", "remove", id_str, "--label", label])
            .await
            .with_context(|| format!("br label remove {id_str} {label} failed"))?;
        Ok(())
    }

    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        let mut args: Vec<String> = vec![
            "create".into(),
            "--title".into(),
            title.into(),
            "--description".into(),
            body.into(),
        ];
        for label in labels {
            args.push("--label".into());
            args.push((*label).into());
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let stdout = self.run_br(&arg_refs).await?;
        let id_str = stdout.trim();
        if id_str.is_empty() {
            bail!("br create returned empty ID");
        }
        Ok(BeadId::from(id_str))
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        // blocker_id blocks blocked_id (child blocks parent)
        // bf dep add <BLOCKER> --blocks <BLOCKED>
        // BLOCKED depends on BLOCKER, so blocked_id depends on blocker_id
        let blocker = blocker_id.as_ref();
        let blocked = blocked_id.as_ref();
        self.run_br(&["dep", "add", blocker, "--blocks", blocked])
            .await
            .with_context(|| format!("br dep add {blocker} --blocks {blocked} failed"))?;
        Ok(())
    }

    /// Crash-safe bead split via a single `bf batch` transaction.
    ///
    /// Creates every child, then links each as a blocker of `parent_id`, all
    /// inside one `BEGIN IMMEDIATE` transaction. A kill/OOM/eviction mid-split
    /// rolls the whole batch back — no orphaned children (plan.md Phase 5.3,
    /// Race 3). If `bf` is missing or the batch fails, we log and fall back to
    /// the historical non-atomic sequence, mirroring `run_bf_claim`'s degrade-
    /// gracefully behavior so this never becomes a hard dependency.
    async fn split_bead(
        &self,
        parent_id: &BeadId,
        children: &[NewChild<'_>],
    ) -> Result<Vec<BeadId>> {
        if children.is_empty() {
            return Ok(Vec::new());
        }

        // Build one atomic batch: N creates, then N dep_add_blocker ops linking
        // each freshly-created child (@0..@N-1) as a blocker of the parent. No
        // `close` op — a split parent stays open/blocked.
        let ops = build_split_batch_ops(parent_id, children);
        match serde_json::to_string(&ops) {
            Ok(ops_json) => match self.run_bf_batch(&ops_json).await {
                Ok(stdout) => {
                    // The batch committed atomically (bf exited 0). Trust it and
                    // return — we must NOT fall back here, or a parse hiccup
                    // would double-create the children that already exist.
                    let ids = parse_batch_created_ids(&stdout);
                    if ids.len() != children.len() {
                        tracing::warn!(
                            parent_id = %parent_id,
                            expected = children.len(),
                            parsed = ids.len(),
                            stdout = %stdout,
                            "bf batch mitosis committed but the child-id parse \
                             count mismatched; returning parsed ids as-is"
                        );
                    }
                    return Ok(ids);
                }
                Err(e) => {
                    // A non-zero exit / timeout / spawn failure means the batch
                    // rolled back (nothing was created), so retrying the
                    // sequential path is safe.
                    tracing::warn!(
                        parent_id = %parent_id,
                        error = %e,
                        "bf batch mitosis failed; falling back to sequential create+dep"
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    parent_id = %parent_id,
                    error = %e,
                    "failed to serialize bf batch ops; falling back to sequential create+dep"
                );
            }
        }

        // Fallback: historical non-atomic sequence (same as the trait default).
        let mut created = Vec::with_capacity(children.len());
        for child in children {
            let child_id = self
                .create_bead(child.title, child.body, child.labels)
                .await
                .with_context(|| format!("failed to create child bead: {}", child.title))?;
            self.add_dependency(&child_id, parent_id)
                .await
                .with_context(|| {
                    format!("failed to add dependency: {child_id} blocks {parent_id}")
                })?;
            created.push(child_id);
        }
        Ok(created)
    }

    async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()> {
        // Remove the dependency: blocked_id depends on blocker_id
        // br dep remove <ISSUE> <DEPENDENCY>
        let blocked = blocked_id.as_ref();
        let blocker = blocker_id.as_ref();
        self.run_br(&["dep", "remove", blocked, blocker])
            .await
            .with_context(|| format!("br dep remove {blocked} {blocker} failed"))?;
        Ok(())
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        let stdout = self
            .run_br(&["doctor", "--repair"])
            .await
            .context("br doctor --repair failed")?;
        Ok(Self::parse_doctor_output(&stdout))
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        let stdout = self.run_br(&["doctor"]).await.context("br doctor failed")?;
        Ok(Self::parse_doctor_output(&stdout))
    }

    async fn full_rebuild(&self) -> Result<()> {
        let db_path = self.workspace.join(".beads/beads.db");

        // Step 1: Remove the corrupt SQLite database.
        if db_path.exists() {
            tokio::fs::remove_file(&db_path)
                .await
                .with_context(|| format!("failed to remove {}", db_path.display()))?;
            tracing::info!(path = %db_path.display(), "removed corrupt database file");
        }

        // Also remove WAL and SHM files if present.
        for suffix in &["-wal", "-shm"] {
            let wal_path = self.workspace.join(format!(".beads/beads.db{suffix}"));
            if wal_path.exists() {
                let _ = tokio::fs::remove_file(&wal_path).await;
            }
        }

        // Step 2: Reimport from JSONL.
        self.run_br(&["sync", "--import-only"])
            .await
            .context("br sync --import-only failed during full rebuild")?;

        // Step 3: Verify with br doctor.
        let verify = self
            .run_br(&["doctor"])
            .await
            .context("br doctor verification failed after rebuild")?;
        let report = Self::parse_doctor_output(&verify);

        if !report.warnings.is_empty() {
            bail!(
                "database still has issues after rebuild: {:?}",
                report.warnings
            );
        }

        tracing::info!("database fully rebuilt from JSONL — verified clean");
        Ok(())
    }

    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
        // Use bf claim's atomic select-score-update to eliminate TOCTOU race.
        // bf claim performs scoring (downstream_impact + critical_float + priority)
        // and the UPDATE in a single BEGIN IMMEDIATE transaction, guaranteeing
        // that concurrent workers receive distinct beads.
        match self.run_bf_claim(actor).await {
            Ok(stdout) => {
                // bf claim returns JSON with bead_id or empty object for no candidates
                let trimmed = stdout.trim();
                if trimmed.is_empty() || trimmed == "{}" || trimmed == "null" {
                    return Ok(ClaimResult::NotClaimable {
                        reason: "no beads available".to_string(),
                    });
                }

                // Parse the JSON response from bf claim
                #[derive(serde::Deserialize)]
                struct BfClaimResponse {
                    bead_id: Option<String>,
                    #[allow(dead_code)]
                    assignee: Option<String>,
                }

                let response: BfClaimResponse = serde_json::from_str(trimmed)
                    .with_context(|| format!("bf claim returned invalid JSON: {}", trimmed))?;

                if let Some(bead_id) = response.bead_id {
                    // Fetch the full bead details
                    self.show(&BeadId::from(bead_id))
                        .await
                        .map(ClaimResult::Claimed)
                } else {
                    Ok(ClaimResult::NotClaimable {
                        reason: "no beads available".to_string(),
                    })
                }
            }
            Err(e) => {
                // If bf is not available, fall back to the old br-style pattern
                tracing::warn!(error = %e, "bf claim failed, falling back to br-style ready+claim");
                let filters = Filters::default();
                let mut candidates = self.ready(&filters).await?;
                // Filter to only Open beads with no assignee - prevents claiming in_progress beads
                candidates
                    .retain(|b| b.status == crate::types::BeadStatus::Open && b.assignee.is_none());
                if let Some(bead) = candidates.first() {
                    self.claim(&bead.id, actor).await
                } else {
                    Ok(ClaimResult::NotClaimable {
                        reason: "no beads available".to_string(),
                    })
                }
            }
        }
    }

    fn has_valid_store(&self) -> bool {
        has_valid_bead_store(&self.workspace)
    }
}

impl BrCliBeadStore {
    /// Parse `br doctor` output into a `RepairReport`.
    fn parse_doctor_output(stdout: &str) -> RepairReport {
        let mut report = RepairReport::default();
        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix("WARN ") {
                // Filter out non-actionable warnings that cannot be repaired
                // (e.g., sqlite3 binary not installed on the system, or
                // leftover recovery backup files from a prior repair/rebuild).
                if rest.contains("sqlite3 not available") || rest.contains("recovery_artifacts") {
                    continue;
                }
                report.warnings.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("FIXED ") {
                report.fixed.push(rest.to_string());
            }
        }
        report
    }

    /// Attempt database recovery: try repair first, then full rebuild.
    ///
    /// Returns the outcome of the recovery attempt. This is the primary
    /// entry point for auto-recovery from SQLite corruption.
    pub async fn recover_db(&self) -> RecoveryOutcome {
        // Step 1: Try br doctor --repair.
        tracing::warn!("attempting database recovery via br doctor --repair");
        match self.doctor_repair().await {
            Ok(report) => {
                tracing::info!(
                    warnings = report.warnings.len(),
                    fixed = report.fixed.len(),
                    "br doctor --repair completed"
                );
                return RecoveryOutcome::Repaired(report);
            }
            Err(e) => {
                tracing::warn!(error = %e, "br doctor --repair failed, attempting full rebuild");
            }
        }

        // Step 2: Full rebuild — rm db + br sync --import + verify.
        match self.full_rebuild().await {
            Ok(()) => RecoveryOutcome::Rebuilt,
            Err(e) => {
                tracing::error!(error = %e, "full database rebuild failed — JSONL may be corrupt");
                RecoveryOutcome::Failed(e)
            }
        }
    }
}

// ─── BfCliBeadStore ─────────────────────────────────────────────────────────────

/// `bf` CLI-backed bead store implementation.
///
/// Uses `bf claim` for atomic server-selected bead claiming. This eliminates
/// the race condition in `BrCliBeadStore.claim()` where two workers could both
/// see the same bead in `ready()` and race to claim it.
///
/// The key difference: `bf claim` atomically selects AND claims a bead in a
/// single BEGIN IMMEDIATE transaction, guaranteeing that concurrent workers
/// receive distinct beads.
pub struct BfCliBeadStore {
    /// Path to the `bf` binary.
    pub bf_path: PathBuf,
    /// Workspace root (directory containing `.beads/`).
    pub workspace: PathBuf,
    /// Model name for telemetry (e.g., "claude-opus-4-7").
    pub model: Option<String>,
    /// Harness name for telemetry (e.g., "needle").
    pub harness: Option<String>,
    /// Harness version for telemetry.
    pub harness_version: Option<String>,
}

impl BfCliBeadStore {
    /// Construct a new store, validating that the `bf` binary exists.
    pub fn new(
        bf_path: PathBuf,
        workspace: PathBuf,
        model: Option<String>,
        harness: Option<String>,
        harness_version: Option<String>,
    ) -> Result<Self> {
        if !bf_path.exists() {
            bail!("bf binary not found at {}", bf_path.display());
        }
        Ok(BfCliBeadStore {
            bf_path,
            workspace,
            model,
            harness,
            harness_version,
        })
    }

    /// Try to find `bf` on PATH or the default install location.
    pub fn discover(
        workspace: PathBuf,
        model: Option<String>,
        harness: Option<String>,
        harness_version: Option<String>,
    ) -> Result<Self> {
        let bf_path = which::which("bf")
            .or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                let candidate = PathBuf::from(format!("{home}/.local/bin/bf"));
                if candidate.exists() {
                    Ok(candidate)
                } else {
                    Err(anyhow!("bf not found on PATH or at ~/.local/bin/bf"))
                }
            })
            .context("bf CLI not found; install bead-forge")?;
        Ok(BfCliBeadStore {
            bf_path,
            workspace,
            model,
            harness,
            harness_version,
        })
    }

    /// Default timeout for bf subprocess calls (30 seconds).
    const DEFAULT_BF_TIMEOUT_SECS: u64 = 30;

    /// Run a `bf` subcommand in the workspace directory and return stdout.
    async fn run_bf(&self, args: &[&str]) -> Result<String> {
        self.run_bf_in(&self.workspace, args, Self::DEFAULT_BF_TIMEOUT_SECS)
            .await
    }

    async fn run_bf_in(&self, dir: &Path, args: &[&str], timeout_secs: u64) -> Result<String> {
        const MAX_RETRIES: u32 = 5;
        const BASE_DELAY_MS: u64 = 50;

        let mut attempt = 0;

        loop {
            attempt += 1;
            let timeout_duration = std::time::Duration::from_secs(timeout_secs);

            let bf_path = self.bf_path.clone();
            let dir = dir.to_path_buf();
            let args = args.to_vec();
            let child = spawn_with_etxtbsy_retry_child(
                || async {
                    let mut cmd = tokio::process::Command::new(&bf_path);
                    cmd.args(&args)
                        .current_dir(&dir)
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .kill_on_drop(true);
                    cmd.spawn()
                },
                5,
                20,
            )
            .await
            .with_context(|| format!("failed to spawn bf subprocess: {args:?}"))?;

            let output =
                match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
                    Ok(Ok(output)) => output,
                    Ok(Err(e)) => {
                        // For subprocess spawn errors, don't retry - these are not transient
                        tracing::error!(
                            args = ?args,
                            attempt,
                            error = %e,
                            "bf subprocess spawn failed, not retrying"
                        );
                        break;
                    }
                    Err(_) => {
                        // Timeouts are not transient lock errors - don't retry
                        tracing::error!(
                            args = ?args,
                            timeout_secs,
                            attempt,
                            "bf subprocess timed out, not retrying"
                        );
                        break;
                    }
                };

            let stdout =
                String::from_utf8(output.stdout).context("bf stdout was not valid UTF-8")?;
            let stderr = String::from_utf8_lossy(&output.stderr);

            if output.status.success() {
                return Ok(stdout);
            }

            let code = output.status.code().unwrap_or(-1);
            let stderr_trimmed = stderr.trim().to_string();

            // Check if this is a transient lock error that should be retried
            let is_lock_error = is_lock_error(&stderr_trimmed);

            if !is_lock_error || attempt >= MAX_RETRIES {
                // Either not a lock error, or we've exhausted retries
                tracing::error!(
                    args = ?args,
                    exit_code = code,
                    attempt,
                    max_retries = MAX_RETRIES,
                    is_lock_error,
                    bf_stderr = %stderr_trimmed,
                    stdout_preview = %stdout.chars().take(200).collect::<String>(),
                    "bf subprocess failed - stderr captured"
                );

                let base_error = anyhow::anyhow!("bf {args:?} exited with code {code}");
                let error_with_stderr = if stderr_trimmed.is_empty() {
                    base_error
                } else {
                    base_error.context(format!("bf stderr: {}", stderr_trimmed))
                };
                return Err(error_with_stderr);
            }

            // This is a lock error and we have retries remaining
            tracing::warn!(
                args = ?args,
                attempt,
                max_retries = MAX_RETRIES,
                exit_code = code,
                bf_stderr = %stderr_trimmed,
                "bf subprocess failed with lock error, retrying with exponential backoff"
            );

            // Calculate exponential backoff delay: BASE_DELAY_MS * 2^(attempt-1)
            let delay_ms = BASE_DELAY_MS * (1 << (attempt - 1));
            let delay = std::time::Duration::from_millis(delay_ms);

            tokio::time::sleep(delay).await;
        }

        // If we broke out of the loop, return an appropriate error
        Err(anyhow::anyhow!(
            "bf subprocess failed after {} attempts",
            attempt
        ))
    }

    /// Parse a JSON array of beads from bf output.
    /// Handles both JSON array format `[{...},{...}]` and NDJSON (one object per line).
    ///
    /// A single unparseable NDJSON line (e.g. a bead carrying a status value
    /// this build of NEEDLE doesn't yet recognize) is logged loudly and
    /// skipped rather than failing the entire list — one bad record used to
    /// take down `list_all()` for every workspace it appeared in, which broke
    /// Weave/Mend/Unravel/Knot (all of which need the full bead list) on
    /// every single cycle. This is NOT the same "silently treat as empty" v1
    /// bug the module doc warns about: that bug swallowed failures and
    /// returned nothing; this still surfaces every bad record via a loud
    /// warning, it just doesn't discard the records that DID parse.
    fn parse_beads(json: &str, context: &str) -> Result<Vec<Bead>> {
        let trimmed = json.trim();
        if trimmed.is_empty() {
            return Ok(vec![]);
        }
        // Try JSON array first (bf show returns [...])
        if trimmed.starts_with('[') {
            return serde_json::from_str::<Vec<Bead>>(trimmed)
                .with_context(|| format!("JSON parse error from {context}:\n{json}"));
        }
        // Fall back to NDJSON (bf list returns one object per line)
        let mut beads = Vec::new();
        for line in trimmed.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Bead>(line) {
                Ok(bead) => beads.push(bead),
                Err(e) => {
                    tracing::error!(
                        context = %context,
                        error = %e,
                        line = %line,
                        "NDJSON parse error on one record — skipping this bead, \
                         keeping the rest of the list intact"
                    );
                }
            }
        }
        Ok(beads)
    }

    /// Parse a single bead from a JSON array (first element).
    fn parse_single_bead(json: &str, context: &str) -> Result<Bead> {
        let beads = Self::parse_beads(json, context)?;
        beads
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("{context} returned empty array"))
    }
}

#[async_trait]
impl BeadStore for BfCliBeadStore {
    async fn list_all(&self) -> Result<Vec<Bead>> {
        // Use a large explicit limit instead of --limit 0, which returns
        // an empty set on bead-forge 0.2.0 (bug). 999999 effectively means "no limit".
        let stdout = self
            .run_bf(&["list", "--json", "--limit", "999999"])
            .await?;
        Self::parse_beads(&stdout, "bf list --json")
    }

    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
        // Use a large explicit limit instead of --limit 0, which returns
        // an empty set on bead-forge 0.2.0 (bug). 999999 effectively means "no limit".
        let mut args = vec!["list", "--json", "--status", "open", "--limit", "999999"];

        // Build filter args — stored so they live long enough for the slice.
        let assignee_arg;
        if let Some(ref assignee) = filters.assignee {
            args.push("--assignee");
            assignee_arg = assignee.clone();
            args.push(&assignee_arg);
        }

        let stdout = self.run_bf(&args).await?;
        let mut beads = Self::parse_beads(&stdout, "bf list --json")?;

        // Apply label exclusion filter (bf CLI doesn't support this natively).
        if !filters.exclude_labels.is_empty() {
            beads.retain(|b| !b.labels.iter().any(|l| filters.exclude_labels.contains(l)));
        }

        // Apply ID exclusion filter (in-memory filter).
        if !filters.exclude_ids.is_empty() {
            beads.retain(|b| !filters.exclude_ids.contains(&b.id));
        }

        Ok(beads)
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        let id_str = id.as_ref();
        let stdout = self
            .run_bf(&["show", id_str, "--json"])
            .await
            .with_context(|| format!("bf show {id_str} failed"))?;
        Self::parse_single_bead(&stdout, &format!("bf show {id_str} --json"))
    }

    async fn claim(&self, _id: &BeadId, actor: &str) -> Result<ClaimResult> {
        // BfCliBeadStore uses atomic claim_auto() for all claim operations.
        // This eliminates the race condition from the old br-style
        // "update + show verify" pattern — two workers racing to claim
        // the same bead will always receive distinct beads.
        self.claim_auto(actor).await
    }

    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
        // Build bf claim args. Velocity-aware scoring metadata is passed
        // BEFORE --assignee/--json; missing values are simply omitted.
        let mut args = vec!["claim"];
        if let Some(ref model) = self.model {
            args.push("--model");
            args.push(model.as_str());
        }
        if let Some(ref harness) = self.harness {
            args.push("--harness");
            args.push(harness.as_str());
        }
        if let Some(ref harness_version) = self.harness_version {
            args.push("--harness-version");
            args.push(harness_version.as_str());
        }
        args.push("--assignee");
        args.push(actor);
        args.push("--json");

        let stdout = self.run_bf(&args).await?;

        // Parse JSON output: {"bead_id": "...", "reclaimed": 0, "assignee": "..."}
        let json: serde_json::Value = serde_json::from_str(&stdout)
            .with_context(|| format!("bf claim returned invalid JSON: {stdout}"))?;

        if let Some(bead_id) = json.get("bead_id").and_then(|v| v.as_str()) {
            if bead_id.is_empty() || stdout.contains("No beads available") {
                return Ok(ClaimResult::NotClaimable {
                    reason: "no beads available".to_string(),
                });
            }
            // Fetch the full bead details
            let bead = self.show(&BeadId::from(bead_id)).await?;
            Ok(ClaimResult::Claimed(bead))
        } else {
            Ok(ClaimResult::NotClaimable {
                reason: "no beads available".to_string(),
            })
        }
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        // --assignee no longer exists on `bf update` in bf 0.4.1 (bf-1hmey);
        // `bf batch` (op=update) still accepts status+assignee together.
        let batch_json = serde_json::to_string(&serde_json::json!([{
            "op": "update",
            "id": id_str,
            "status": "open",
            "assignee": "",
        }]))
        .context("failed to serialize release batch payload")?;
        self.run_bf(&["batch", "--json", &batch_json])
            .await
            .with_context(|| format!("bf batch release {id_str} failed"))?;
        Ok(())
    }

    async fn block(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bf(&["update", id_str, "--status", "blocked"])
            .await
            .with_context(|| format!("bf block {id_str} failed"))?;
        Ok(())
    }

    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        // --assignee no longer exists on `bf update` in bf 0.4.1 (bf-1hmey);
        // `bf batch` (op=update) still accepts assignee alone.
        let batch_json = serde_json::to_string(&serde_json::json!([{
            "op": "update",
            "id": id_str,
            "assignee": "",
        }]))
        .context("failed to serialize clear_assignee batch payload")?;
        self.run_bf(&["batch", "--json", &batch_json])
            .await
            .with_context(|| format!("bf batch clear_assignee {id_str} failed"))?;
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        self.run_bf(&["sync", "--flush-only"])
            .await
            .context("bf sync --flush-only failed")?;
        Ok(())
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bf(&["reopen", id_str])
            .await
            .with_context(|| format!("bf reopen {id_str} failed"))?;
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        let bead = self.show(id).await?;
        Ok(bead.labels)
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bf(&["label", "add", id_str, "--label", label])
            .await
            .with_context(|| format!("bf label add {id_str} {label} failed"))?;
        Ok(())
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bf(&["label", "remove", id_str, "--label", label])
            .await
            .with_context(|| format!("bf label remove {id_str} {label} failed"))?;
        Ok(())
    }

    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        let mut args: Vec<String> = vec![
            "create".into(),
            "--title".into(),
            title.into(),
            "--description".into(),
            body.into(),
        ];
        for label in labels {
            args.push("--label".into());
            args.push((*label).into());
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let stdout = self.run_bf(&arg_refs).await?;
        let id_str = stdout.trim();
        if id_str.is_empty() {
            bail!("bf create returned empty ID");
        }
        Ok(BeadId::from(id_str))
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        let blocker = blocker_id.as_ref();
        let blocked = blocked_id.as_ref();
        self.run_bf(&["dep", "add", blocker, "--blocks", blocked])
            .await
            .with_context(|| format!("bf dep add {blocker} --blocks {blocked} failed"))?;
        Ok(())
    }

    async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()> {
        let blocked = blocked_id.as_ref();
        let blocker = blocker_id.as_ref();
        self.run_bf(&["dep", "remove", blocked, blocker])
            .await
            .with_context(|| format!("bf dep remove {blocked} {blocker} failed"))?;
        Ok(())
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        let stdout = self
            .run_bf(&["doctor", "--repair"])
            .await
            .context("bf doctor --repair failed")?;
        Ok(BrCliBeadStore::parse_doctor_output(&stdout))
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        let stdout = self.run_bf(&["doctor"]).await.context("bf doctor failed")?;
        Ok(BrCliBeadStore::parse_doctor_output(&stdout))
    }

    async fn full_rebuild(&self) -> Result<()> {
        let db_path = self.workspace.join(".beads/beads.db");

        if db_path.exists() {
            tokio::fs::remove_file(&db_path)
                .await
                .with_context(|| format!("failed to remove {}", db_path.display()))?;
            tracing::info!(path = %db_path.display(), "removed corrupt database file");
        }

        for suffix in &["-wal", "-shm"] {
            let wal_path = self.workspace.join(format!(".beads/beads.db{suffix}"));
            if wal_path.exists() {
                let _ = tokio::fs::remove_file(&wal_path).await;
            }
        }

        self.run_bf(&["sync", "--import-only"])
            .await
            .context("bf sync --import-only failed during full rebuild")?;

        let verify = self
            .run_bf(&["doctor"])
            .await
            .context("bf doctor verification failed after rebuild")?;
        let report = BrCliBeadStore::parse_doctor_output(&verify);

        if !report.warnings.is_empty() {
            bail!(
                "database still has issues after rebuild: {:?}",
                report.warnings
            );
        }

        tracing::info!("database fully rebuilt from JSONL — verified clean");
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        has_valid_bead_store(&self.workspace)
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use tokio::time::{Duration, Instant};

    #[test]
    fn filters_default_is_empty() {
        let f = Filters::default();
        assert!(f.assignee.is_none());
        assert!(f.exclude_labels.is_empty());
        assert!(f.exclude_ids.is_empty());
    }

    #[test]
    fn filters_with_exclude_ids_filters_beads() {
        use std::collections::HashSet;

        // Create a Filters with exclude_ids containing "bf-abc"
        let mut exclude_ids = HashSet::new();
        exclude_ids.insert(BeadId::from("bf-abc".to_string()));

        let filters = Filters {
            assignee: None,
            exclude_labels: vec![],
            exclude_ids,
        };

        // Verify the exclude_ids contains the expected bead ID
        assert!(filters
            .exclude_ids
            .contains(&BeadId::from("bf-abc".to_string())));
        assert_eq!(filters.exclude_ids.len(), 1);
    }

    #[test]
    fn parse_beads_empty_json_array() {
        let beads = BrCliBeadStore::parse_beads("[]", "test").unwrap();
        assert!(beads.is_empty());
    }

    #[test]
    fn parse_beads_empty_string_returns_empty() {
        let beads = BrCliBeadStore::parse_beads("", "test").unwrap();
        assert!(beads.is_empty());
    }

    fn minimal_bead_json(id: &str, status: &str) -> String {
        format!(
            r#"{{"id":"{id}","title":"Test bead","description":"desc","priority":2,"status":"{status}","assignee":null,"source_repo":"/home/coding/NEEDLE","dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}"#
        )
    }

    #[test]
    fn bf_parse_beads_accepts_completed_status() {
        // bf has been observed emitting "completed" for some done beads. A
        // single such record must not fail the whole list — see
        // needle-weave-completed-status.
        let json = minimal_bead_json("bf-1", "completed");
        let beads = BfCliBeadStore::parse_beads(&json, "test").unwrap();
        assert_eq!(beads.len(), 1);
        assert_eq!(beads[0].status, crate::types::BeadStatus::Done);
    }

    #[test]
    fn bf_parse_beads_skips_one_bad_line_keeps_the_rest() {
        // A genuinely unparseable record (unknown field type, corrupt line,
        // etc.) must not take down every other bead in the same `bf list
        // --json` call — that was the root cause of Weave/Mend/Unravel/Knot
        // silently erroring on every cycle for any workspace with one such
        // record. The bad line is skipped and loudly logged, not silently
        // dropped from view entirely (that would repeat the v1 "silent
        // empty" bug this module's doc comment warns about).
        let good_one = minimal_bead_json("bf-1", "open");
        let good_two = minimal_bead_json("bf-2", "closed");
        let bad = r#"{"id":"bf-bad","status":"open" this is not valid json"#;
        let json = format!("{good_one}\n{bad}\n{good_two}");
        let beads = BfCliBeadStore::parse_beads(&json, "test").unwrap();
        let ids: Vec<_> = beads.iter().map(|b| b.id.to_string()).collect();
        assert_eq!(ids, vec!["bf-1", "bf-2"]);
    }

    #[test]
    fn parse_beads_malformed_json_is_error() {
        let result = BrCliBeadStore::parse_beads("{ not json", "test");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("JSON parse error"));
    }

    #[test]
    fn parse_single_bead_empty_array_is_error() {
        let result = BrCliBeadStore::parse_single_bead("[]", "test");
        assert!(result.is_err());
    }

    #[test]
    fn repair_report_parses_warn_and_fixed_lines() {
        let report = BrCliBeadStore::parse_doctor_output(
            "WARN some-warning\nFIXED repaired-item\nOK normal-line\n",
        );
        assert_eq!(report.warnings, vec!["some-warning"]);
        assert_eq!(report.fixed, vec!["repaired-item"]);
    }

    // ── Corruption detection tests ──────────────────────────────────────────

    #[test]
    fn detects_malformed_db_error() {
        assert!(is_corruption_error(
            "Error: database disk image is malformed"
        ));
    }

    #[test]
    fn detects_locked_db_error() {
        assert!(is_lock_error("database is locked"));
        // Also verify that lock errors are NOT corruption errors (they're transient)
        assert!(!is_corruption_error("database is locked"));
    }

    #[test]
    fn detects_not_a_database_error() {
        assert!(is_corruption_error("file is not a database"));
    }

    #[test]
    fn detects_case_insensitive() {
        assert!(is_corruption_error(
            "ERROR: Database Disk Image Is Malformed"
        ));
    }

    #[test]
    fn non_corruption_error_returns_false() {
        assert!(!is_corruption_error("bead not found"));
        assert!(!is_corruption_error("connection refused"));
        assert!(!is_corruption_error(""));
    }

    #[test]
    fn corruption_in_longer_message() {
        let msg = "br [\"list\"] exited with code 1\nstderr: Error: database disk image is malformed\nstdout: ";
        assert!(is_corruption_error(msg));
    }

    // ── parse_doctor_output tests ───────────────────────────────────────────

    #[test]
    fn parse_doctor_output_empty() {
        let report = BrCliBeadStore::parse_doctor_output("");
        assert!(report.warnings.is_empty());
        assert!(report.fixed.is_empty());
    }

    #[test]
    fn parse_doctor_output_multiple_entries() {
        let report = BrCliBeadStore::parse_doctor_output(
            "WARN index missing\nWARN stale ref\nFIXED rebuilt index\nOK\n",
        );
        assert_eq!(report.warnings.len(), 2);
        assert_eq!(report.fixed.len(), 1);
    }

    #[test]
    fn parse_doctor_output_filters_sqlite3_not_available() {
        let report = BrCliBeadStore::parse_doctor_output(
            "WARN sqlite3 not available for integrity check\nWARN real issue\nFIXED something\n",
        );
        assert_eq!(
            report.warnings,
            vec!["real issue"],
            "sqlite3 not available should be filtered out"
        );
        assert_eq!(report.fixed, vec!["something"]);
    }

    #[test]
    fn parse_doctor_output_filters_recovery_artifacts() {
        let report = BrCliBeadStore::parse_doctor_output(
            "WARN db.recovery_artifacts: Preserved recovery artifacts remain for this database family (1 item(s))\nWARN real issue\n",
        );
        assert_eq!(
            report.warnings,
            vec!["real issue"],
            "recovery_artifacts should be filtered out"
        );
    }

    // ── Sync conflict detection tests ─────────────────────────────────────

    #[test]
    fn sync_conflict_detects_sync_conflict_marker() {
        assert!(is_sync_conflict("Error: SYNC_CONFLICT detected"));
    }

    #[test]
    fn sync_conflict_detects_jsonl_is_newer() {
        assert!(is_sync_conflict("JSONL is newer than database"));
    }

    #[test]
    fn sync_conflict_detects_lowercase_marker() {
        assert!(is_sync_conflict("sync conflict on update"));
    }

    #[test]
    fn sync_conflict_in_longer_stderr() {
        let msg = "br [\"update\"] exited with code 6\nstderr: SYNC_CONFLICT\nstdout: ";
        assert!(is_sync_conflict(msg));
    }

    #[test]
    fn sync_conflict_returns_false_for_non_conflict() {
        assert!(!is_sync_conflict("bead not found"));
        assert!(!is_sync_conflict("database disk image is malformed"));
        assert!(!is_sync_conflict(""));
    }

    #[test]
    fn sync_conflict_is_case_sensitive() {
        // SYNC_CONFLICT is an exact marker, case matters
        assert!(!is_sync_conflict("sync_conflict"));
        assert!(is_sync_conflict("SYNC_CONFLICT"));
    }

    // ── has_valid_bead_store tests ─────────────────────────────────────────

    #[test]
    fn has_valid_bead_store_returns_false_for_nonexistent_directory() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        assert!(!has_valid_bead_store(workspace));
    }

    #[test]
    fn has_valid_bead_store_returns_true_for_beads_directory() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();
        assert!(has_valid_bead_store(workspace));
    }

    #[test]
    fn has_valid_bead_store_returns_false_for_file_instead_of_directory() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::write(workspace.join(".beads"), "not a directory").unwrap();
        assert!(!has_valid_bead_store(workspace));
    }

    // ── parse_beads edge case tests ───────────────────────────────────────

    #[test]
    fn parse_beads_whitespace_only_returns_empty() {
        let beads = BrCliBeadStore::parse_beads("   \n\t  ", "test").unwrap();
        assert!(beads.is_empty());
    }

    // ── version handshake tests ────────────────────────────────────────────

    #[tokio::test]
    async fn version_check_known_bad_0_2_0() {
        // Create a temporary script that mimics bead-forge 0.2.0
        let tmp_dir = tempfile::tempdir().unwrap();
        let fake_bf = tmp_dir.path().join("fake-bf-0.2.0");
        std::fs::write(
            &fake_bf,
            r#"#!/bin/sh
echo "bf 0.2.0"
"#,
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_bf,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let result = check_bead_forge_version(&fake_bf).await;
        match result {
            VersionCheck::KnownBad { version, issues } => {
                assert_eq!(version, "bf 0.2.0");
                assert!(issues
                    .iter()
                    .any(|i| i.contains("--limit 0 returns empty set")));
            }
            other => panic!("Expected KnownBad, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn version_check_known_bad_0_1_x() {
        // Test detection of 0.1.x versions
        let tmp_dir = tempfile::tempdir().unwrap();
        let fake_bf = tmp_dir.path().join("fake-bf-0.1.9");
        std::fs::write(
            &fake_bf,
            r#"#!/bin/sh
echo "bf 0.1.9"
"#,
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_bf,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let result = check_bead_forge_version(&fake_bf).await;
        match result {
            VersionCheck::KnownBad { version, issues } => {
                assert_eq!(version, "bf 0.1.9");
                assert!(issues.iter().any(|i| i.contains("pre-0.2.0 versions")));
            }
            other => panic!("Expected KnownBad, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn version_check_ok_for_newer_versions() {
        // Test that newer versions (e.g., 0.3.0) are not flagged as bad
        let tmp_dir = tempfile::tempdir().unwrap();
        let fake_bf = tmp_dir.path().join("fake-bf-0.3.0");
        std::fs::write(
            &fake_bf,
            r#"#!/bin/sh
echo "bf 0.3.0"
"#,
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_bf,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let result = check_bead_forge_version(&fake_bf).await;
        match result {
            VersionCheck::Ok => {
                // Expected result for unknown/good versions
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn version_check_failed_for_missing_binary() {
        let fake_bf = PathBuf::from("/nonexistent/bf-binary-xyz");
        let result = check_bead_forge_version(&fake_bf).await;
        match result {
            VersionCheck::Failed { reason } => {
                assert!(reason.contains("failed to spawn") || reason.contains("No such file"));
            }
            other => panic!("Expected Failed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn version_check_failed_for_empty_output() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let fake_bf = tmp_dir.path().join("fake-bf-empty");
        std::fs::write(
            &fake_bf,
            r#"#!/bin/sh
# Output nothing
"#,
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_bf,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let result = check_bead_forge_version(&fake_bf).await;
        match result {
            VersionCheck::Failed { reason } => {
                assert!(reason.contains("no output"));
            }
            other => panic!("Expected Failed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn version_check_handles_various_output_formats() {
        // Test that the parser handles different version output formats
        let tmp_dir = tempfile::tempdir().unwrap();
        let fake_bf = tmp_dir.path().join("fake-bf-various");
        std::fs::write(
            &fake_bf,
            r#"#!/bin/sh
echo "bf 0.2.0-github"
"#,
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_bf,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let result = check_bead_forge_version(&fake_bf).await;
        match result {
            VersionCheck::KnownBad { version, issues } => {
                assert_eq!(version, "bf 0.2.0-github");
                assert!(issues.iter().any(|i| i.contains("--limit 0")));
            }
            other => panic!("Expected KnownBad, got {:?}", other),
        }
    }

    // ── CLI arg verification tests ────────────────────────────────────────────

    #[tokio::test]
    async fn br_cli_bead_store_ready_passes_explicit_limit() {
        // Verify that ready() passes an explicit limit of 10000
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Use test-specific args file in temp dir to avoid race conditions
        let args_file = tmp_dir.path().join("br-ready-args.txt");

        // Create a fake br that logs its arguments
        let fake_br = tmp_dir.path().join("fake-br-ready-limit");
        std::fs::write(
            &fake_br,
            format!(
                r#"#!/bin/sh
echo "$@" > {}
echo '[]'
"#,
                args_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_br,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BrCliBeadStore::new(
            fake_br.clone(),
            workspace.to_path_buf(),
            None, // model
            None, // harness
            None, // harness_version
        )
        .unwrap();
        let filters = Filters::default();

        let _ = store.ready(&filters).await;

        // Read back the arguments that were passed
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(args.contains("--limit"), "ready() must pass --limit flag");
        assert!(args.contains("10000"), "ready() must pass limit of 10000");

        // Cleanup handled by tmp_dir drop
    }

    #[tokio::test]
    async fn br_cli_bead_store_list_all_passes_large_explicit_limit() {
        // Verify that list_all() passes an explicit limit of 999999 (not 0)
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Use test-specific args file in temp dir to avoid race conditions
        let args_file = tmp_dir.path().join("br-list-args.txt");

        // Create a fake br that logs its arguments
        let fake_br = tmp_dir.path().join("fake-br-list-limit");
        std::fs::write(
            &fake_br,
            format!(
                r#"#!/bin/sh
echo "$@" > {}
echo '[]'
"#,
                args_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_br,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BrCliBeadStore::new(
            fake_br.clone(),
            workspace.to_path_buf(),
            None, // model
            None, // harness
            None, // harness_version
        )
        .unwrap();

        let _ = store.list_all().await;

        // Read back the arguments that were passed
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(
            args.contains("--limit"),
            "list_all() must pass --limit flag"
        );
        assert!(
            args.contains("999999"),
            "list_all() must pass limit of 999999"
        );
        assert!(
            !args.contains("--limit 0"),
            "list_all() must NOT pass limit of 0"
        );

        // Cleanup handled by tmp_dir drop
    }

    #[tokio::test]
    async fn bf_cli_bead_store_ready_passes_explicit_limit() {
        // Verify that BfCliBeadStore ready() passes an explicit limit of 999999
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Use test-specific args file in temp dir to avoid race conditions
        let args_file = tmp_dir.path().join("bf-ready-args.txt");

        // Create a fake bf that logs its arguments
        let fake_bf = tmp_dir.path().join("fake-bf-ready-limit");
        std::fs::write(
            &fake_bf,
            format!(
                r#"#!/bin/sh
echo "$@" > {}
echo '[]'
"#,
                args_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_bf,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BfCliBeadStore::new(fake_bf.clone(), workspace.to_path_buf(), None, None, None)
            .unwrap();
        let filters = Filters::default();

        let _ = store.ready(&filters).await;

        // Read back the arguments that were passed
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(
            args.contains("--limit"),
            "bf ready() must pass --limit flag"
        );
        assert!(
            args.contains("999999"),
            "bf ready() must pass limit of 999999"
        );

        // Cleanup handled by tmp_dir drop
    }

    #[tokio::test]
    async fn br_cli_bead_store_ready_filters_by_exclude_ids() {
        use std::collections::HashSet;

        // Test that ready() filters out beads whose IDs are in exclude_ids
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Create a fake br that returns multiple beads
        let fake_br = tmp_dir.path().join("fake-br-ready-exclude");
        std::fs::write(
            &fake_br,
            r#"#!/bin/sh
echo '[{"id":"bf-abc","title":"Test bead ABC","description":"desc","priority":2,"status":"open","assignee":null,"source_repo":"/home/coding/NEEDLE","dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"},{"id":"bf-def","title":"Test bead DEF","description":"desc","priority":2,"status":"open","assignee":null,"source_repo":"/home/coding/NEEDLE","dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}]'
"#,
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_br,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BrCliBeadStore::new(fake_br.clone(), workspace.to_path_buf(), None, None, None)
            .unwrap();

        // Test 1: No exclude_ids filtering - both beads returned
        let filters = Filters::default();
        let beads = store.ready(&filters).await.unwrap();
        assert_eq!(
            beads.len(),
            2,
            "should return both beads when no exclude_ids"
        );

        // Test 2: Exclude one bead by ID
        let mut exclude_ids = HashSet::new();
        exclude_ids.insert(BeadId::from("bf-abc".to_string()));

        let filters_with_exclude = Filters {
            assignee: None,
            exclude_labels: vec![],
            exclude_ids,
        };

        let filtered_beads = store.ready(&filters_with_exclude).await.unwrap();
        assert_eq!(
            filtered_beads.len(),
            1,
            "should return only one bead after exclude_ids filtering"
        );
        assert_eq!(
            filtered_beads[0].id.as_ref(),
            "bf-def",
            "remaining bead should be bf-def"
        );
        assert!(
            !filtered_beads.iter().any(|b| b.id.as_ref() == "bf-abc"),
            "bf-abc should be excluded"
        );
    }

    #[tokio::test]
    async fn bf_cli_bead_store_ready_filters_by_exclude_ids() {
        use std::collections::HashSet;

        // Test that ready() filters out beads whose IDs are in exclude_ids
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Create a fake bf that returns multiple beads
        let fake_bf = tmp_dir.path().join("fake-bf-ready-exclude");
        std::fs::write(
            &fake_bf,
            r#"#!/bin/sh
echo '[{"id":"bf-abc","title":"Test bead ABC","description":"desc","priority":2,"status":"open","assignee":null,"source_repo":"/home/coding/NEEDLE","dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"},{"id":"bf-def","title":"Test bead DEF","description":"desc","priority":2,"status":"open","assignee":null,"source_repo":"/home/coding/NEEDLE","dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}]'
"#,
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_bf,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BfCliBeadStore::new(fake_bf.clone(), workspace.to_path_buf(), None, None, None)
            .unwrap();

        // Test 1: No exclude_ids filtering - both beads returned
        let filters = Filters::default();
        let beads = store.ready(&filters).await.unwrap();
        assert_eq!(
            beads.len(),
            2,
            "should return both beads when no exclude_ids"
        );

        // Test 2: Exclude one bead by ID
        let mut exclude_ids = HashSet::new();
        exclude_ids.insert(BeadId::from("bf-abc".to_string()));

        let filters_with_exclude = Filters {
            assignee: None,
            exclude_labels: vec![],
            exclude_ids,
        };

        let filtered_beads = store.ready(&filters_with_exclude).await.unwrap();
        assert_eq!(
            filtered_beads.len(),
            1,
            "should return only one bead after exclude_ids filtering"
        );
        assert_eq!(
            filtered_beads[0].id.as_ref(),
            "bf-def",
            "remaining bead should be bf-def"
        );
        assert!(
            !filtered_beads.iter().any(|b| b.id.as_ref() == "bf-abc"),
            "bf-abc should be excluded"
        );
    }

    #[tokio::test]
    async fn bf_cli_bead_store_list_all_passes_explicit_limit() {
        // Verify that BfCliBeadStore list_all() passes an explicit limit of 999999
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Use test-specific args file in temp dir to avoid race conditions
        let args_file = tmp_dir.path().join("bf-list-args.txt");

        // Create a fake bf that logs its arguments
        let fake_bf = tmp_dir.path().join("fake-bf-list-limit");
        std::fs::write(
            &fake_bf,
            format!(
                r#"#!/bin/sh
echo "$@" > {}
echo '[]'
"#,
                args_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_bf,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BfCliBeadStore::new(fake_bf.clone(), workspace.to_path_buf(), None, None, None)
            .unwrap();

        let _ = store.list_all().await;

        // Read back the arguments that were passed
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(
            args.contains("--limit"),
            "bf list_all() must pass --limit flag"
        );
        assert!(
            args.contains("999999"),
            "bf list_all() must pass limit of 999999"
        );
        assert!(
            !args.contains("--limit 0"),
            "bf list_all() must NOT pass limit of 0"
        );

        // Cleanup handled by tmp_dir drop
    }

    // ─── ETXTBSY retry tests ───────────────────────────────────────────────────────

    /// Helper function to create an ETXTBSY error (errno 26 on Unix).
    fn make_etxtbsy_error() -> io::Error {
        io::Error::from_raw_os_error(26)
    }

    /// Helper function to create a non-ETXTBSY error.
    fn make_other_error() -> io::Error {
        io::Error::new(io::ErrorKind::NotFound, "not found")
    }

    #[tokio::test]
    async fn etxtbsy_retry_linear_succeeds_on_first_attempt() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result = spawn_with_etxtbsy_retry(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            5,
            20,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn etxtbsy_retry_linear_retries_on_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        // Fail first 2 attempts with ETXTBSY
                        Err::<_, io::Error>(make_etxtbsy_error())
                    } else {
                        Ok::<_, io::Error>(b"success".to_vec())
                    }
                }
            },
            5,
            20,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 3); // 2 failures + 1 success
    }

    #[tokio::test]
    async fn etxtbsy_retry_linear_exhausts_attempts() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Always fail with ETXTBSY
                    Err::<_, io::Error>(make_etxtbsy_error())
                }
            },
            3,
            20,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 3); // max attempts
    }

    #[tokio::test]
    async fn etxtbsy_retry_linear_fails_fast_on_non_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Fail with non-ETXTBSY error (should not retry)
                    Err::<_, io::Error>(make_other_error())
                }
            },
            5,
            20,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(call_count.load(Ordering::SeqCst), 1); // Should only be called once
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_succeeds_on_first_attempt() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result = spawn_with_etxtbsy_retry_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            10,
            20,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_retries_on_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt < 3 {
                        // Fail first 3 attempts with ETXTBSY
                        Err::<_, io::Error>(make_etxtbsy_error())
                    } else {
                        Ok::<_, io::Error>(b"success".to_vec())
                    }
                }
            },
            10,
            20,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 4); // 3 failures + 1 success
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_exhausts_attempts() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Always fail with ETXTBSY
                    Err::<_, io::Error>(make_etxtbsy_error())
                }
            },
            5,
            20,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 5); // max attempts
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_fails_fast_on_non_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Fail with non-ETXTBSY error (should not retry)
                    Err::<_, io::Error>(make_other_error())
                }
            },
            10,
            20,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(call_count.load(Ordering::SeqCst), 1); // Should only be called once
    }

    #[tokio::test]
    async fn etxtbsy_retry_linear_timing() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result = spawn_with_etxtbsy_retry(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        Err::<_, io::Error>(make_etxtbsy_error())
                    } else {
                        Ok::<_, io::Error>(b"success".to_vec())
                    }
                }
            },
            5,
            50,
        )
        .await;

        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 3); // 2 failures + 1 success

        // With 2 retries at 50ms each, should take ~100ms
        assert!(elapsed >= Duration::from_millis(90));
        assert!(elapsed < Duration::from_millis(200)); // Upper bound with tolerance
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_timing() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result = spawn_with_etxtbsy_retry_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt < 3 {
                        Err::<_, io::Error>(make_etxtbsy_error())
                    } else {
                        Ok::<_, io::Error>(b"success".to_vec())
                    }
                }
            },
            10,
            20,
        )
        .await;

        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 4); // 3 failures + 1 success

        // Exponential backoff: 20ms + 40ms + 80ms = ~140ms with jitter
        assert!(elapsed >= Duration::from_millis(100));
        assert!(elapsed < Duration::from_millis(300)); // Upper bound with jitter tolerance
    }

    #[tokio::test]
    async fn etxtbsy_retry_child_wrapper_works() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        // Create a mock child process result
        let result = spawn_with_etxtbsy_retry_child(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Return a mock child-like result (we just test the wrapper passes through)
                    Err::<_, io::Error>(make_etxtbsy_error())
                }
            },
            3,
            20,
        )
        .await;

        // Should fail after exhausting attempts
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_child_wrapper_works() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result = spawn_with_etxtbsy_retry_exponential_child(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Err::<_, io::Error>(make_etxtbsy_error())
                }
            },
            5,
            20,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 5);
    }
}
